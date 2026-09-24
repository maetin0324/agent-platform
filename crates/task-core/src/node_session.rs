//! ノードごとの継続セッション（ADR-0054 D1。Phase 67）。
//!
//! ここは**純粋なデータ定義と SQL だけ**（アダプタごとの resume の作法・トークンの逼迫判定・
//! アカウント変更の検出は `task-dispatch` の責務。DESIGN 原則 1 / ADR-0001 D2: このクレートは
//! LLM 呼び出しもサブプロセス起動もしない）。
//!
//! - CoS の対話は全体で 1 本（`node_id = "cos"`, `kind = Conversation`, `project_id = None` 固定）。
//! - 部署の根ノード自身のレビュー・切り分け run（ADR-0051）は部署ごとに 1 本（`kind = Lead`）。
//! - 「有効なセッション」は `retired_at IS NULL` の行が高々 1 件、という不変条件をアプリケーション側
//!   （`task-dispatch`）が `node_session_retire` → `node_session_create` の順で呼ぶことで保つ。
//!   ストア自身は複数の有効な行があっても壊れない（`node_session_active` は最新の 1 件を返す）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::org::ProjectId;
use crate::store::{SqliteStore, StoreError, format_rfc3339, parse_rfc3339};

/// `node_sessions.kind`（ADR-0054 D1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// CoS の対話（`node_id = "cos"` 固定。全体で 1 本）。
    Conversation,
    /// 部署の根ノード自身のレビュー・切り分け run（ADR-0051）。部署ごとに 1 本。
    Lead,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::Conversation => "conversation",
            SessionKind::Lead => "lead",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "conversation" => Some(SessionKind::Conversation),
            "lead" => Some(SessionKind::Lead),
            _ => None,
        }
    }
}

/// `node_sessions` の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NodeSession {
    pub id: String,
    pub node_id: String,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// アダプタに渡す／アダプタから返る継続用の id。celeris が前もって決める（claude-code）ときは
    /// 非空、アダプタが決める（codex / acp）ときは確定するまで空文字。
    pub session_id: String,
    pub turns: i64,
    pub approx_tokens: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub last_used_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::node_session::opt_rfc3339")]
    #[schemars(with = "Option<String>")]
    pub retired_at: Option<OffsetDateTime>,
}

impl NodeSession {
    /// 新しいセッションの行（`node_session_create` に渡す前の組み立て）。
    pub fn new(
        node_id: impl Into<String>,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        adapter: impl Into<String>,
        account_id: Option<String>,
        session_id: impl Into<String>,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            node_id: node_id.into(),
            kind,
            project_id,
            adapter: adapter.into(),
            account_id,
            session_id: session_id.into(),
            turns: 0,
            approx_tokens: 0,
            created_at: now,
            last_used_at: now,
            retired_at: None,
        }
    }
}

/// ADR-0056（Phase 78）: `crate::mcp` も同じ `Option<OffsetDateTime>` の RFC 3339 往復を要るため、
/// crate 内に公開した（意味は「RFC 3339 の `Option`」というだけで `node_session` 固有ではない）。
pub(crate) mod opt_rfc3339 {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    pub fn serialize<S: Serializer>(
        value: &Option<OffsetDateTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(t) => {
                let s = t
                    .format(&Rfc3339)
                    .map_err(|e| serde::ser::Error::custom(e.to_string()))?;
                serializer.serialize_some(&s)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<OffsetDateTime>, D::Error> {
        let raw: Option<String> = Option::deserialize(deserializer)?;
        match raw {
            Some(s) => OffsetDateTime::parse(&s, &Rfc3339)
                .map(Some)
                .map_err(|e| serde::de::Error::custom(e.to_string())),
            None => Ok(None),
        }
    }
}

/// ノードごとの継続セッションの永続状態（`SqliteStore` が実装する）。
pub trait NodeSessionStore: Send + Sync {
    /// `(node_id, kind, project_id)` の**有効な**（`retired_at IS NULL`）セッション。無ければ `None`。
    /// 複数あっても壊れない: `last_used_at` が最新の 1 件を返す。
    fn node_session_active(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
    ) -> Result<Option<NodeSession>, StoreError>;
    /// 新しいセッションを作る。呼び出し側が先に [`NodeSessionStore::node_session_retire`] で
    /// 既存の有効なセッションを引退させておくこと（このメソッド自身は既存行を触らない）。
    fn node_session_create(&self, session: &NodeSession) -> Result<(), StoreError>;
    /// `(node_id, kind, project_id)` の**有効な**セッションがあれば `retired_at = now` にする。
    /// 無ければ何もしない（`false` を返す）。
    fn node_session_retire(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        now: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// run を 1 回終えるたびに、その run の usage が分かった時点で呼ぶ: `turns += 1`、
    /// `approx_tokens += add_tokens`、`last_used_at = now`。有効なセッションが無ければ何もしない（`false`）。
    fn node_session_touch(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        add_tokens: i64,
        now: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// `session_id` だけを上書きする（`turns`/`approx_tokens`/`last_used_at` は変えない）。
    /// codex / acp のように、アダプタが run の途中で初めて確定させる id を、run 完了前の早い時点
    /// （`EventSink::session_established`）で確定させるために使う。有効なセッションが無ければ `false`。
    fn node_session_set_id(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        session_id: &str,
    ) -> Result<bool, StoreError>;
}

const SELECT_NODE_SESSION: &str = "SELECT id, node_id, kind, project_id, adapter, account_id, \
     session_id, turns, approx_tokens, created_at, last_used_at, retired_at FROM node_sessions";

fn row_to_node_session(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<NodeSession, StoreError>> {
    let id: String = row.get(0)?;
    let node_id: String = row.get(1)?;
    let kind_col: String = row.get(2)?;
    let project_id: Option<String> = row.get(3)?;
    let adapter: String = row.get(4)?;
    let account_id: Option<String> = row.get(5)?;
    let session_id: String = row.get(6)?;
    let turns: i64 = row.get(7)?;
    let approx_tokens: i64 = row.get(8)?;
    let created_at: String = row.get(9)?;
    let last_used_at: String = row.get(10)?;
    let retired_at: Option<String> = row.get(11)?;

    let Some(kind) = SessionKind::parse(&kind_col) else {
        return Ok(Err(StoreError::Invalid(format!(
            "invalid node_sessions.kind: {kind_col}"
        ))));
    };
    let project_id = match project_id {
        Some(s) => match s.parse::<ProjectId>() {
            Ok(p) => Some(p),
            Err(_) => {
                return Ok(Err(StoreError::Invalid(format!(
                    "invalid node_sessions.project_id: {s}"
                ))));
            }
        },
        None => None,
    };
    let created_at = match parse_rfc3339(&created_at) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let last_used_at = match parse_rfc3339(&last_used_at) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let retired_at = match retired_at.map(|s| parse_rfc3339(&s)).transpose() {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    Ok(Ok(NodeSession {
        id,
        node_id,
        kind,
        project_id,
        adapter,
        account_id,
        session_id,
        turns,
        approx_tokens,
        created_at,
        last_used_at,
        retired_at,
    }))
}

impl NodeSessionStore for SqliteStore {
    fn node_session_active(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
    ) -> Result<Option<NodeSession>, StoreError> {
        let conn = self.lock()?;
        let sql = format!(
            "{SELECT_NODE_SESSION} WHERE node_id = ?1 AND kind = ?2 AND retired_at IS NULL \
             AND project_id IS ?3 ORDER BY last_used_at DESC, id DESC LIMIT 1"
        );
        let row = conn
            .query_row(
                &sql,
                rusqlite::params![node_id, kind.as_str(), project_id.map(|p| p.to_string())],
                row_to_node_session,
            )
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn node_session_create(&self, session: &NodeSession) -> Result<(), StoreError> {
        let created_at = format_rfc3339(session.created_at)?;
        let last_used_at = format_rfc3339(session.last_used_at)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO node_sessions (id, node_id, kind, project_id, adapter, account_id, \
             session_id, turns, approx_tokens, created_at, last_used_at, retired_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
            rusqlite::params![
                session.id,
                session.node_id,
                session.kind.as_str(),
                session.project_id.map(|p| p.to_string()),
                session.adapter,
                session.account_id,
                session.session_id,
                session.turns,
                session.approx_tokens,
                created_at,
                last_used_at,
            ],
        )?;
        Ok(())
    }

    fn node_session_retire(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        now: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let ts = format_rfc3339(now)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE node_sessions SET retired_at = ?4 \
             WHERE node_id = ?1 AND kind = ?2 AND project_id IS ?3 AND retired_at IS NULL",
            rusqlite::params![
                node_id,
                kind.as_str(),
                project_id.map(|p| p.to_string()),
                ts
            ],
        )?;
        Ok(changed > 0)
    }

    fn node_session_touch(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        add_tokens: i64,
        now: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let ts = format_rfc3339(now)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE node_sessions SET turns = turns + 1, approx_tokens = approx_tokens + ?5, \
             last_used_at = ?4 \
             WHERE node_id = ?1 AND kind = ?2 AND project_id IS ?3 AND retired_at IS NULL",
            rusqlite::params![
                node_id,
                kind.as_str(),
                project_id.map(|p| p.to_string()),
                ts,
                add_tokens,
            ],
        )?;
        Ok(changed > 0)
    }

    fn node_session_set_id(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        session_id: &str,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE node_sessions SET session_id = ?4 \
             WHERE node_id = ?1 AND kind = ?2 AND project_id IS ?3 AND retired_at IS NULL",
            rusqlite::params![
                node_id,
                kind.as_str(),
                project_id.map(|p| p.to_string()),
                session_id
            ],
        )?;
        Ok(changed > 0)
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().expect("open")
    }

    #[test]
    fn create_then_active_round_trips() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        assert_eq!(
            store
                .node_session_active("cos", SessionKind::Conversation, None)
                .expect("active"),
            None
        );
        let session = NodeSession::new(
            "cos",
            SessionKind::Conversation,
            None,
            "claude-code",
            Some("acct-a".to_string()),
            "sess-1",
            now,
        );
        store.node_session_create(&session).expect("create");
        let active = store
            .node_session_active("cos", SessionKind::Conversation, None)
            .expect("active")
            .expect("some");
        assert_eq!(active.session_id, "sess-1");
        assert_eq!(active.account_id.as_deref(), Some("acct-a"));
        assert_eq!(active.turns, 0);
        assert_eq!(active.approx_tokens, 0);
        assert!(active.retired_at.is_none());
    }

    #[test]
    fn touch_bumps_turns_and_tokens_and_set_id_updates_the_session_id_only() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        let session = NodeSession::new(
            "cos",
            SessionKind::Conversation,
            None,
            "codex",
            None,
            "", // codex: 確定するまで空。
            now,
        );
        store.node_session_create(&session).expect("create");

        // `EventSink::session_established` 相当: run の途中で id が確定する（turns/tokens は動かない）。
        let set = store
            .node_session_set_id("cos", SessionKind::Conversation, None, "thread-abc")
            .expect("set_id");
        assert!(set);
        let active = store
            .node_session_active("cos", SessionKind::Conversation, None)
            .expect("active")
            .expect("some");
        assert_eq!(active.session_id, "thread-abc");
        assert_eq!(active.turns, 0);
        assert_eq!(active.approx_tokens, 0);

        // run 完了後、usage が分かった時点で touch する。
        let touched = store
            .node_session_touch(
                "cos",
                SessionKind::Conversation,
                None,
                1234,
                now + time::Duration::seconds(5),
            )
            .expect("touch");
        assert!(touched);

        let active = store
            .node_session_active("cos", SessionKind::Conversation, None)
            .expect("active")
            .expect("some");
        assert_eq!(active.turns, 1);
        assert_eq!(active.approx_tokens, 1234);
        assert_eq!(active.session_id, "thread-abc");

        // 2 回目の touch: 累積だけ増える。
        store
            .node_session_touch(
                "cos",
                SessionKind::Conversation,
                None,
                100,
                now + time::Duration::seconds(10),
            )
            .expect("touch 2");
        let active = store
            .node_session_active("cos", SessionKind::Conversation, None)
            .expect("active")
            .expect("some");
        assert_eq!(active.turns, 2);
        assert_eq!(active.approx_tokens, 1334);
        assert_eq!(active.session_id, "thread-abc");
    }

    #[test]
    fn set_id_and_touch_are_no_ops_when_there_is_no_active_session() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        assert!(
            !store
                .node_session_set_id("cos", SessionKind::Conversation, None, "x")
                .expect("set_id")
        );
        assert!(
            !store
                .node_session_touch("cos", SessionKind::Conversation, None, 10, now)
                .expect("touch")
        );
    }

    #[test]
    fn retire_then_create_replaces_the_active_session() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        let first = NodeSession::new(
            "engineering",
            SessionKind::Lead,
            None,
            "claude-code",
            Some("acct-a".to_string()),
            "sess-1",
            now,
        );
        store.node_session_create(&first).expect("create 1");
        let retired = store
            .node_session_retire(
                "engineering",
                SessionKind::Lead,
                None,
                now + time::Duration::seconds(1),
            )
            .expect("retire");
        assert!(retired);
        // 2 回目の retire は何もしない（もう有効な行が無い）。
        assert!(
            !store
                .node_session_retire(
                    "engineering",
                    SessionKind::Lead,
                    None,
                    now + time::Duration::seconds(2)
                )
                .expect("retire again")
        );
        assert_eq!(
            store
                .node_session_active("engineering", SessionKind::Lead, None)
                .expect("active"),
            None
        );

        let second = NodeSession::new(
            "engineering",
            SessionKind::Lead,
            None,
            "claude-code",
            Some("acct-b".to_string()),
            "sess-2",
            now + time::Duration::seconds(2),
        );
        store.node_session_create(&second).expect("create 2");
        let active = store
            .node_session_active("engineering", SessionKind::Lead, None)
            .expect("active")
            .expect("some");
        assert_eq!(active.session_id, "sess-2");
        assert_eq!(active.account_id.as_deref(), Some("acct-b"));
    }

    #[test]
    fn conversation_and_lead_kinds_and_different_nodes_are_independent() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        store
            .node_session_create(&NodeSession::new(
                "cos",
                SessionKind::Conversation,
                None,
                "claude-code",
                None,
                "cos-sess",
                now,
            ))
            .expect("create cos");
        store
            .node_session_create(&NodeSession::new(
                "engineering",
                SessionKind::Lead,
                None,
                "claude-code",
                None,
                "eng-sess",
                now,
            ))
            .expect("create eng");
        store
            .node_session_create(&NodeSession::new(
                "research",
                SessionKind::Lead,
                None,
                "codex",
                None,
                "res-sess",
                now,
            ))
            .expect("create res");

        assert_eq!(
            store
                .node_session_active("cos", SessionKind::Conversation, None)
                .expect("active")
                .expect("some")
                .session_id,
            "cos-sess"
        );
        assert_eq!(
            store
                .node_session_active("engineering", SessionKind::Lead, None)
                .expect("active")
                .expect("some")
                .session_id,
            "eng-sess"
        );
        assert_eq!(
            store
                .node_session_active("research", SessionKind::Lead, None)
                .expect("active")
                .expect("some")
                .session_id,
            "res-sess"
        );
        // 部署が無いノードの Lead セッションを引いても None（他の部署の行に混ざらない）。
        assert_eq!(
            store
                .node_session_active("ops", SessionKind::Lead, None)
                .expect("active"),
            None
        );
    }

    #[test]
    fn kind_as_str_and_parse_round_trip() {
        for kind in [SessionKind::Conversation, SessionKind::Lead] {
            assert_eq!(SessionKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(SessionKind::parse("bogus"), None);
    }
}
