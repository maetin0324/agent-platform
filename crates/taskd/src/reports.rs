//! 報告の圧縮（ADR-0033 D3。Phase 25。監査による修正は ADR-0034 D2/D7）。
//!
//! 親ノードにたまった子の報告を、**レビュー用の run 1 回**で 1 件にまとめる。判断（何件たまったか、
//! 最古が何時間経ったか）は決定的で、まとめる中身だけが LLM の仕事（`Check::Reviewer` と同じ「別 run」の形）。
//!
//! ここは `tick_loop` の中から同期で呼ばれる（B1: チャネルに送らない）。やることは
//! 「`reports` を読む」「まとめのタスクを `tasks` に 1 件作る」だけで、LLM もワーカーも起動しない。
//! 起こした run が `done` になると、ディスパッチャ（`task-dispatch` の `reports` モジュール）が
//! その `summary` を親ノードの報告にし、`sources` に子の報告を入れる。
//!
//! 監査 H-2: 同じノード・同じ案件のまとめタスクが直近失敗していれば、`compress_after_secs` が
//! 経つまで次のまとめを作らない(バックオフ)。子の報告は `sources` に入るまで「レビュー待ち」のままなので、
//! まとめが失敗し続けても tick ごとに新しいタスクが積み上がることはない。

use serde::Deserialize;
use task_core::report::{self, Report};
use task_core::{
    Budget, GenreSpec, ListFilter, ListOrder, OrgNode, ProjectId, RoleSpec, Status, StoreError, TaskId,
    TaskKind, TaskStore,
};
use task_ops::add::NewTaskSpec;
use time::OffsetDateTime;

/// まとめの run の予算（短い読み書きだけなので小さく取る）。
const COMPACTION_BUDGET: Budget = Budget {
    max_turns: 8,
    max_wall_secs: 600,
    max_retries: 1,
};

/// 開いているまとめタスクを探すときに見る件数の上限。
const OPEN_TASK_SCAN: usize = 500;

/// `[reports]`（ADR-0033 D3）。閾値だけを持つ。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReportsConfig {
    /// 子の報告がこの件数たまったら、まとめの run を 1 回起こす。
    #[serde(default = "default_compress_after")]
    pub compress_after: usize,
    /// 件数に満たなくても、最古の報告がこの秒数を過ぎたら起こす（既定 2 時間）。
    #[serde(default = "default_compress_after_secs")]
    pub compress_after_secs: u64,
}

fn default_compress_after() -> usize {
    report::DEFAULT_COMPRESS_AFTER
}

fn default_compress_after_secs() -> u64 {
    report::DEFAULT_COMPRESS_AFTER_SECS
}

impl Default for ReportsConfig {
    fn default() -> Self {
        Self {
            compress_after: default_compress_after(),
            compress_after_secs: default_compress_after_secs(),
        }
    }
}

/// 親になっているノード（子を 1 つ以上持つノード）。
fn parents(org: &[OrgNode]) -> Vec<&OrgNode> {
    org.iter()
        .filter(|n| org.iter().any(|c| c.parent_id.as_deref() == Some(n.id.as_str())))
        .collect()
}

/// 案件ごとに分ける（`None` = 案件なし）。並びは入力（古い順）のまま。
fn group_by_project(pending: Vec<Report>) -> Vec<(Option<ProjectId>, Vec<Report>)> {
    let mut groups: Vec<(Option<ProjectId>, Vec<Report>)> = Vec::new();
    for report in pending {
        match groups.iter_mut().find(|(p, _)| *p == report.project_id) {
            Some((_, items)) => items.push(report),
            None => groups.push((report.project_id, vec![report])),
        }
    }
    groups
}

/// そのノード・その案件のまとめタスクが既に開いている（終端でない）か。
/// 同じ報告を二重にまとめないための決定的な検査。
fn has_open_compaction_task(
    store: &dyn TaskStore,
    node_id: &str,
    project_id: Option<ProjectId>,
) -> Result<bool, StoreError> {
    let filter = ListFilter {
        statuses: vec![
            Status::Draft,
            Status::Ready,
            Status::Running,
            Status::Blocked,
            Status::Reviewing,
        ],
        project_id,
        ..ListFilter::default()
    };
    let page = store.list_page(&filter, ListOrder::UpdatedDesc, None, OPEN_TASK_SCAN)?;
    Ok(page.items.iter().any(|t| {
        t.role.as_deref() == Some(report::COMPACTION_ROLE)
            && t.assignee.as_deref() == Some(node_id)
            && t.project_id == project_id
    }))
}

/// 監査 H-2: 同じノード・同じ案件のまとめタスクが直近 `within_secs` 以内に `Failed` / `Cancelled` で
/// 終わっていれば、次のまとめを作らない(バックオフ)。まとめの run が失敗し続けても、子の報告がたまる
/// たびに無限にタスクが作られるのを防ぐ。
fn has_recently_failed_compaction_task(
    store: &dyn TaskStore,
    node_id: &str,
    project_id: Option<ProjectId>,
    now: OffsetDateTime,
    within_secs: u64,
) -> Result<bool, StoreError> {
    let filter = ListFilter {
        statuses: vec![Status::Failed, Status::Cancelled],
        project_id,
        ..ListFilter::default()
    };
    let page = store.list_page(&filter, ListOrder::UpdatedDesc, None, OPEN_TASK_SCAN)?;
    let within = i64::try_from(within_secs).unwrap_or(i64::MAX);
    Ok(page.items.iter().any(|t| {
        t.role.as_deref() == Some(report::COMPACTION_ROLE)
            && t.assignee.as_deref() == Some(node_id)
            && t.project_id == project_id
            && (now - t.updated_at).whole_seconds() < within
    }))
}

/// まとめの run のタスクの指定（`kind = execute`、担当は親ノード）。
/// 受け入れ条件は置かない: 出力は「1 件の報告」そのもので、決定的に確かめられるものが無いため
/// （レビューは条件ゼロで全 pass = `done`。報告自体は run の終端の時点で作られる）。
///
/// 監査 L-4（Phase 27）: tier / アダプタ / 分野は自分で決めず、`task_ops::add::create_support_task` の
/// 解決順（タスクの値 > 役割の既定 > `assignee` 由来 > 分野の既定 > 全体の既定）に任せる。
/// `role = report-compressor` が `[[roles]]` に無い構成でも落ちない（既定が埋まらないだけ）。
fn compaction_spec(node: &OrgNode, project_id: Option<ProjectId>, pending: &[Report]) -> NewTaskSpec {
    NewTaskSpec { repos: Vec::new(),
        title: format!("報告のまとめ: {}", node.name),
        objective: report::compaction_objective(node, pending),
        acceptance: Vec::new(),
        kind: TaskKind::Execute,
        tier: None,
        priority: 0,
        parent: None,
        depends_on: Vec::new(),
        max_turns: Some(COMPACTION_BUDGET.max_turns),
        max_wall_secs: Some(COMPACTION_BUDGET.max_wall_secs),
        max_retries: COMPACTION_BUDGET.max_retries,
        role: Some(report::COMPACTION_ROLE.to_string()),
        genre: None,
        aggregate: false,
        project_id,
        milestone_id: None,
        assignee: Some(node.id.clone()),
        // 作業ディレクトリは `workspace_root/<task_id>`（相対パスをディスパッチャが解決する）。
        workspace: None,
        cluster: None,
        adapter: None,
    }
}

/// tick ごとに 1 回呼ぶ。閾値を超えた親ノード × 案件ごとに、まとめのタスクを 1 件ずつ作る。
/// 作ったタスクの id を返す（テストとログのため）。**LLM は呼ばない**。
pub fn schedule_report_compaction(
    store: &dyn TaskStore,
    config: &ReportsConfig,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Result<Vec<TaskId>, StoreError> {
    let org = store.org_list()?;
    let mut created = Vec::new();
    for node in parents(&org) {
        let pending = store.report_unreviewed_children(&node.id)?;
        if pending.is_empty() {
            continue;
        }
        for (project_id, items) in group_by_project(pending) {
            if !report::compaction_due(&items, now, config.compress_after, config.compress_after_secs) {
                continue;
            }
            if has_open_compaction_task(store, &node.id, project_id)? {
                continue;
            }
            // 監査 H-2: 直近で失敗したまとめタスクがあれば、`compress_after_secs` 経つまでバックオフする。
            if has_recently_failed_compaction_task(store, &node.id, project_id, now, config.compress_after_secs)? {
                tracing::debug!(node = %node.id, ?project_id, "reports: backing off after a recently failed compaction run");
                continue;
            }
            let spec = compaction_spec(node, project_id, &items);
            // 作成経路の検証（案件が消えている等）で落ちたら、その組だけ諦めて次へ進む
            // （1 つの案件の不整合で他のノードのまとめまで止めない）。
            let task = match task_ops::add::create_support_task(store, spec, roles, genres, now) {
                Ok(task) => task,
                Err(e) => {
                    tracing::warn!(node = %node.id, ?project_id, error = %e, "reports: could not create the compaction task");
                    continue;
                }
            };
            tracing::info!(
                node = %node.id,
                task_id = %task.id,
                reports = items.len(),
                "reports: scheduled a compaction run"
            );
            created.push(task.id);
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::org::OrgKind;
    use task_core::report::{ReportKind, ReportStore};
    use task_core::{Report, ReportFilter, ReportId, SqliteStore};

    fn org_node(id: &str, parent: Option<&str>, kind: OrgKind) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            name: id.into(),
            kind,
            genre: None,
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn store_with_org() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("open");
        for n in [
            org_node("secretary", None, OrgKind::Secretary),
            org_node("coding", Some("secretary"), OrgKind::Department),
            org_node("coding-poc", Some("coding"), OrgKind::Section),
        ] {
            store.org_upsert(&n).expect("seed");
        }
        store
    }

    /// 案件を 1 件作って id を返す（まとめタスクの作成経路が案件の実在を検証するため）。
    fn seed_project(store: &SqliteStore) -> ProjectId {
        let now = OffsetDateTime::now_utc();
        let project = task_core::Project {
            id: ProjectId::new(),
            title: "案件".into(),
            request: "依頼".into(),
            status: task_core::ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: now,
            updated_at: now,
        };
        store.project_create(&project).expect("project");
        project.id
    }

    fn child_report(project: Option<ProjectId>, at: OffsetDateTime, n: usize) -> Report {
        Report {
            id: ReportId::new(),
            project_id: project,
            node_id: "coding-poc".into(),
            task_id: None,
            kind: ReportKind::Result,
            level: 2,
            headline: format!("結果 {n}"),
            body: format!("本文 {n}"),
            sources: Vec::new(),
            read_at: None,
            created_at: at,
        }
    }

    #[test]
    fn four_reports_schedule_a_run_but_three_do_not() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let project = seed_project(&store);
        let cfg = ReportsConfig::default();

        for n in 0..3 {
            store.report_append(&child_report(Some(project), now, n)).expect("append");
        }
        assert!(
            schedule_report_compaction(&store, &cfg, &[], &[], now)
                .expect("schedule")
                .is_empty(),
            "3 件では起きない"
        );

        store.report_append(&child_report(Some(project), now, 3)).expect("append");
        let created = schedule_report_compaction(&store, &cfg, &[], &[], now).expect("schedule");
        assert_eq!(created.len(), 1, "4 件で起きる");

        let task = store.get(created[0]).expect("get").expect("some");
        assert_eq!(task.kind, TaskKind::Execute);
        assert_eq!(task.assignee.as_deref(), Some("coding"));
        assert_eq!(task.project_id, Some(project));
        assert_eq!(task.role.as_deref(), Some(report::COMPACTION_ROLE));
        for n in 0..4 {
            assert!(task.objective.contains(&format!("結果 {n}")), "{}", task.objective);
            assert!(task.objective.contains(&format!("本文 {n}")), "{}", task.objective);
        }

        // 開いているまとめがある間は二重に作らない。
        assert!(
            schedule_report_compaction(&store, &cfg, &[], &[], now)
                .expect("schedule")
                .is_empty()
        );
    }

    #[test]
    fn an_old_report_schedules_a_run_even_below_the_count() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let cfg = ReportsConfig::default();
        store
            .report_append(&child_report(Some(seed_project(&store)), now - time::Duration::hours(1), 0))
            .expect("append");
        assert!(
            schedule_report_compaction(&store, &cfg, &[], &[], now)
                .expect("schedule")
                .is_empty(),
            "1 時間では起きない"
        );
        let created = schedule_report_compaction(&store, &cfg, &[], &[], now + time::Duration::hours(2)).expect("schedule");
        assert_eq!(created.len(), 1, "2 時間経過で起きる");
    }

    /// 監査 H-2: まとめ run が失敗し続けても、`compress_after_secs` が経つまでは次のまとめを作らない。
    #[test]
    fn a_recently_failed_compaction_task_backs_off_until_compress_after_secs_passes() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let project = seed_project(&store);
        let cfg = ReportsConfig::default();

        for n in 0..4 {
            store.report_append(&child_report(Some(project), now, n)).expect("append");
        }

        // 同じノード・同じ案件の、直近失敗したまとめタスク。
        let node = OrgNode {
            id: "coding".into(),
            parent_id: Some("secretary".into()),
            name: "coding".into(),
            kind: OrgKind::Department,
            genre: None,
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        };
        let failed = task_ops::add::create_support_task(
            &store,
            compaction_spec(&node, Some(project), &[]),
            &[],
            &[],
            now - time::Duration::minutes(10),
        )
        .expect("support task");
        store
            .acquire_lease(failed.id, "run-failed", std::time::Duration::from_secs(60))
            .expect("lease");
        let outcome = store
            .apply_transition(failed.id, task_core::Trigger::WorkerError { retryable: false }, None)
            .expect("fail");
        assert_eq!(outcome.next, Status::Failed);

        assert!(
            schedule_report_compaction(&store, &cfg, &[], &[], now)
                .expect("schedule")
                .is_empty(),
            "失敗直後はバックオフされ、次のまとめを作らない"
        );

        // `compress_after_secs` が経てば作られる。
        let later = now + time::Duration::seconds(cfg.compress_after_secs as i64) + time::Duration::minutes(11);
        let created = schedule_report_compaction(&store, &cfg, &[], &[], later).expect("schedule");
        assert_eq!(created.len(), 1, "バックオフ期間を過ぎればまた作られる");
    }

    #[test]
    fn reports_of_different_projects_get_one_run_each() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let cfg = ReportsConfig::default();
        let a = seed_project(&store);
        let b = seed_project(&store);
        for n in 0..4 {
            store.report_append(&child_report(Some(a), now, n)).expect("append");
            store.report_append(&child_report(Some(b), now, n)).expect("append");
        }
        let created = schedule_report_compaction(&store, &cfg, &[], &[], now).expect("schedule");
        assert_eq!(created.len(), 2);
        let projects: Vec<_> = created
            .iter()
            .filter_map(|id| store.get(*id).ok().flatten())
            .filter_map(|t| t.project_id)
            .collect();
        assert!(projects.contains(&a) && projects.contains(&b));
    }

    #[test]
    fn once_the_summary_exists_the_children_are_no_longer_pending() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let project = seed_project(&store);
        let cfg = ReportsConfig::default();
        let children: Vec<Report> = (0..4).map(|n| child_report(Some(project), now, n)).collect();
        store.report_append_all(&children).expect("append");
        let created = schedule_report_compaction(&store, &cfg, &[], &[], now).expect("schedule");
        assert_eq!(created.len(), 1);

        // まとめの run が done になったときに作られる報告（`task-dispatch` と同じ形）を手で入れる。
        let summary = Report {
            id: ReportId::new(),
            project_id: Some(project),
            node_id: "coding".into(),
            task_id: Some(created[0]),
            kind: ReportKind::Result,
            level: 1,
            headline: "まとめ".into(),
            body: "まとめ本文".into(),
            sources: children.iter().map(|c| c.id).collect(),
            read_at: None,
            created_at: now,
        };
        store.report_append(&summary).expect("append");
        assert!(store.report_unreviewed_children("coding").expect("pending").is_empty());

        // まとめの報告は、その 1 段上（秘書）のレビュー対象になる。
        let up = store.report_unreviewed_children("secretary").expect("pending");
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].id, summary.id);
        assert_eq!(
            store.report_list(&ReportFilter::default()).expect("list").len(),
            5
        );
    }
}
