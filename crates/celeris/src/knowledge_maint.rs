//! 知識の自動メンテナンス（ADR-0047 D4。Phase 62）。
//!
//! **LLM はここに一切無い**（CLAUDE.md「ディスパッチャやストアに LLM 呼び出しを入れない」）。ここは
//! `reports.rs`/`milestone_review.rs` と同じ形: tick から同期で呼ばれ、ストアと KB のファイルを見て
//! 「知識整理 run を起こすか」「起きている run を KB へ適用するか」を**決定的に**判断するだけ。
//! LLM が動くのは `langmem` アダプタ（`task-worker`）が起こす python プロセスの中だけ。
//!
//! - [`schedule`] — 終端になり報告ができたタスクのうち、まだ知識整理 run を持たないものを 1 tick に
//!   1 件だけ選び、`knowledge` harness（adapter = `langmem`、tier = cheap）の支援タスクを作る
//!   （`knowledge_runs` に `scheduled` の目印を残す）。
//! - [`apply_finished`] — `knowledge_runs` が `scheduled` のまま、その run のタスクが終端になったものを
//!   見つけ、`done` なら `artifacts/knowledge-candidates.json` を読んで
//!   [`task_ops::knowledge::apply_candidates`] で KB へ適用し、`failed`/`cancelled` ならそのまま
//!   `failed` にする。

use std::path::Path;

use task_core::knowledge::{self as kb, MaintenanceInput, RelatedPage};
use task_core::report::{self, support_kind};
use task_core::{
    Event, GenreSpec, KnowledgeRunState, ListFilter, ListOrder, RoleSpec, Status, StoreError, Task,
    TaskId, TaskKind, TaskStore, Tier,
};
use task_ops::add::{NewTaskSpec, PriorityInput};
use task_worker::MemoryDir;
use time::OffsetDateTime;

/// `langmem` の adapter id（`task_worker::LangMemAdapter::ID` と同じ。celeris は task-worker を
/// 直接 import しなくても済むよう、ここでも定数として持つ）。
const LANGMEM_ADAPTER: &str = "langmem";
/// 知識整理 run の予算（短い抽出だけなので小さく取る。ADR-0047 D4 / `harness::BUILTIN_KNOWLEDGE` と同じ値）。
const KNOWLEDGE_BUDGET_MAX_TURNS: u32 = 4;
const KNOWLEDGE_BUDGET_MAX_WALL_SECS: u64 = 900;
const KNOWLEDGE_BUDGET_MAX_RETRIES: u32 = 1;
/// 終端タスクを探すときに見る件数の上限（`reports::OPEN_TASK_SCAN` と同じ考え方）。
const TASK_SCAN_LIMIT: usize = 500;
/// `knowledge_runs` の `scheduled` を探すときに見る件数の上限。
const RUN_SCAN_LIMIT: usize = 500;
/// 関連ページの本文抜粋の上限（字数）。
const EXCERPT_MAX_CHARS: usize = 800;
/// 既存の索引の題名を依頼文に入れる上限（前置きの索引と同じ上限。ADR-0047 D2）。
const EXISTING_TITLES_MAX: usize = 200;

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.trim().to_string();
    }
    s.chars().take(max).collect::<String>().trim().to_string()
}

/// tick ごとに 1 回呼ぶ。`[knowledge.langmem] enabled = false` か KB が未初期化なら何もしない。
/// 対象になり得るタスクを 1 件見つけたら、その 1 件の知識整理タスクを作って終わる
/// （ADR-0047 §3 Phase 62「1 タスクにつき 1 回」「1 tick に最大 1 件」）。**LLM は呼ばない**。
#[allow(clippy::too_many_arguments)]
pub fn schedule(
    store: &dyn TaskStore,
    knowledge_root: &Path,
    enabled: bool,
    // backfill 禁止（ADR-0037 D5 と同じ規則。実機 2026-09-20: 有効にした瞬間に過去の終端タスク 49 件ぶんの
    // 知識整理 run が起きるところだった）: この時刻より前に終端になったタスクは対象にしない。daemon の起動時刻を渡す。
    not_before: OffsetDateTime,
    max_related_pages: usize,
    memory_dir: Option<&MemoryDir>,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Result<Vec<TaskId>, StoreError> {
    if !enabled || !task_ops::knowledge::exists(knowledge_root) {
        return Ok(Vec::new());
    }
    let filter = ListFilter {
        statuses: vec![Status::Done, Status::Failed, Status::Cancelled],
        ..ListFilter::default()
    };
    let page = store.list_page(&filter, ListOrder::UpdatedDesc, None, TASK_SCAN_LIMIT)?;
    for task in page.items {
        // 裏方（対話・計画・圧縮・承認・合成レビュー・途中目標レビュー・知識整理自身）は対象外。
        if support_kind(&task).is_some() {
            continue;
        }
        if task.updated_at < not_before {
            continue;
        }
        // 検証の煙試験（ADR-0041 D5）も対象外（tick が動く通常運用では基本的に出てこないが、念のため）。
        if task.role.as_deref() == Some(task_core::BUILTIN_SMOKE) {
            continue;
        }
        if task.assignee.is_none() {
            continue; // 担当ノードが無ければ知識整理タスクの担当も決められない。
        }
        if store.knowledge_run_exists(task.id)? {
            continue;
        }
        if let Some(project_id) = task.project_id
            && let Some(project) = store.project_get(project_id)?
            && project.archived_at.is_some()
        {
            continue;
        }
        let Some(spec) = build_run_spec(store, knowledge_root, &task, max_related_pages, memory_dir)?
        else {
            continue;
        };
        let run_task = match task_ops::add::create_support_task(store, spec, roles, genres, now) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "knowledge: could not create the maintenance task");
                continue;
            }
        };
        store.knowledge_run_create(task.id, run_task.id, now)?;
        tracing::info!(task_id = %task.id, run_task_id = %run_task.id, "knowledge: scheduled a maintenance run");
        return Ok(vec![run_task.id]);
    }
    Ok(Vec::new())
}

/// ADR-0052 D3（Phase 64）: 失敗した知識整理 run を**一度だけ**作り直す。tick ごとに 1 回呼ぶ。
///
/// `knowledge_runs.state = failed` で `retried_at` がまだ無い行を 1 tick に 1 件だけ拾い、同じ元タスクから
/// 新しい run タスクを作って `retried_at` を書く（2 回目のやり直しは無い。人が
/// `celerisctl knowledge rerun <task_id>` で `retried_at` を消したときだけまた 1 回だけ拾われる）。
///
/// [`schedule`] と違って `not_before`（backfill 禁止）は見ない。やり直しの対象は「既に 1 回 run を
/// 起こした」タスクだけなので、起動より前に終端になっていた古い仕事を掘り起こすことにはならない
/// （実機 2026-09-20/21 にトンネルが落ちて落ちた 4 件を、配備後に拾い直すための経路）。
///
/// 2 回目が `langmem` で走るか汎用ハーネスで走るかは、dispatch のときの到達性の検査が決める（D1）。
#[allow(clippy::too_many_arguments)]
pub fn retry_failed(
    store: &dyn TaskStore,
    knowledge_root: &Path,
    enabled: bool,
    max_related_pages: usize,
    memory_dir: Option<&MemoryDir>,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Result<Vec<TaskId>, StoreError> {
    if !enabled || !task_ops::knowledge::exists(knowledge_root) {
        return Ok(Vec::new());
    }
    for run in store.knowledge_run_recent(RUN_SCAN_LIMIT)? {
        if run.state != KnowledgeRunState::Failed || run.retried_at.is_some() {
            continue;
        }
        let Some(task) = store.get(run.task_id)? else {
            continue; // 元のタスクが消えている（やり直しても依頼文を組めない）。
        };
        let Some(spec) = build_run_spec(store, knowledge_root, &task, max_related_pages, memory_dir)?
        else {
            continue;
        };
        let run_task = match task_ops::add::create_support_task(store, spec, roles, genres, now) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "knowledge: could not create the retry task");
                continue;
            }
        };
        // `retried_at IS NULL` の行だけが書き換わる（ADR-0052 D3「一度だけ」はストアが守る）。
        if !store.knowledge_run_retry(run.task_id, run_task.id, now)? {
            tracing::warn!(task_id = %run.task_id, "knowledge: the run was already retried; skipping");
            continue;
        }
        tracing::info!(task_id = %run.task_id, run_task_id = %run_task.id, "knowledge: retrying a failed maintenance run (once)");
        return Ok(vec![run_task.id]);
    }
    Ok(Vec::new())
}

/// 元のタスク 1 件から知識整理 run の依頼（[`NewTaskSpec`]）を組む（[`schedule`] と [`retry_failed`] が
/// 同じものを使う）。報告がまだ無い・担当がいないなら `None`。**決定的**（LLM は呼ばない）。
fn build_run_spec(
    store: &dyn TaskStore,
    knowledge_root: &Path,
    task: &Task,
    max_related_pages: usize,
    memory_dir: Option<&MemoryDir>,
) -> Result<Option<NewTaskSpec>, StoreError> {
    let Some(assignee) = task.assignee.clone() else {
        return Ok(None);
    };
    {
        let reports = store.report_list(&task_core::ReportFilter {
            task_id: Some(task.id),
            limit: 1,
            ..Default::default()
        })?;
        let Some(report) = reports.into_iter().next() else {
            return Ok(None); // まだ報告が無い（terminal と報告生成は別トランザクションなので、極短い間だけあり得る）。
        };

        let comments = store
            .comments_for(task.id)?
            .into_iter()
            .map(|c| {
                let who = c
                    .author
                    .clone()
                    .unwrap_or_else(|| c.author_kind.as_str().to_string());
                format!("{who}: {}", c.body.trim())
            })
            .collect::<Vec<_>>();

        let query = [
            task.title.as_str(),
            &task.labels.join(" "),
            &task.skills.join(" "),
        ]
        .join(" ");
        let related_pages: Vec<RelatedPage> =
            task_ops::knowledge::search(knowledge_root, &query, None, max_related_pages)
                .into_iter()
                .filter_map(|hit| {
                    let raw = task_ops::knowledge::read_page(knowledge_root, &hit.item.path)?;
                    let (_, body) = kb::front_matter(&raw);
                    Some(RelatedPage {
                        path: hit.item.path,
                        title: hit.item.title,
                        excerpt: truncate_chars(body, EXCERPT_MAX_CHARS),
                    })
                })
                .collect();

        let notes_excerpt = memory_dir
            .map(|m| {
                m.load(&assignee, task.project_id.map(|p| p.to_string()).as_deref())
                    .notes
            })
            .unwrap_or_default();

        let existing_titles: Vec<String> = task_ops::knowledge::ensure_index(knowledge_root)
            .items
            .iter()
            .take(EXISTING_TITLES_MAX)
            .map(|i| format!("{} — {}", i.path, i.title))
            .collect();

        let input = MaintenanceInput {
            task_id: task.id.to_string(),
            task_title: task.title.clone(),
            task_objective: task.objective.clone(),
            report_headline: report.headline.clone(),
            report_body: report.body.clone(),
            result_summary: String::new(),
            comments,
            related_pages,
            notes_excerpt,
            existing_titles,
        };
        let objective = kb::maintenance_objective(&input);

        let spec = NewTaskSpec {
            repos: Vec::new(),
            title: format!("知識整理: {}", task.title),
            objective,
            acceptance: Vec::new(),
            kind: TaskKind::Execute,
            tier: Some(Tier::Cheap),
            priority: Some(PriorityInput::Number(0)),
            parent: None,
            depends_on: Vec::new(),
            max_turns: Some(KNOWLEDGE_BUDGET_MAX_TURNS),
            max_wall_secs: Some(KNOWLEDGE_BUDGET_MAX_WALL_SECS),
            max_retries: KNOWLEDGE_BUDGET_MAX_RETRIES,
            role: Some(report::KNOWLEDGE_ROLE.to_string()),
            genre: None,
            aggregate: false,
            skills: Vec::new(),
            mode: None,
            project_id: task.project_id,
            milestone_id: None,
            assignee: Some(assignee),
            workspace: None,
            cluster: None,
            // ADR-0047 D4: harness の解決に頼らず、adapter/tier を明示する（`knowledge` harness は
            // 組み込みのままで `[[genres]]`/`[[roles]]` に射影されないため。task_core::report::KNOWLEDGE_ROLE
            // のドキュメントコメント参照）。
            adapter: Some(LANGMEM_ADAPTER.to_string()),
            labels: Vec::new(),
            category: None,
            status: None,
        };
        Ok(Some(spec))
    }
}

/// ADR-0052 D2: その run が**どの経路**で抽出したか（`knowledge_runs.via` と `summary_json.via`）。
///
/// 判断材料は run タスクの `WorkerStarted.adapter`（= 実際に走ったアダプタ）だけ。`langmem` なら
/// `"langmem"`、それ以外なら `"fallback:<adapter>"`。run が 1 度も始まらなかったときは `None`。
fn via_of(store: &dyn TaskStore, run_task_id: TaskId) -> Option<String> {
    let events = store.events_for(run_task_id).ok()?;
    let adapter = events.iter().rev().find_map(|(_, e)| match e {
        Event::WorkerStarted { adapter, .. } => Some(adapter.clone()),
        _ => None,
    })?;
    Some(if adapter == LANGMEM_ADAPTER {
        task_core::VIA_LANGMEM.to_string()
    } else {
        task_core::via_fallback(&adapter)
    })
}

/// `artifacts/knowledge-candidates.json` の形。
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct CandidatesFile {
    #[serde(default)]
    candidates: Vec<kb::Candidate>,
}

/// tick ごとに 1 回呼ぶ。`knowledge_runs` が `scheduled` のまま、その run のタスクが終端になったものを
/// 見つけて適用する。適用した件数を返す（複数を同じ tick で処理してよい。適用は決定的で軽い）。
pub fn apply_finished(
    store: &dyn TaskStore,
    knowledge_root: &Path,
    workspace_root: &Path,
    now: OffsetDateTime,
) -> Result<usize, StoreError> {
    if !task_ops::knowledge::exists(knowledge_root) {
        return Ok(0);
    }
    let mut applied = 0usize;
    for run in store.knowledge_run_recent(RUN_SCAN_LIMIT)? {
        if run.state != KnowledgeRunState::Scheduled {
            continue;
        }
        let Some(run_task) = store.get(run.run_task_id)? else {
            // 支援タスクが（何らかの理由で）消えている。二度と進まないので failed にして終わらせる。
            store.knowledge_run_finish(run.task_id, KnowledgeRunState::Failed, now, None, None)?;
            continue;
        };
        if !run_task.status.is_terminal() {
            continue;
        }
        // ADR-0052 D2: 適用は経路に関係なく同じ。どちらで走ったかだけ `via` に残す。
        let via = via_of(store, run.run_task_id);
        if run_task.status != Status::Done {
            store.knowledge_run_finish(
                run.task_id,
                KnowledgeRunState::Failed,
                now,
                None,
                via.as_deref(),
            )?;
            continue;
        }
        let workspace_dir = workspace_root.join(run_task.id.to_string());
        let artifacts_dir = task_core::artifacts::artifacts_dir_for(&run_task, &workspace_dir);
        let candidates_path = artifacts_dir.join("knowledge-candidates.json");
        let candidates = std::fs::read_to_string(&candidates_path)
            .ok()
            .and_then(|text| serde_json::from_str::<CandidatesFile>(&text).ok())
            .map(|f| f.candidates)
            .unwrap_or_default();
        let outcome = task_ops::knowledge::apply_candidates(
            knowledge_root,
            &run.task_id.to_string(),
            &candidates,
        );
        tracing::info!(
            task_id = %run.task_id,
            run_task_id = %run.run_task_id,
            via = via.as_deref().unwrap_or("-"),
            committed = outcome.committed.len(),
            inboxed = outcome.inboxed.len(),
            dropped = outcome.dropped.len(),
            "knowledge: applied the maintenance run's candidates"
        );
        let mut summary = outcome.summary();
        summary.via = via.clone();
        store.knowledge_run_finish(
            run.task_id,
            KnowledgeRunState::Done,
            now,
            Some(&summary),
            via.as_deref(),
        )?;
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::org::{OrgKind, OrgNode};
    use task_core::report::{Report, ReportId, ReportKind, ReportStore};
    use task_core::{
        Budget, KnowledgeRunStore, Project, ProjectId, ProjectStatus, SqliteStore,
        TaskId as CoreTaskId, Tier, WorkerHint, WorkspaceSpec,
    };

    fn store_with_node() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("open");
        let now = OffsetDateTime::now_utc();
        store
            .org_upsert(&OrgNode {
                profile: Default::default(),
                id: "coding".into(),
                parent_id: None,
                name: "coding".into(),
                kind: OrgKind::Secretary,
                genre: None,
                brief: String::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            })
            .expect("seed org");
        store
    }

    fn seed_project(store: &SqliteStore, archived: bool) -> ProjectId {
        let now = OffsetDateTime::now_utc();
        let project = Project {
            archived_at: if archived { Some(now) } else { None },
            paused_from: None,
            id: ProjectId::new(),
            title: "案件".into(),
            request: "依頼".into(),
            status: ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: now,
            updated_at: now,
        };
        store.project_create(&project).expect("project");
        project.id
    }

    #[allow(clippy::too_many_arguments)]
    fn terminal_task(
        status: Status,
        assignee: Option<&str>,
        role: Option<&str>,
        project_id: Option<ProjectId>,
        conversation: bool,
    ) -> task_core::Task {
        let now = OffsetDateTime::now_utc();
        task_core::Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: CoreTaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "pegasus の初期セットアップ".into(),
            objective: "pegasus に pjsub の使い方を確認する".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status,
            priority: 1,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "ws".into(),
                mode: None,
            },
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 0,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: role.map(str::to_string),
            genre: None,
            aggregate: false,
            project_id,
            milestone_id: None,
            assignee: assignee.map(str::to_string),
            conversation: if conversation {
                Some(task_core::MessageId::new())
            } else {
                None
            },
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    fn add_report(store: &SqliteStore, task: &task_core::Task) {
        store
            .report_append(&Report {
                id: ReportId::new(),
                project_id: task.project_id,
                node_id: task.assignee.clone().unwrap_or_default(),
                task_id: Some(task.id),
                kind: ReportKind::Result,
                level: 0,
                headline: "pjsub の投げ方を確認した".into(),
                body: "pjsub -L node=1 で投げられる。".into(),
                sources: Vec::new(),
                read_at: None,
                created_at: task.updated_at,
            })
            .expect("append report");
    }

    fn kb_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("knowledge");
        task_ops::knowledge::init(&root).expect("init");
        (dir, root)
    }

    /// backfill 禁止: `not_before` より前に終端になったタスクからは作らない（実機 2026-09-20）。
    #[test]
    fn tasks_finished_before_the_daemon_started_are_not_backfilled() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let task = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&task).expect("insert");
        add_report(&store, &task);
        let now = OffsetDateTime::now_utc();
        let later = task.updated_at + time::Duration::hours(1);
        let created =
            schedule(&store, &root, true, later, 10, None, &[], &[], now).expect("schedule");
        assert!(created.is_empty(), "古い終端タスクは対象外");
        let created = schedule(
            &store,
            &root,
            true,
            task.updated_at,
            10,
            None,
            &[],
            &[],
            now,
        )
        .expect("schedule");
        assert_eq!(created.len(), 1, "起動後に終端になったものは対象");
    }

    /// ADR-0047 D4: 終端タスク（担当あり・報告あり）から知識整理タスクを 1 件作り、
    /// `knowledge_runs` に `scheduled` を残す。2 回目は同じタスクからは作らない。
    #[test]
    fn schedule_creates_one_maintenance_task_and_is_idempotent() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let task = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&task).expect("insert");
        add_report(&store, &task);

        let now = OffsetDateTime::now_utc();
        let created = schedule(
            &store,
            &root,
            true,
            OffsetDateTime::UNIX_EPOCH,
            10,
            None,
            &[],
            &[],
            now,
        )
        .expect("schedule");
        assert_eq!(created.len(), 1);
        let run_task = store.get(created[0]).expect("get").expect("some");
        assert_eq!(run_task.role.as_deref(), Some(report::KNOWLEDGE_ROLE));
        assert_eq!(
            run_task.worker_hint.adapter.as_deref(),
            Some(LANGMEM_ADAPTER)
        );
        assert_eq!(run_task.worker_hint.tier, Tier::Cheap);
        assert_eq!(run_task.assignee.as_deref(), Some("coding"));
        assert!(
            run_task.objective.contains(&task.id.to_string()),
            "{}",
            run_task.objective
        );
        assert!(
            run_task.objective.contains("pjsub の投げ方を確認した"),
            "{}",
            run_task.objective
        );
        assert_eq!(
            task_core::report::support_kind(&run_task),
            Some("knowledge")
        );

        let run = store
            .knowledge_run_get(task.id)
            .expect("get run")
            .expect("some");
        assert_eq!(run.run_task_id, created[0]);
        assert_eq!(run.state, KnowledgeRunState::Scheduled);

        // 2 回目は同じタスクからはもう作らない（既に `knowledge_runs` がある）。
        assert!(
            schedule(
                &store,
                &root,
                true,
                OffsetDateTime::UNIX_EPOCH,
                10,
                None,
                &[],
                &[],
                now
            )
            .expect("schedule again")
            .is_empty()
        );
    }

    /// `enabled = false` / KB 未初期化なら何もしない。
    #[test]
    fn schedule_noops_when_disabled_or_the_kb_is_missing() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let task = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&task).expect("insert");
        add_report(&store, &task);
        let now = OffsetDateTime::now_utc();
        assert!(
            schedule(
                &store,
                &root,
                false,
                OffsetDateTime::UNIX_EPOCH,
                10,
                None,
                &[],
                &[],
                now
            )
            .expect("schedule")
            .is_empty(),
            "enabled = false"
        );
        let missing_root = root.join("does-not-exist");
        assert!(
            schedule(
                &store,
                &missing_root,
                true,
                OffsetDateTime::UNIX_EPOCH,
                10,
                None,
                &[],
                &[],
                now
            )
            .expect("schedule")
            .is_empty(),
            "KB 未初期化"
        );
    }

    /// 裏方（対話・圧縮）・担当なし・案件アーカイブ済みは対象外。
    #[test]
    fn schedule_skips_support_tasks_unassigned_tasks_and_archived_projects() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let now = OffsetDateTime::now_utc();

        let conversation = terminal_task(Status::Done, Some("coding"), None, None, true);
        store.insert(&conversation).expect("insert");
        add_report(&store, &conversation);

        let compaction = terminal_task(
            Status::Done,
            Some("coding"),
            Some(task_core::COMPACTION_ROLE),
            None,
            false,
        );
        store.insert(&compaction).expect("insert");
        add_report(&store, &compaction);

        let unassigned = terminal_task(Status::Done, None, None, None, false);
        store.insert(&unassigned).expect("insert");
        add_report(&store, &unassigned);

        let archived_project = seed_project(&store, true);
        let archived = terminal_task(
            Status::Done,
            Some("coding"),
            None,
            Some(archived_project),
            false,
        );
        store.insert(&archived).expect("insert");
        add_report(&store, &archived);

        assert!(
            schedule(
                &store,
                &root,
                true,
                OffsetDateTime::UNIX_EPOCH,
                10,
                None,
                &[],
                &[],
                now
            )
            .expect("schedule")
            .is_empty()
        );
    }

    /// 報告がまだ無い終端タスクは（極短い間だけ）対象外。
    #[test]
    fn schedule_skips_a_terminal_task_without_a_report_yet() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let task = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&task).expect("insert");
        let now = OffsetDateTime::now_utc();
        assert!(
            schedule(
                &store,
                &root,
                true,
                OffsetDateTime::UNIX_EPOCH,
                10,
                None,
                &[],
                &[],
                now
            )
            .expect("schedule")
            .is_empty()
        );
    }

    /// ADR-0047 D4: `done` の知識整理 run は `artifacts/knowledge-candidates.json` を読んで適用し、
    /// `knowledge_runs` を `done` にする。`failed` の run は候補を読まず `failed` にする。
    #[test]
    fn apply_finished_applies_a_done_run_and_fails_a_failed_run() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let workspace_root = tempfile::tempdir().expect("workspace");

        let source = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&source).expect("insert");
        add_report(&store, &source);

        // done の run: `<workspace_root>/<run_task_id>/artifacts/knowledge-candidates.json` を用意する。
        let done_run_task = terminal_task(
            Status::Done,
            Some("coding"),
            Some(report::KNOWLEDGE_ROLE),
            None,
            false,
        );
        store.insert(&done_run_task).expect("insert");
        let artifacts_dir = workspace_root
            .path()
            .join(done_run_task.id.to_string())
            .join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("mkdir");
        std::fs::write(
            artifacts_dir.join("knowledge-candidates.json"),
            r#"{"candidates": [{"op": "create", "path": "environment/tools/newtool.md",
                "title": "newtool", "tags": [], "scope": "environment", "body": "使い方。",
                "sources": ["task:x"], "confidence": "high"}]}"#,
        )
        .expect("write candidates");
        store
            .knowledge_run_create(source.id, done_run_task.id, OffsetDateTime::now_utc())
            .expect("create run");

        // failed の run（別の元タスク）。
        let source2 = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&source2).expect("insert");
        add_report(&store, &source2);
        let failed_run_task = terminal_task(
            Status::Failed,
            Some("coding"),
            Some(report::KNOWLEDGE_ROLE),
            None,
            false,
        );
        store.insert(&failed_run_task).expect("insert");
        store
            .knowledge_run_create(source2.id, failed_run_task.id, OffsetDateTime::now_utc())
            .expect("create run");

        let now = OffsetDateTime::now_utc();
        let applied = apply_finished(&store, &root, workspace_root.path(), now).expect("apply");
        assert_eq!(applied, 1, "done の run だけ数える");

        let done = store
            .knowledge_run_get(source.id)
            .expect("get")
            .expect("some");
        assert_eq!(done.state, KnowledgeRunState::Done);
        let summary = done.summary.expect("summary");
        assert_eq!(summary.ingested, 1);
        assert!(root.join("environment/tools/newtool.md").exists());

        let failed = store
            .knowledge_run_get(source2.id)
            .expect("get")
            .expect("some");
        assert_eq!(failed.state, KnowledgeRunState::Failed);
        assert!(failed.summary.is_none());

        // 未終端の run は触らない。
        let source3 = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&source3).expect("insert");
        add_report(&store, &source3);
        let running_run_task = terminal_task(
            Status::Running,
            Some("coding"),
            Some(report::KNOWLEDGE_ROLE),
            None,
            false,
        );
        store.insert(&running_run_task).expect("insert");
        store
            .knowledge_run_create(source3.id, running_run_task.id, OffsetDateTime::now_utc())
            .expect("create run");
        assert_eq!(
            apply_finished(&store, &root, workspace_root.path(), now).expect("apply"),
            0
        );
        assert_eq!(
            store
                .knowledge_run_get(source3.id)
                .expect("get")
                .expect("some")
                .state,
            KnowledgeRunState::Scheduled
        );
    }

    /// run タスクに `WorkerStarted` を 1 つ書く（`via` の判定材料）。
    fn started_with(store: &SqliteStore, task: &task_core::Task, adapter: &str) {
        store
            .append_event(
                task.id,
                &task_core::Event::WorkerStarted {
                    run_id: ulid::Ulid::new().to_string(),
                    adapter: adapter.to_string(),
                    model: "m".into(),
                    provider: Some("p".into()),
                    account: None,
                    role: None,
                    task_role: task.role.clone(),
                },
            )
            .expect("append");
    }

    /// 元タスク 1 件ぶんの「失敗した知識整理 run」を作る（報告つき）。
    fn failed_run(store: &SqliteStore) -> (task_core::Task, task_core::Task) {
        let source = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&source).expect("insert");
        add_report(store, &source);
        let run_task = terminal_task(
            Status::Failed,
            Some("coding"),
            Some(report::KNOWLEDGE_ROLE),
            None,
            false,
        );
        store.insert(&run_task).expect("insert");
        store
            .knowledge_run_create(source.id, run_task.id, OffsetDateTime::now_utc())
            .expect("create run");
        (source, run_task)
    }

    /// ADR-0052 D3: 失敗した知識整理 run は次の tick で**ちょうど 1 回**だけ作り直される（3 回目は無い）。
    /// `not_before`（backfill 禁止）は見ない ＝ 実機で 2026-09-20/21 に落ちた古い run も拾える。
    #[test]
    fn a_failed_run_is_retried_exactly_once() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let workspace_root = tempfile::tempdir().expect("workspace");
        let (source, first_run) = failed_run(&store);
        started_with(&store, &first_run, LANGMEM_ADAPTER);

        let now = OffsetDateTime::now_utc();
        apply_finished(&store, &root, workspace_root.path(), now).expect("apply");
        let run = store
            .knowledge_run_get(source.id)
            .expect("get")
            .expect("some");
        assert_eq!(run.state, KnowledgeRunState::Failed);
        assert_eq!(run.via.as_deref(), Some(task_core::VIA_LANGMEM));
        assert!(run.retried_at.is_none());

        // 1 回目のやり直し: 新しい run タスクができ、`retried_at` が入る。
        let retried =
            retry_failed(&store, &root, true, 10, None, &[], &[], now).expect("retry_failed");
        assert_eq!(retried.len(), 1, "1 tick に 1 件");
        let second_run_id = retried[0];
        assert_ne!(second_run_id, first_run.id);
        let run = store
            .knowledge_run_get(source.id)
            .expect("get")
            .expect("some");
        assert_eq!(run.run_task_id, second_run_id);
        assert_eq!(run.state, KnowledgeRunState::Scheduled);
        assert!(run.retried_at.is_some());
        assert!(run.via.is_none(), "やり直しで経路の記録は消える");
        let second = store.get(second_run_id).expect("get").expect("some");
        assert_eq!(second.worker_hint.adapter.as_deref(), Some(LANGMEM_ADAPTER));
        assert_eq!(
            task_core::report::support_kind(&second),
            Some("knowledge"),
            "やり直しも裏方の支援タスク"
        );

        // まだ終わっていないので 2 回目の呼び出しでは何も起きない。
        assert!(
            retry_failed(&store, &root, true, 10, None, &[], &[], now)
                .expect("retry again")
                .is_empty()
        );

        // 2 回目も失敗した（`apply_finished` が `failed` にする）→ もう作り直さない（`retried_at` あり）。
        store
            .knowledge_run_finish(source.id, KnowledgeRunState::Failed, now, None, None)
            .expect("finish");
        assert!(
            retry_failed(&store, &root, true, 10, None, &[], &[], now)
                .expect("no third")
                .is_empty(),
            "3 回目は無い"
        );

        // `celerisctl knowledge rerun` 相当（`retried_at` を消す）でもう 1 回だけ拾われる。
        assert!(store.knowledge_run_reset(source.id).expect("reset"));
        assert_eq!(
            retry_failed(&store, &root, true, 10, None, &[], &[], now)
                .expect("after rerun")
                .len(),
            1
        );
    }

    /// `enabled = false` / KB 未初期化なら、やり直しも起きない。
    #[test]
    fn retry_noops_when_disabled_or_the_kb_is_missing() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let (_source, run_task) = failed_run(&store);
        started_with(&store, &run_task, LANGMEM_ADAPTER);
        let now = OffsetDateTime::now_utc();
        apply_finished(&store, &root, root.as_path(), now).expect("apply");
        assert!(
            retry_failed(&store, &root, false, 10, None, &[], &[], now)
                .expect("disabled")
                .is_empty()
        );
        assert!(
            retry_failed(
                &store,
                &root.join("does-not-exist"),
                true,
                10,
                None,
                &[],
                &[],
                now
            )
            .expect("no kb")
            .is_empty()
        );
    }

    /// ADR-0052 D2: フォールバックで走った run も適用は同じで、`via = "fallback:<adapter>"` が
    /// `knowledge_runs` と `summary_json` の両方に残る。
    #[test]
    fn a_fallback_run_is_applied_the_same_way_and_records_its_via() {
        let store = store_with_node();
        let (_dir, root) = kb_dir();
        let workspace_root = tempfile::tempdir().expect("workspace");

        let source = terminal_task(Status::Done, Some("coding"), None, None, false);
        store.insert(&source).expect("insert");
        add_report(&store, &source);
        let run_task = terminal_task(
            Status::Done,
            Some("coding"),
            Some(report::KNOWLEDGE_ROLE),
            None,
            false,
        );
        store.insert(&run_task).expect("insert");
        // フォールバック先は汎用のアダプタ（ここでは開発用の `fake`）。
        started_with(&store, &run_task, "fake");
        let artifacts_dir = workspace_root
            .path()
            .join(run_task.id.to_string())
            .join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).expect("mkdir");
        std::fs::write(
            artifacts_dir.join("knowledge-candidates.json"),
            r#"{"candidates": [{"op": "create", "path": "environment/tools/fallback.md",
                "title": "fallback", "tags": [], "scope": "environment", "body": "使い方。",
                "sources": ["task:x"], "confidence": "high"}]}"#,
        )
        .expect("write candidates");
        store
            .knowledge_run_create(source.id, run_task.id, OffsetDateTime::now_utc())
            .expect("create run");

        let now = OffsetDateTime::now_utc();
        assert_eq!(
            apply_finished(&store, &root, workspace_root.path(), now).expect("apply"),
            1
        );
        let run = store
            .knowledge_run_get(source.id)
            .expect("get")
            .expect("some");
        assert_eq!(run.state, KnowledgeRunState::Done);
        assert_eq!(run.via.as_deref(), Some("fallback:fake"));
        let summary = run.summary.expect("summary");
        assert_eq!(summary.ingested, 1, "適用は経路に関係なく同じ");
        assert_eq!(summary.via.as_deref(), Some("fallback:fake"));
        assert!(root.join("environment/tools/fallback.md").exists());
    }
}
