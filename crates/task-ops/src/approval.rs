//! 認可の決定（ADR-0033 D5。Phase 26）。SPEC §3.6「あなたはそれに対して『今回だけ』か『同じようなことは
//! 今後ずっと』のどちらかの認可を出す。永続の認可は文字で記録してエージェントに注入する」。
//!
//! 人が `approvals` の 1 件に `once` / `standing` / `denied` のどれで答えても、**タスクの再開は既存の
//! 「質問に答える」経路（`gate::answer`）に相乗りする**（二重実装しない）。ここで足すのは:
//! - `denied` は「認めない: <answer>」を答えにする（ワーカーが自分で判断できるように）。
//! - `standing` は上と同じ再開に加えて `standing_rules` に 1 行足す。
//!
//! `Approval` の取得と「無い id は 404」の判定は呼び出し側（`task-api`）が行う（`report.rs` / `reports.rs`
//! と同じ役割分担）。ここは判断と、決まった後の書き込みだけ。

use task_core::TaskStore;
use task_core::approval::{Approval, ApprovalId, Decision, StandingRule, StandingRuleId};
use time::OffsetDateTime;

use crate::error::OpsError;
use crate::gate::{self, TransitionResult};

/// `standing` のときの適用範囲（SPEC §3.6 の「今後ずっと」を誰に適用するか）。要求本文の `scope` に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// そのノードだけ（既定）。
    #[default]
    Node,
    /// 全員（`standing_rules.node_id = NULL`）。
    All,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "node" => Some(Scope::Node),
            "all" => Some(Scope::All),
            _ => None,
        }
    }
}

/// `decide` の結果。
#[derive(Debug, Clone, PartialEq)]
pub struct DecideOutcome {
    pub approval: Approval,
    /// `decision == Standing` のときだけ `Some`。
    pub standing_rule: Option<StandingRule>,
    /// `approval.task_id` があるときだけ `Some`（`gate::answer` がタスクを再開した結果）。
    pub transition: Option<TransitionResult>,
}

/// 人が答える（ADR-0033 D5）。`approval` は呼び出し側が `approval_get` で引いた既存の行
/// （404 の判定は呼び出し側の責任）。
pub fn decide(
    store: &dyn TaskStore,
    approval: Approval,
    decision: Decision,
    answer: String,
    scope: Scope,
    now: OffsetDateTime,
) -> Result<DecideOutcome, OpsError> {
    if answer.trim().is_empty() {
        return Err(OpsError::Validation("answer must not be blank".to_string()));
    }
    let id: ApprovalId = approval.id;
    let decided = store
        .approval_decide(id, decision, Some(answer.clone()), now)?
        .unwrap_or(approval);

    // ADR-0033 D5: `denied` は「認めない: …」を答えにして、ワーカーが自分で判断できるようにする。
    // `once` / `standing` はそのまま答えを渡す。
    let effective_answer = match decision {
        Decision::Denied => format!("認めない: {answer}"),
        Decision::Once | Decision::Standing => answer.clone(),
    };

    // 既存の「質問に答える」経路にそのまま乗せる（`task_id` が無い approval は再開するタスクが無い）。
    let transition = match decided.task_id {
        Some(task_id) => Some(gate::answer(store, task_id, effective_answer, None)?),
        None => None,
    };

    let standing_rule = if decision == Decision::Standing {
        // Phase 27（監査 H-1）: 部をまたぐ委譲の質問だけは、答えの文ではなく**質問の鍵**を規則にする
        // （`delegate` の照合が前方一致でできるように）。それ以外の質問は従来どおり答えの文。
        let rule = crate::conversation::cross_department_key(&decided.question)
            .unwrap_or_else(|| answer.clone());
        let rule = StandingRule {
            id: StandingRuleId::new(),
            node_id: match scope {
                Scope::Node => Some(decided.node_id.clone()),
                Scope::All => None,
            },
            rule,
            created_at: now,
        };
        store.standing_rule_append(&rule)?;
        Some(rule)
    } else {
        None
    };

    Ok(DecideOutcome {
        approval: decided,
        standing_rule,
        transition,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::approval::ApprovalStore;
    use task_core::org::{OrgKind, OrgNode};
    use task_core::{
        Budget, Check, Criterion, SqliteStore, Status, Task, TaskId, TaskKind, Tier, WorkerHint,
        WorkspaceSpec,
    };

    fn node(id: &str, kind: OrgKind) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            profile: Default::default(),
            id: id.into(),
            parent_id: None,
            name: id.into(),
            kind,
            genre: None,
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// `Blocked` のタスクと、それを指す `Approval` を作る（`gate::answer` が再開できる状態）。
    fn blocked_task_with_approval(store: &SqliteStore, node_id: &str) -> (Task, Approval) {
        let now = OffsetDateTime::now_utc();
        let id = TaskId::new();
        let task = Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id,
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Human,
            }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Blocked,
            priority: 0,
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
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: Some(node_id.to_string()),
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        };
        store.create_task(&task, vec![]).expect("create");
        let approval = Approval {
            id: task_core::approval::ApprovalId::new(),
            project_id: None,
            node_id: node_id.to_string(),
            task_id: Some(id),
            question: "どのクラスタを使いますか".into(),
            decision: None,
            answer: None,
            created_at: now,
            decided_at: None,
        };
        store.approval_append(&approval).expect("append");
        (store.get(id).expect("get").expect("task"), approval)
    }

    #[test]
    fn once_answers_the_task_and_leaves_no_standing_rule() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("coding-poc", OrgKind::Secretary))
            .expect("seed");
        let (task, approval) = blocked_task_with_approval(&store, "coding-poc");

        let outcome = decide(
            &store,
            approval.clone(),
            Decision::Once,
            "pegasus".into(),
            Scope::Node,
            OffsetDateTime::now_utc(),
        )
        .expect("decide");
        assert_eq!(outcome.approval.decision, Some(Decision::Once));
        assert_eq!(outcome.approval.answer.as_deref(), Some("pegasus"));
        assert!(outcome.standing_rule.is_none());
        let transition = outcome.transition.expect("transition");
        assert_eq!(
            transition.to,
            Status::Ready,
            "既存の答える経路（blocked → ready）で再開する"
        );
        assert_eq!(
            store.get(task.id).expect("get").expect("task").status,
            Status::Ready
        );
    }

    #[test]
    fn standing_records_a_rule_scoped_to_the_node_by_default() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("coding-poc", OrgKind::Secretary))
            .expect("seed");
        let (_, approval) = blocked_task_with_approval(&store, "coding-poc");

        let outcome = decide(
            &store,
            approval,
            Decision::Standing,
            "pegasus のジョブは 1 ノードで始めてよい".into(),
            Scope::Node,
            OffsetDateTime::now_utc(),
        )
        .expect("decide");
        let rule = outcome.standing_rule.expect("rule");
        assert_eq!(rule.node_id.as_deref(), Some("coding-poc"));
        assert_eq!(rule.rule, "pegasus のジョブは 1 ノードで始めてよい");
        assert_eq!(
            store.standing_rule_list(Some("coding-poc")).expect("list"),
            vec![rule]
        );
    }

    #[test]
    fn standing_with_scope_all_applies_to_everyone() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("coding-poc", OrgKind::Secretary))
            .expect("seed");
        let (_, approval) = blocked_task_with_approval(&store, "coding-poc");

        let outcome = decide(
            &store,
            approval,
            Decision::Standing,
            "深夜は連絡しない".into(),
            Scope::All,
            OffsetDateTime::now_utc(),
        )
        .expect("decide");
        assert_eq!(outcome.standing_rule.expect("rule").node_id, None);
        // 全員向けなので他ノードの一覧にも出る。
        assert_eq!(
            store
                .standing_rule_list(Some("someone-else"))
                .expect("list")
                .len(),
            1
        );
    }

    #[test]
    fn denied_prefixes_the_answer_so_the_worker_can_tell() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("coding-poc", OrgKind::Secretary))
            .expect("seed");
        let (task, approval) = blocked_task_with_approval(&store, "coding-poc");

        let outcome = decide(
            &store,
            approval,
            Decision::Denied,
            "予算超過".into(),
            Scope::Node,
            OffsetDateTime::now_utc(),
        )
        .expect("decide");
        assert_eq!(outcome.approval.decision, Some(Decision::Denied));
        assert!(outcome.standing_rule.is_none());
        let events = store.events_for(task.id).expect("events");
        let answered = events.iter().rev().find_map(|(_, e)| match e {
            task_core::Event::Answered { answer, .. } => Some(answer.clone()),
            _ => None,
        });
        assert_eq!(answered.as_deref(), Some("認めない: 予算超過"));
    }

    #[test]
    fn a_blank_answer_is_rejected_without_writing_anything() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("coding-poc", OrgKind::Secretary))
            .expect("seed");
        let (_, approval) = blocked_task_with_approval(&store, "coding-poc");
        assert!(matches!(
            decide(
                &store,
                approval.clone(),
                Decision::Once,
                "   ".into(),
                Scope::Node,
                OffsetDateTime::now_utc()
            ),
            Err(OpsError::Validation(_))
        ));
        assert!(
            store
                .approval_get(approval.id)
                .expect("get")
                .expect("some")
                .is_pending()
        );
    }

    #[test]
    fn an_approval_without_a_task_id_only_records_the_decision() {
        let store = SqliteStore::open_in_memory().expect("open");
        store
            .org_upsert(&node("secretary", OrgKind::Secretary))
            .expect("seed");
        let approval = Approval {
            id: task_core::approval::ApprovalId::new(),
            project_id: None,
            node_id: "secretary".into(),
            task_id: None,
            question: "クラスタの障害、続報を待ちますか".into(),
            decision: None,
            answer: None,
            created_at: OffsetDateTime::now_utc(),
            decided_at: None,
        };
        store.approval_append(&approval).expect("append");
        let outcome = decide(
            &store,
            approval,
            Decision::Once,
            "待つ".into(),
            Scope::Node,
            OffsetDateTime::now_utc(),
        )
        .expect("decide");
        assert!(outcome.transition.is_none());
        assert_eq!(outcome.approval.answer.as_deref(), Some("待つ"));
    }
}
