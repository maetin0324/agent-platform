//! 認可（ADR-0033 D5。Phase 26）。SPEC §3.6「少しでも聞くべきだとエージェントが判断したら、あなたに
//! 指示を仰ぐ。あなたはそれに対して『今回だけ』か『同じようなことは今後ずっと』のどちらかの認可を出す。
//! 永続の認可は文字で記録してエージェントに注入する」。
//!
//! - 既存の `Question` 終端（ADR-0010: `Status::Blocked` と `answers[]`）はそのまま残る。ここは、その終端が
//!   起きたときに `approvals` の行を 1 件**追記するだけ**（人が読む・答えるための窓口）。
//! - 人が `once` / `standing` / `denied` のどれで答えても、最終的には**同じ `answers[]` の経路**
//!   （`task_ops::gate::answer`）でタスクを再開する。二重の実装はしない（`task-ops/src/approval.rs` 参照）。
//! - `standing` のときだけ `standing_rules` に 1 行増え、以後の run の前置きに常に注入される
//!   （`crate::approval::StandingRuleStore` を読むのは `task-dispatch`、前置きに描くのは `task-worker::preamble`）。
//! - 自動判定・自動化は今回はやらない（ADR-0033 D5「規則がまだ無い」）。
//!
//! `store.rs` は `TaskStore` の supertrait として `ApprovalStore` を要求するだけで、実装（SQL）はここにある
//! （`report.rs` と同じ形）。

use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension, params, params_from_iter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::model::TaskId;
use crate::org::ProjectId;
use crate::store::{SqliteStore, StoreError, format_rfc3339, parse_rfc3339};

/// 認可 1 件の識別子（ULID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalId(#[schemars(with = "String")] pub Ulid);

impl ApprovalId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for ApprovalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ApprovalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ApprovalId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 永続の規則の識別子（ULID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct StandingRuleId(#[schemars(with = "String")] pub Ulid);

impl StandingRuleId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for StandingRuleId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for StandingRuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for StandingRuleId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 人の決定（SPEC §3.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// 今回だけ。
    Once,
    /// 同じようなことは今後ずっと（`standing_rules` に 1 行増える）。
    Standing,
    /// 認めない。
    Denied,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Once => "once",
            Decision::Standing => "standing",
            Decision::Denied => "denied",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "once" => Some(Decision::Once),
            "standing" => Some(Decision::Standing),
            "denied" => Some(Decision::Denied),
            _ => None,
        }
    }
}

/// 1 件の認可の要求（`approvals` テーブル。ADR-0033 D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Approval {
    pub id: ApprovalId,
    /// 案件（案件に紐づかない質問なら `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// 聞いてきた組織のノード（`task.assignee`、無ければ秘書）。
    pub node_id: String,
    /// きっかけになったタスク（無いことは今回は無いが、`approval_decide` の後も残す前提で任意にしてある）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "time::serde::rfc3339::option")]
    #[schemars(with = "Option<String>")]
    pub decided_at: Option<OffsetDateTime>,
}

impl Approval {
    /// まだ人が答えていない。
    pub fn is_pending(&self) -> bool {
        self.decision.is_none()
    }
}

/// 永続の認可 1 行（`standing_rules` テーブル。ADR-0033 D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StandingRule {
    pub id: StandingRuleId,
    /// `None` = 全員（どのノードの run にも注入される）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub rule: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
}

// ---- `approvals` / `standing_rules` 表の読み書き（SQL はここだけ。`store.rs` は supertrait で要求するだけ）----

/// ADR-0033 D5: `approvals` と `standing_rules` の読み書き。`TaskStore` の supertrait で、実装は
/// `SqliteStore` のみ（ディスパッチャは `Arc<dyn TaskStore>` から呼ぶ）。
pub trait ApprovalStore: Send + Sync {
    /// 1 件追記する（`Question` 終端のたび。ADR-0010 の既存の `answers[]` の仕組みには触れない）。
    fn approval_append(&self, approval: &Approval) -> Result<(), StoreError>;
    fn approval_get(&self, id: ApprovalId) -> Result<Option<Approval>, StoreError>;
    /// 古い順（`created_at` 昇順、同値は `id` 昇順。答える順に並ぶキューとして扱う）。
    ///
    /// `pending`: `Some(true)` = 未決定だけ（`decision IS NULL`）、`Some(false)` = **決定済みだけ**
    /// （`decision IS NOT NULL`。人が決めたものの履歴）、`None` = 絞り込み無し。
    /// GUI からの依頼 R5（Phase 27）で三値にした（以前は `bool` で、`false` が「絞り込み無し」だった）。
    fn approval_list(
        &self,
        pending: Option<bool>,
        project_id: Option<ProjectId>,
        node_id: Option<&str>,
    ) -> Result<Vec<Approval>, StoreError>;
    /// `decision` / `answer` / `decided_at` を書く（既に決まっていても上書きする。人が答え直せるように）。
    /// 無い id は `Ok(None)`。
    fn approval_decide(
        &self,
        id: ApprovalId,
        decision: Decision,
        answer: Option<String>,
        at: OffsetDateTime,
    ) -> Result<Option<Approval>, StoreError>;

    fn standing_rule_append(&self, rule: &StandingRule) -> Result<(), StoreError>;
    /// `node_id = Some(id)` なら「全員向け（`node_id IS NULL`）」+「そのノード向け」、`None` なら絞り込み無し
    /// （全ノード分。GUI の一覧・編集に使う）。古い順。
    fn standing_rule_list(&self, node_id: Option<&str>) -> Result<Vec<StandingRule>, StoreError>;
    /// 無い id は `Ok(false)`。
    fn standing_rule_delete(&self, id: StandingRuleId) -> Result<bool, StoreError>;
}

fn row_to_approval(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Approval, StoreError>> {
    let id: String = row.get(0)?;
    let project_id: Option<String> = row.get(1)?;
    let node_id: String = row.get(2)?;
    let task_id: Option<String> = row.get(3)?;
    let question: String = row.get(4)?;
    let decision_col: Option<String> = row.get(5)?;
    let answer: Option<String> = row.get(6)?;
    let created_at: String = row.get(7)?;
    let decided_at: Option<String> = row.get(8)?;
    let Ok(id) = id.parse::<ApprovalId>() else {
        return Ok(Err(StoreError::Invalid(format!("invalid approval id: {id}"))));
    };
    let project_id = match project_id {
        None => None,
        Some(raw) => match raw.parse::<ProjectId>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(Err(StoreError::Invalid(format!("invalid approval project_id: {raw}")))),
        },
    };
    let task_id = match task_id {
        None => None,
        Some(raw) => match raw.parse::<TaskId>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(Err(StoreError::Invalid(format!("invalid approval task_id: {raw}")))),
        },
    };
    let decision = match decision_col {
        None => None,
        Some(raw) => match Decision::parse(&raw) {
            Some(d) => Some(d),
            None => return Ok(Err(StoreError::Invalid(format!("invalid approval decision: {raw}")))),
        },
    };
    Ok((|| {
        Ok(Approval {
            id,
            project_id,
            node_id,
            task_id,
            question,
            decision,
            answer,
            created_at: parse_rfc3339(&created_at)?,
            decided_at: match decided_at {
                Some(raw) => Some(parse_rfc3339(&raw)?),
                None => None,
            },
        })
    })())
}

fn row_to_standing_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<StandingRule, StoreError>> {
    let id: String = row.get(0)?;
    let node_id: Option<String> = row.get(1)?;
    let rule: String = row.get(2)?;
    let created_at: String = row.get(3)?;
    let Ok(id) = id.parse::<StandingRuleId>() else {
        return Ok(Err(StoreError::Invalid(format!("invalid standing rule id: {id}"))));
    };
    Ok((|| {
        Ok(StandingRule {
            id,
            node_id,
            rule,
            created_at: parse_rfc3339(&created_at)?,
        })
    })())
}

const SELECT_APPROVAL: &str = "SELECT id, project_id, node_id, task_id, question, decision, answer, \
                               created_at, decided_at FROM approvals";
const SELECT_STANDING_RULE: &str = "SELECT id, node_id, rule, created_at FROM standing_rules";

impl ApprovalStore for SqliteStore {
    fn approval_append(&self, approval: &Approval) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approvals (id, project_id, node_id, task_id, question, decision, answer, \
             created_at, decided_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                approval.id.to_string(),
                approval.project_id.map(|p| p.to_string()),
                approval.node_id,
                approval.task_id.map(|t| t.to_string()),
                approval.question,
                approval.decision.map(|d| d.as_str()),
                approval.answer,
                format_rfc3339(approval.created_at)?,
                approval.decided_at.map(format_rfc3339).transpose()?,
            ],
        )?;
        Ok(())
    }

    fn approval_get(&self, id: ApprovalId) -> Result<Option<Approval>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(&format!("{SELECT_APPROVAL} WHERE id = ?1"), params![id.to_string()], row_to_approval)
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn approval_list(
        &self,
        pending: Option<bool>,
        project_id: Option<ProjectId>,
        node_id: Option<&str>,
    ) -> Result<Vec<Approval>, StoreError> {
        let mut where_sql = String::from(" WHERE 1 = 1");
        let mut args: Vec<SqlValue> = Vec::new();
        match pending {
            Some(true) => where_sql.push_str(" AND decision IS NULL"),
            // R5: 「未決定ではない」= 人が決めたものだけ（以前は絞り込み無しになっていた）。
            Some(false) => where_sql.push_str(" AND decision IS NOT NULL"),
            None => {}
        }
        if let Some(project_id) = project_id {
            where_sql.push_str(" AND project_id = ?");
            args.push(SqlValue::Text(project_id.to_string()));
        }
        if let Some(node_id) = node_id {
            where_sql.push_str(" AND node_id = ?");
            args.push(SqlValue::Text(node_id.to_string()));
        }
        let sql = format!("{SELECT_APPROVAL}{where_sql} ORDER BY created_at ASC, id ASC");
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), row_to_approval)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn approval_decide(
        &self,
        id: ApprovalId,
        decision: Decision,
        answer: Option<String>,
        at: OffsetDateTime,
    ) -> Result<Option<Approval>, StoreError> {
        let ts = format_rfc3339(at)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE approvals SET decision = ?2, answer = ?3, decided_at = ?4 WHERE id = ?1",
            params![id.to_string(), decision.as_str(), answer, ts],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let row = conn
            .query_row(&format!("{SELECT_APPROVAL} WHERE id = ?1"), params![id.to_string()], row_to_approval)
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn standing_rule_append(&self, rule: &StandingRule) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO standing_rules (id, node_id, rule, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![rule.id.to_string(), rule.node_id, rule.rule, format_rfc3339(rule.created_at)?],
        )?;
        Ok(())
    }

    fn standing_rule_list(&self, node_id: Option<&str>) -> Result<Vec<StandingRule>, StoreError> {
        let (where_sql, args): (String, Vec<SqlValue>) = match node_id {
            Some(node_id) => (
                " WHERE node_id IS NULL OR node_id = ?".to_string(),
                vec![SqlValue::Text(node_id.to_string())],
            ),
            None => (String::new(), Vec::new()),
        };
        let sql = format!("{SELECT_STANDING_RULE}{where_sql} ORDER BY created_at ASC, id ASC");
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), row_to_standing_rule)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn standing_rule_delete(&self, id: StandingRuleId) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let changed = conn.execute("DELETE FROM standing_rules WHERE id = ?1", params![id.to_string()])?;
        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::{OrgKind, OrgNode};
    use crate::store::TaskStore;

    fn node(id: &str, parent: Option<&str>, kind: OrgKind) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            name: id.to_string(),
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
            node("secretary", None, OrgKind::Secretary),
            node("coding", Some("secretary"), OrgKind::Department),
            node("coding-poc", Some("coding"), OrgKind::Section),
        ] {
            TaskStore::org_upsert(&store, &n).expect("seed org");
        }
        store
    }

    fn sample(node_id: &str, project: Option<ProjectId>, question: &str, at: OffsetDateTime) -> Approval {
        Approval {
            id: ApprovalId::new(),
            project_id: project,
            node_id: node_id.to_string(),
            task_id: Some(TaskId::new()),
            question: question.to_string(),
            decision: None,
            answer: None,
            created_at: at,
            decided_at: None,
        }
    }

    #[test]
    fn decision_has_fixed_spellings() {
        assert_eq!(Decision::Once.as_str(), "once");
        assert_eq!(Decision::parse("standing"), Some(Decision::Standing));
        assert_eq!(Decision::parse("bogus"), None);
    }

    #[test]
    fn append_and_get_round_trip_including_a_pending_row() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let project = ProjectId::new();
        let a = sample("coding-poc", Some(project), "どのクラスタを使いますか", now);
        store.approval_append(&a).expect("append");
        let back = store.approval_get(a.id).expect("get").expect("some");
        assert_eq!(back, a);
        assert!(back.is_pending());
        assert_eq!(store.approval_get(ApprovalId::new()).expect("get"), None);
    }

    #[test]
    fn list_filters_by_pending_project_and_node_oldest_first() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let project = ProjectId::new();
        let other = ProjectId::new();
        let a = sample("coding-poc", Some(project), "a", now - time::Duration::minutes(2));
        let b = sample("coding-poc", Some(project), "b", now - time::Duration::minutes(1));
        let c = sample("secretary", Some(other), "c", now);
        store.approval_append(&a).expect("append");
        store.approval_append(&b).expect("append");
        store.approval_append(&c).expect("append");

        let all = store.approval_list(None, None, None).expect("list");
        assert_eq!(all.iter().map(|x| x.id).collect::<Vec<_>>(), vec![a.id, b.id, c.id], "oldest first");

        let by_project = store.approval_list(None, Some(project), None).expect("list");
        assert_eq!(by_project.len(), 2);
        let by_node = store.approval_list(None, None, Some("secretary")).expect("list");
        assert_eq!(by_node.iter().map(|x| x.id).collect::<Vec<_>>(), vec![c.id]);

        store
            .approval_decide(a.id, Decision::Once, Some("pegasus".into()), now)
            .expect("decide")
            .expect("some");
        let pending = store.approval_list(Some(true), None, None).expect("list");
        assert_eq!(pending.iter().map(|x| x.id).collect::<Vec<_>>(), vec![b.id, c.id]);
        // R5（Phase 27）: `Some(false)` は**決定済みだけ**（以前は絞り込み無しと同じだった）。
        let decided = store.approval_list(Some(false), None, None).expect("list");
        assert_eq!(decided.iter().map(|x| x.id).collect::<Vec<_>>(), vec![a.id]);
        assert!(decided.iter().all(|x| x.decision.is_some()));
        // 絞り込みは他の条件と AND で効く。
        assert!(store.approval_list(Some(false), None, Some("secretary")).expect("list").is_empty());
        assert_eq!(store.approval_list(None, None, None).expect("list").len(), 3);
    }

    #[test]
    fn deciding_writes_the_decision_answer_and_decided_at_and_can_be_redone() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let a = sample("coding-poc", None, "a", now);
        store.approval_append(&a).expect("append");

        let decided = store
            .approval_decide(a.id, Decision::Denied, Some("だめです".into()), now)
            .expect("decide")
            .expect("some");
        assert_eq!(decided.decision, Some(Decision::Denied));
        assert_eq!(decided.answer.as_deref(), Some("だめです"));
        assert_eq!(decided.decided_at, Some(now));
        assert!(!decided.is_pending());

        // 無い id は None（何も書かない）。
        assert_eq!(
            store.approval_decide(ApprovalId::new(), Decision::Once, None, now).expect("decide"),
            None
        );

        // 答え直せる（上書き）。
        let redone = store
            .approval_decide(a.id, Decision::Once, Some("やっぱりいいです".into()), now)
            .expect("decide")
            .expect("some");
        assert_eq!(redone.decision, Some(Decision::Once));
        assert_eq!(redone.answer.as_deref(), Some("やっぱりいいです"));
    }

    #[test]
    fn standing_rules_round_trip_and_list_combines_global_and_node_specific() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let global = StandingRule {
            id: StandingRuleId::new(),
            node_id: None,
            rule: "深夜は連絡しない".into(),
            created_at: now - time::Duration::minutes(2),
        };
        let for_coding = StandingRule {
            id: StandingRuleId::new(),
            node_id: Some("coding-poc".into()),
            rule: "pegasus は 1 ノードで始めてよい".into(),
            created_at: now - time::Duration::minutes(1),
        };
        let for_secretary = StandingRule {
            id: StandingRuleId::new(),
            node_id: Some("secretary".into()),
            rule: "秘書だけの規則".into(),
            created_at: now,
        };
        store.standing_rule_append(&global).expect("append");
        store.standing_rule_append(&for_coding).expect("append");
        store.standing_rule_append(&for_secretary).expect("append");

        // 絞り込み無し = 全件。
        let all = store.standing_rule_list(None).expect("list");
        assert_eq!(all.len(), 3);

        // そのノード向け: 全員向け + そのノードの分だけ（他ノードの分は出ない）。
        let for_coding_view = store.standing_rule_list(Some("coding-poc")).expect("list");
        assert_eq!(
            for_coding_view.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![global.id, for_coding.id],
            "oldest first"
        );

        assert!(store.standing_rule_delete(for_coding.id).expect("delete"));
        assert!(!store.standing_rule_delete(for_coding.id).expect("delete"), "already gone");
        let after = store.standing_rule_list(Some("coding-poc")).expect("list");
        assert_eq!(after.iter().map(|r| r.id).collect::<Vec<_>>(), vec![global.id]);
    }
}
