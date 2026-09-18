//! 実行中の委譲の検証（ADR-0016 D2 / M6 / M7）。ストアを見る検証（既存 ID の依存、祖先、木の深さ・run 数）は
//! ここに置く。ストアを見ない検証（空欄・配列内インデックスの範囲・自己参照・閉路）は `task_core::delegate`
//! （`validate_each`）にある。I/O は `TaskStore` の読み取りだけ。LLM 呼び出し・挿入は無い
//! （挿入は `TaskStore::delegate_children` が行う）。

use task_core::{
    DelegateDep, DelegateTask, DelegationLimits, GenreSpec, RoleSpec, Status, Task, TaskId, TaskStore, WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::OpsError;

/// `plan_delegation` の結果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DelegationOutcome {
    /// 挿入してよい子（`status = Draft`。`TaskStore::delegate_children` が `Accept` する）。提案の順。
    pub accepted: Vec<Task>,
    /// 拒否した提案の理由。書式は `tasks[<i>] "<title>": <reason>`。ディスパッチャが `WorkerProgress` に載せる。
    pub rejected: Vec<String>,
}

fn reject(index: usize, title: &str, reason: impl std::fmt::Display) -> String {
    format!("tasks[{index}] {title:?}: {reason}")
}

/// 祖先の数 + 1（根 = 1）。親を辿る（64 hop で打ち切り）。
pub fn tree_depth(store: &dyn TaskStore, task: &Task) -> Result<u32, OpsError> {
    let mut depth: u32 = 1;
    let mut current = task.clone();
    for _ in 0..64 {
        match current.parent_id {
            Some(parent_id) => match store.get(parent_id)? {
                Some(parent) => {
                    depth += 1;
                    current = parent;
                }
                None => break,
            },
            None => break,
        }
    }
    Ok(depth)
}

/// 木の根の ID。
pub fn tree_root(store: &dyn TaskStore, task: &Task) -> Result<TaskId, OpsError> {
    let mut current = task.clone();
    for _ in 0..64 {
        match current.parent_id {
            Some(parent_id) => match store.get(parent_id)? {
                Some(parent) => current = parent,
                None => break,
            },
            None => break,
        }
    }
    Ok(current.id)
}

/// 根とその全子孫の `Event::WorkerStarted { role: None, .. }`（ワーカー run）の合計。
pub fn tree_worker_runs(store: &dyn TaskStore, root: TaskId) -> Result<u32, OpsError> {
    let mut total: u32 = 0;
    let mut stack: Vec<TaskId> = vec![root];
    while let Some(id) = stack.pop() {
        let events = store.events_for(id)?;
        for (_, event) in events {
            if let task_core::Event::WorkerStarted { role: None, .. } = event {
                total += 1;
            }
        }
        for child in store.children(id)? {
            stack.push(child.id);
        }
    }
    Ok(total)
}

/// `parent` の直接の子のうち終端でないものの数（`Status::is_terminal`）。
pub fn pending_children(store: &dyn TaskStore, parent: TaskId) -> Result<usize, OpsError> {
    Ok(store
        .children(parent)?
        .iter()
        .filter(|c| !c.status.is_terminal())
        .count())
}

/// `parent` の祖先の ID 列（親、その親、…）。
pub fn ancestors(store: &dyn TaskStore, task: &Task) -> Result<Vec<TaskId>, OpsError> {
    let mut out = Vec::new();
    let mut current = task.clone();
    for _ in 0..64 {
        match current.parent_id {
            Some(parent_id) => {
                out.push(parent_id);
                match store.get(parent_id)? {
                    Some(parent) => current = parent,
                    None => break,
                }
            }
            None => break,
        }
    }
    Ok(out)
}

/// ADR-0039 D2: そのタスクが属する案件の作業場所（`projects.workspace`）。案件に属さない・案件が
/// 作業場所を決めていないなら `None`（従来どおり、子は親の workspace を継ぐ）。ストアの読み取りだけ。
pub fn project_workspace(store: &dyn TaskStore, task: &Task) -> Result<Option<WorkspaceSpec>, OpsError> {
    let Some(project_id) = task.project_id else {
        return Ok(None);
    };
    Ok(store.project_get(project_id)?.and_then(|p| p.workspace))
}

/// ADR-0016 D2 / M6 / M7, ADR-0027 D1: 実行中の委譲の検証。`already_delegated_this_run` は同じ run で
/// 既に受け入れた件数。`genres` は `genre`（分野）の解決・検証に使う（未知の `genre`、`role` と組み合わせた
/// ときの不整合は `task_core::validate_each` が拒否する）。
#[allow(clippy::too_many_arguments)]
pub fn plan_delegation(
    store: &dyn TaskStore,
    parent: &Task,
    proposals: &[DelegateTask],
    already_delegated_this_run: usize,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    limits: &DelegationLimits,
    now: OffsetDateTime,
) -> Result<DelegationOutcome, OpsError> {
    if proposals.is_empty() {
        return Ok(DelegationOutcome::default());
    }

    // 1. 木全体の上限。1 件でも当たれば全件拒否。
    let would_be_depth = tree_depth(store, parent)? + 1;
    if would_be_depth > limits.max_tree_depth {
        let reason = format!("tree depth would become {would_be_depth} (max {})", limits.max_tree_depth);
        let rejected = proposals
            .iter()
            .enumerate()
            .map(|(i, t)| reject(i, &t.title, &reason))
            .collect();
        return Ok(DelegationOutcome {
            accepted: vec![],
            rejected,
        });
    }
    let root = tree_root(store, parent)?;
    let runs = tree_worker_runs(store, root)?;
    if runs >= limits.max_tree_runs {
        let reason = format!("tree already has {runs} worker runs (max {})", limits.max_tree_runs);
        let rejected = proposals
            .iter()
            .enumerate()
            .map(|(i, t)| reject(i, &t.title, &reason))
            .collect();
        return Ok(DelegationOutcome {
            accepted: vec![],
            rejected,
        });
    }

    // 2. ストアを見ない検証。
    let each = task_core::validate_each(proposals, genres);

    // 3. ID 依存の検証（ストアを見る）。
    let ancestor_ids = ancestors(store, parent)?;
    let mut per_item: Vec<Result<(), String>> = Vec::with_capacity(proposals.len());
    for (i, result) in each.into_iter().enumerate() {
        if let Err(e) = result {
            per_item.push(Err(e.to_string()));
            continue;
        }
        let mut item_err: Option<String> = None;
        for dep in &proposals[i].depends_on {
            if let DelegateDep::Id(s) = dep {
                let Ok(dep_id) = s.parse::<TaskId>() else {
                    // `validate_each` already rejects malformed ids; unreachable in practice.
                    item_err = Some(format!("dependency {s} is not a task id"));
                    break;
                };
                if dep_id == parent.id {
                    item_err = Some(format!("dependency {dep_id} is the delegating task itself"));
                    break;
                }
                if ancestor_ids.contains(&dep_id) {
                    item_err = Some(format!("dependency {dep_id} is an ancestor of the delegating task"));
                    break;
                }
                match store.get(dep_id)? {
                    None => {
                        item_err = Some(format!("dependency {dep_id} does not exist"));
                        break;
                    }
                    Some(dep) if matches!(dep.status, Status::Failed | Status::Cancelled) => {
                        item_err = Some(format!(
                            "dependency {dep_id} has status {:?} and cannot be depended on",
                            dep.status
                        ));
                        break;
                    }
                    Some(_) => {}
                }
            }
        }
        per_item.push(match item_err {
            Some(e) => Err(e),
            None => Ok(()),
        });
    }

    // 4. 1 run の件数の上限。通った提案を順に数える。
    let mut accepted_indices: Vec<usize> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();
    let mut count = already_delegated_this_run;
    for (i, result) in per_item.into_iter().enumerate() {
        match result {
            Err(reason) => rejected.push(reject(i, &proposals[i].title, reason)),
            Ok(()) => {
                if count >= limits.max_delegate_per_run {
                    rejected.push(reject(
                        i,
                        &proposals[i].title,
                        format!("per-run delegation limit ({}) reached", limits.max_delegate_per_run),
                    ));
                    continue;
                }
                count += 1;
                accepted_indices.push(i);
            }
        }
    }

    // 5. 組み立て。ADR-0033 D4: `assignee` の解決に組織図が要る（ストアの読み取りだけ。LLM は使わない）。
    let org = store.org_list()?;
    // ADR-0039 D2: 子の作業場所は 明示 > 案件の workspace > 親（案件を引くのもストアの読み取りだけ）。
    let project = project_workspace(store, parent)?;
    let home = task_core::home_dir();
    let workspace = task_core::WorkspaceContext {
        project: project.as_ref(),
        home: home.as_deref(),
    };
    let accepted =
        task_core::materialize_delegated(parent, proposals, &accepted_indices, &org, roles, genres, workspace, now);

    Ok(DelegationOutcome { accepted, rejected })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use task_core::{
        Budget, Check, Criterion, Event, RunRole, SqliteStore, Task, TaskKind, Tier, WorkerHint, WorkspaceSpec,
    };

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn dt(title: &str, deps: Vec<DelegateDep>) -> DelegateTask {
        DelegateTask {
            title: title.into(),
            objective: format!("do {title}"),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            role: None,
            genre: None,
            depends_on: deps,
            tier: None,
            assignee: None,
            workspace: None,
        }
    }

    fn make_task(parent_id: Option<TaskId>, status: Status) -> Task {
        let t = now();
        Task {
            id: TaskId::new(),
            parent_id,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status,
            priority: 3,
            worker_hint: WorkerHint {
                tier: Tier::Frontier,
                adapter: Some("fake".into()),
            },
            workspace: WorkspaceSpec::Local { path: PathBuf::from("/tmp/ws") },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 600,
                max_retries: 2,
            },
            attempts: 0,
            lease: None,
            created_at: t,
            updated_at: t,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation: None,
        }
    }

    fn insert(store: &SqliteStore, task: &Task) {
        store.insert(task).expect("insert");
    }

    #[test]
    fn accepts_proposals_maps_index_deps_and_applies_role_defaults() {
        let store = SqliteStore::open_in_memory().expect("open");
        let parent = make_task(None, Status::Running);
        insert(&store, &parent);

        let roles = vec![RoleSpec {
            id: "implementer".into(),
            tier: Some(Tier::Cheap),
            adapter: Some("codex".into()),
            max_turns: None,
            max_wall_secs: None,
            instructions: None,
        }];
        let mut a = dt("a", vec![]);
        a.role = Some("implementer".into());
        let b = dt("b", vec![DelegateDep::Index(0)]);
        let proposals = vec![a, b];

        let out = plan_delegation(&store, &parent, &proposals, 0, &roles, &[], &DelegationLimits::default(), now())
            .expect("plan_delegation");
        assert_eq!(out.accepted.len(), 2);
        assert!(out.rejected.is_empty());
        assert_eq!(out.accepted[0].worker_hint.tier, Tier::Cheap);
        assert_eq!(out.accepted[1].depends_on, vec![out.accepted[0].id]);
    }

    /// ADR-0027 D1: `plan_delegation` は分野を解決し（役割の分野が一意なら継ぐ）、`genre` を指定した
    /// タスクの `role` がその分野に無ければ拒否する。unrelated_genre はここまでの検証を通っても
    /// role/genre の不整合で落ちる。
    #[test]
    fn plan_delegation_resolves_genre_and_rejects_role_genre_mismatch() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut parent = make_task(None, Status::Running);
        parent.genre = Some("coding".into());
        insert(&store, &parent);

        let genres = vec![task_core::GenreSpec {
            id: "literature".into(),
            description: "survey".into(),
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-scout".into(), "literature-reader".into()],
            ..task_core::GenreSpec::default()
        }];

        // 1. role だけ指定: 分野が一意に決まるので "literature" を継ぐ。
        let mut by_role = dt("by-role", vec![]);
        by_role.role = Some("literature-scout".into());
        // 2. genre と role が矛盾: 拒否される。
        let mut mismatched = dt("mismatched", vec![]);
        mismatched.genre = Some("literature".into());
        mismatched.role = Some("implementer".into());
        // 3. 何も指定しない: 親の分野 "coding" を継ぐ。
        let inherits = dt("inherits", vec![]);

        let proposals = vec![by_role, mismatched, inherits];
        let out = plan_delegation(&store, &parent, &proposals, 0, &[], &genres, &DelegationLimits::default(), now())
            .expect("plan_delegation");
        assert_eq!(out.accepted.len(), 2);
        assert_eq!(out.accepted[0].title, "by-role");
        assert_eq!(out.accepted[0].genre.as_deref(), Some("literature"));
        assert_eq!(out.accepted[1].title, "inherits");
        assert_eq!(out.accepted[1].genre.as_deref(), Some("coding"));
        assert_eq!(out.rejected.len(), 1);
        assert!(out.rejected[0].contains("mismatched"), "{}", out.rejected[0]);
        assert!(out.rejected[0].contains("is not one of genre"), "{}", out.rejected[0]);
    }

    #[test]
    fn per_run_limit_rejects_the_remainder() {
        let store = SqliteStore::open_in_memory().expect("open");
        let parent = make_task(None, Status::Running);
        insert(&store, &parent);

        let limits = DelegationLimits {
            max_delegate_per_run: 2,
            ..DelegationLimits::default()
        };
        let proposals = vec![dt("a", vec![]), dt("b", vec![])];
        let out = plan_delegation(&store, &parent, &proposals, 1, &[], &[], &limits, now()).expect("plan_delegation");
        assert_eq!(out.accepted.len(), 1);
        assert_eq!(out.rejected.len(), 1);
        assert!(out.rejected[0].contains("per-run delegation limit"), "{}", out.rejected[0]);
    }

    #[test]
    fn tree_depth_limit_rejects_everything() {
        let store = SqliteStore::open_in_memory().expect("open");
        let root = make_task(None, Status::Running);
        insert(&store, &root);
        let mid = make_task(Some(root.id), Status::Running);
        insert(&store, &mid);
        let parent = make_task(Some(mid.id), Status::Running);
        insert(&store, &parent);
        // tree_depth(parent) == 3, +1 == 4 > max_tree_depth (3) -> reject all
        let limits = DelegationLimits {
            max_tree_depth: 3,
            ..DelegationLimits::default()
        };
        let proposals = vec![dt("a", vec![]), dt("b", vec![])];
        let out = plan_delegation(&store, &parent, &proposals, 0, &[], &[], &limits, now()).expect("plan_delegation");
        assert!(out.accepted.is_empty());
        assert_eq!(out.rejected.len(), 2);
        assert!(out.rejected[0].contains("tree depth would become 4"), "{}", out.rejected[0]);
    }

    #[test]
    fn tree_runs_limit_rejects_everything_and_ignores_reviewer_runs() {
        let store = SqliteStore::open_in_memory().expect("open");
        let root = make_task(None, Status::Running);
        insert(&store, &root);
        let child = make_task(Some(root.id), Status::Running);
        insert(&store, &child);

        // 2 ワーカー run（root と child）+ 1 reviewer run（数えない）。
        store
            .append_event(
                root.id,
                &Event::WorkerStarted {
                    run_id: "r1".into(),
                    adapter: "a".into(),
                    model: "m".into(),
                    provider: None,
                    account: None,
                    role: None,
                    task_role: None,
                },
            )
            .expect("append");
        store
            .append_event(
                child.id,
                &Event::WorkerStarted {
                    run_id: "r2".into(),
                    adapter: "a".into(),
                    model: "m".into(),
                    provider: None,
                    account: None,
                    role: None,
                    task_role: None,
                },
            )
            .expect("append");
        store
            .append_event(
                child.id,
                &Event::WorkerStarted {
                    run_id: "rev1".into(),
                    adapter: "a".into(),
                    model: "m".into(),
                    provider: None,
                    account: None,
                    role: Some(RunRole::Reviewer),
                    task_role: None,
                },
            )
            .expect("append");

        assert_eq!(tree_worker_runs(&store, root.id).expect("count"), 2);

        let limits = DelegationLimits {
            max_tree_runs: 2,
            ..DelegationLimits::default()
        };
        let proposals = vec![dt("a", vec![])];
        let out = plan_delegation(&store, &child, &proposals, 0, &[], &[], &limits, now()).expect("plan_delegation");
        assert!(out.accepted.is_empty());
        assert_eq!(out.rejected.len(), 1);
        assert!(out.rejected[0].contains("tree already has 2 worker runs"), "{}", out.rejected[0]);
    }

    #[test]
    fn rejects_self_and_ancestor_and_missing_dependencies_but_accepts_others() {
        let store = SqliteStore::open_in_memory().expect("open");
        let grandparent = make_task(None, Status::Running);
        insert(&store, &grandparent);
        let parent = make_task(Some(grandparent.id), Status::Running);
        insert(&store, &parent);
        let missing_id = TaskId::new();

        let proposals = vec![
            dt("self-ref", vec![DelegateDep::Id(parent.id.to_string())]),
            dt("ancestor-ref", vec![DelegateDep::Id(grandparent.id.to_string())]),
            dt("missing-ref", vec![DelegateDep::Id(missing_id.to_string())]),
            dt("ok", vec![]),
        ];
        let out = plan_delegation(&store, &parent, &proposals, 0, &[], &[], &DelegationLimits::default(), now())
            .expect("plan_delegation");
        assert_eq!(out.accepted.len(), 1);
        assert_eq!(out.accepted[0].title, "ok");
        assert_eq!(out.rejected.len(), 3);
        assert!(out.rejected[0].contains("is the delegating task itself"), "{}", out.rejected[0]);
        assert!(out.rejected[1].contains("is an ancestor of the delegating task"), "{}", out.rejected[1]);
        assert!(out.rejected[2].contains("does not exist"), "{}", out.rejected[2]);
    }

    #[test]
    fn rejects_dependency_with_failed_status() {
        let store = SqliteStore::open_in_memory().expect("open");
        let parent = make_task(None, Status::Running);
        insert(&store, &parent);
        let failed_dep = make_task(None, Status::Failed);
        insert(&store, &failed_dep);

        let proposals = vec![dt("a", vec![DelegateDep::Id(failed_dep.id.to_string())])];
        let out = plan_delegation(&store, &parent, &proposals, 0, &[], &[], &DelegationLimits::default(), now())
            .expect("plan_delegation");
        assert!(out.accepted.is_empty());
        assert_eq!(out.rejected.len(), 1);
        assert!(out.rejected[0].contains("has status Failed and cannot be depended on"), "{}", out.rejected[0]);
    }

    /// ADR-0039 D2: 委譲した子は **明示 > 案件の workspace > 親** の順で作業場所を決める。
    /// 案件の作業場所は DB（`projects.workspace`）から引く（ストアの読み取りだけ）。
    #[test]
    fn delegated_children_inherit_the_projects_workspace() {
        let store = SqliteStore::open_in_memory().expect("open");
        let now_ts = now();
        let project = task_core::Project {
            id: task_core::ProjectId::new(),
            title: "Pluvio".into(),
            request: "PoC".into(),
            status: task_core::ProjectStatus::Active,
            secretary_summary: None,
            workspace: Some(WorkspaceSpec::Remote {
                cluster: "pegasus".into(),
                path: PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
            }),
            created_at: now_ts,
            updated_at: now_ts,
        };
        store.project_create(&project).expect("project");

        let mut parent = make_task(None, Status::Running);
        parent.project_id = Some(project.id);
        insert(&store, &parent);

        // 1. 何も書かなければ案件の作業場所（親の `/tmp/ws` ではない）。
        let out = plan_delegation(&store, &parent, &[dt("a", vec![])], 0, &[], &[], &DelegationLimits::default(), now())
            .expect("plan_delegation");
        assert_eq!(out.accepted[0].workspace, project.workspace.clone().expect("some"));

        // 2. 子が明示すれば案件より強い。
        let mut explicit = dt("b", vec![]);
        let elsewhere = WorkspaceSpec::Local {
            path: PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
        };
        explicit.workspace = Some(elsewhere.clone());
        let out = plan_delegation(&store, &parent, &[explicit], 0, &[], &[], &DelegationLimits::default(), now())
            .expect("plan_delegation");
        assert_eq!(out.accepted[0].workspace, elsewhere);

        // 3. 案件に属さない親では従来どおり親を継ぐ。
        let orphan = make_task(None, Status::Running);
        insert(&store, &orphan);
        let out = plan_delegation(&store, &orphan, &[dt("c", vec![])], 0, &[], &[], &DelegationLimits::default(), now())
            .expect("plan_delegation");
        assert_eq!(out.accepted[0].workspace, orphan.workspace);
    }

    #[test]
    fn pending_children_does_not_count_terminal_children() {
        let store = SqliteStore::open_in_memory().expect("open");
        let parent = make_task(None, Status::Running);
        insert(&store, &parent);
        let done_child = make_task(Some(parent.id), Status::Done);
        insert(&store, &done_child);
        let running_child = make_task(Some(parent.id), Status::Running);
        insert(&store, &running_child);

        assert_eq!(pending_children(&store, parent.id).expect("pending"), 1);
    }
}
