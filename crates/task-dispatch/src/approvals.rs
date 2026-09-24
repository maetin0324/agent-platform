//! 認可（ADR-0033 D5。Phase 26）。
//!
//! run が `Question` で終わったとき（既存の「人への質問」の流れ — `Trigger::WorkerQuestion` /
//! `Status::Blocked` / `answers[]` はそのまま）に、`approvals` の行を 1 件追記する。**LLM は呼ばない**
//! （DESIGN 原則 1）: 追記する文面は run の自己申告（または部をまたぐ委譲の質問）そのままで、判断は
//! 「誰宛てにするか」（`task.assignee`、無ければ秘書）だけの決定的な処理。

use task_core::approval::{Approval, ApprovalId};
use task_core::{OrgKind, StoreError, Task, TaskStore};
use time::OffsetDateTime;

/// 質問の宛先ノード: `task.assignee`、無ければ秘書（組織に無ければ `None`）。SPEC §3.1 / ADR-0033 D5。
fn question_node_id(store: &dyn TaskStore, task: &Task) -> Result<Option<String>, StoreError> {
    if let Some(assignee) = &task.assignee {
        return Ok(Some(assignee.clone()));
    }
    let org = store.org_list()?;
    Ok(org
        .into_iter()
        .find(|n| n.kind == OrgKind::Secretary)
        .map(|n| n.id))
}

/// `Question` で終わった run を `approvals` に 1 件追記する（組織がまだ無い = 種を蒔いていない DB では
/// 宛先が決められないので何もしない）。
///
/// Phase 27: **同じタスク・同じ文面の未決の行があれば増やさない**（部をまたぐ委譲は run をやり直すたびに
/// 同じ質問が上がるので、認可の一覧が同じ行で埋まらないようにする。決定的な文字列の一致だけで判断する）。
pub(crate) fn record_question_approval(
    store: &dyn TaskStore,
    task: &Task,
    text: &str,
    now: OffsetDateTime,
) -> Result<Option<Approval>, StoreError> {
    let Some(node_id) = question_node_id(store, task)? else {
        return Ok(None);
    };
    if let Some(open) = store
        .approval_list(Some(true), None, Some(&node_id))?
        .into_iter()
        .find(|a| a.task_id == Some(task.id) && a.question == text)
    {
        return Ok(Some(open));
    }
    let approval = Approval {
        id: ApprovalId::new(),
        project_id: task.project_id,
        node_id,
        task_id: Some(task.id),
        question: text.to_string(),
        decision: None,
        answer: None,
        created_at: now,
        decided_at: None,
    };
    store.approval_append(&approval)?;
    Ok(Some(approval))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use task_core::approval::ApprovalStore;
    use task_core::org::{OrgKind as OK, OrgNode};
    use task_core::{
        Budget, ProjectId, SqliteStore, Status, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec,
    };

    fn node(id: &str, parent: Option<&str>, kind: OK) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            profile: Default::default(),
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

    fn task(assignee: Option<&str>, project: Option<ProjectId>) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "調べる".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Running,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Cheap,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from("."),
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
            role: None,
            genre: None,
            aggregate: false,
            project_id: project,
            milestone_id: None,
            assignee: assignee.map(str::to_string),
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    #[test]
    fn a_question_with_an_assignee_goes_straight_to_that_node() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("secretary", None, OrgKind::Secretary))
            .expect("seed");
        store
            .org_upsert(&node("coding-poc", Some("secretary"), OrgKind::Section))
            .expect("seed");
        let project = ProjectId::new();
        let t = task(Some("coding-poc"), Some(project));
        let now = OffsetDateTime::now_utc();

        let approval = record_question_approval(&store, &t, "どのクラスタを使いますか", now)
            .expect("record")
            .expect("some");
        assert_eq!(approval.node_id, "coding-poc");
        assert_eq!(approval.task_id, Some(t.id));
        assert_eq!(approval.project_id, Some(project));
        assert_eq!(approval.question, "どのクラスタを使いますか");
        assert!(approval.is_pending());

        let listed = store.approval_list(Some(true), None, None).expect("list");
        assert_eq!(listed, vec![approval]);
    }

    #[test]
    fn a_question_without_an_assignee_falls_back_to_the_secretary() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("secretary", None, OrgKind::Secretary))
            .expect("seed");
        let t = task(None, None);
        let approval =
            record_question_approval(&store, &t, "続けますか", OffsetDateTime::now_utc())
                .expect("record")
                .expect("some");
        assert_eq!(approval.node_id, "secretary");
    }

    #[test]
    fn without_an_organization_nothing_is_recorded() {
        let store = SqliteStore::open_in_memory().expect("open");
        let t = task(None, None);
        assert_eq!(
            record_question_approval(&store, &t, "続けますか", OffsetDateTime::now_utc())
                .expect("record"),
            None
        );
        assert!(
            store
                .approval_list(None, None, None)
                .expect("list")
                .is_empty()
        );
    }
}
