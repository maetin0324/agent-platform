//! ADR-0056 D1 / D4（Phase 78）: 外部エージェントが Celeris を操作する MCP サーバーの認証とログ。
//!
//! ここは**純粋なデータ定義と SQL だけ**（JSON-RPC のディスパッチ・スコープの適用・レート制限は
//! `crates/celeris-mcp` の責務。DESIGN 原則 1 / ADR-0001 D2: このクレートは LLM 呼び出しも
//! サブプロセス起動もしない）。
//!
//! - `mcp_clients`: クライアントごとの Bearer トークン。**値そのものは持たない**（`token_hash` は
//!   SHA-256 の 16 進文字列。発行は `celerisctl mcp client add`）。
//! - `mcp_calls`: `tools/call` の監査ログ（ADR-0056 D4: 引数と結果の本文は残さない）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::store::{SqliteStore, StoreError, format_rfc3339, parse_rfc3339};

/// ADR-0056 D4: MCP クライアントに許すスコープ。JSON では `as_str()` の綴り（`"knowledge:read"` 等。
/// `:` を含むので `#[serde(rename_all = "snake_case")]` は使えず、手で実装する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpScope {
    KnowledgeRead,
    KnowledgePropose,
    TasksRead,
    /// Phase 101: `task_comment` / `task_answer`（`POST /tasks/{id}/comments` / `/answer` と同じ）。
    TasksInteract,
    /// Phase 101: `task_retry` / `task_cancel`（`POST /tasks/{id}/retry` / `/cancel` と同じ）。
    TasksControl,
    /// Phase 101: `task_approve` / `task_reject`（`POST /tasks/{id}/approve` / `/reject` と同じ）。
    TasksDecide,
    ConsoleInstruct,
    OrgRead,
    OrgWrite,
    SkillsRead,
    SkillsWrite,
}

impl McpScope {
    pub fn as_str(self) -> &'static str {
        match self {
            McpScope::KnowledgeRead => "knowledge:read",
            McpScope::KnowledgePropose => "knowledge:propose",
            McpScope::TasksRead => "tasks:read",
            McpScope::TasksInteract => "tasks:interact",
            McpScope::TasksControl => "tasks:control",
            McpScope::TasksDecide => "tasks:decide",
            McpScope::ConsoleInstruct => "console:instruct",
            McpScope::OrgRead => "org:read",
            McpScope::OrgWrite => "org:write",
            McpScope::SkillsRead => "skills:read",
            McpScope::SkillsWrite => "skills:write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "knowledge:read" => Some(McpScope::KnowledgeRead),
            "knowledge:propose" => Some(McpScope::KnowledgePropose),
            "tasks:read" => Some(McpScope::TasksRead),
            "tasks:interact" => Some(McpScope::TasksInteract),
            "tasks:control" => Some(McpScope::TasksControl),
            "tasks:decide" => Some(McpScope::TasksDecide),
            "console:instruct" => Some(McpScope::ConsoleInstruct),
            "org:read" => Some(McpScope::OrgRead),
            "org:write" => Some(McpScope::OrgWrite),
            "skills:read" => Some(McpScope::SkillsRead),
            "skills:write" => Some(McpScope::SkillsWrite),
            _ => None,
        }
    }

    /// ADR-0056 D4: `celerisctl mcp client add` の既定スコープ（read 系 + `knowledge:propose` +
    /// `console:instruct`。`org:write` / `skills:write` は明示が要る）。
    pub const DEFAULT: [McpScope; 5] = [
        McpScope::KnowledgeRead,
        McpScope::KnowledgePropose,
        McpScope::TasksRead,
        McpScope::ConsoleInstruct,
        McpScope::OrgRead,
    ];
}

impl Serialize for McpScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for McpScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        McpScope::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown mcp scope {raw:?}")))
    }
}

impl schemars::JsonSchema for McpScope {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "McpScope".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": [
                "knowledge:read", "knowledge:propose", "tasks:read",
                "tasks:interact", "tasks:control", "tasks:decide", "console:instruct",
                "org:read", "org:write", "skills:read", "skills:write",
            ],
        })
    }
}

/// スコープの集合をカンマ区切りの文字列にする（DB の `mcp_clients.scopes`）。
pub fn scopes_to_string(scopes: &[McpScope]) -> String {
    scopes
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// カンマ区切りの文字列からスコープの集合を読む。知らない語は無視する（前方互換）。
pub fn scopes_from_string(raw: &str) -> Vec<McpScope> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(McpScope::parse)
        .collect()
}

/// `mcp_clients` の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct McpClient {
    pub id: String,
    pub name: String,
    /// トークンの SHA-256（16 進）。**値そのものは持たない**。`None` は `--no-token` で作った客
    /// （ADR-0056 D1: `auth = "none"` の口に `client = "<id>"` で固定する専用。`token` の口では
    /// 絶対に一致しない）。
    #[serde(skip_serializing)]
    pub token_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<McpScope>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::node_session::opt_rfc3339")]
    #[schemars(with = "Option<String>")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::node_session::opt_rfc3339")]
    #[schemars(with = "Option<String>")]
    pub revoked_at: Option<OffsetDateTime>,
}

impl McpClient {
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    pub fn has_scope(&self, scope: McpScope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// `mcp_calls` の 1 行（ADR-0056 D4: 引数と結果の本文は持たない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct McpCall {
    pub id: String,
    pub client_id: String,
    pub tool: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    pub latency_ms: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub at: OffsetDateTime,
}

/// MCP クライアント表の永続状態（`SqliteStore` が実装する）。
pub trait McpClientStore: Send + Sync {
    fn mcp_client_create(&self, client: &McpClient) -> Result<(), StoreError>;
    fn mcp_client_get(&self, id: &str) -> Result<Option<McpClient>, StoreError>;
    fn mcp_client_by_token_hash(&self, token_hash: &str) -> Result<Option<McpClient>, StoreError>;
    /// `created_at` の昇順。
    fn mcp_client_list(&self) -> Result<Vec<McpClient>, StoreError>;
    /// 無ければ `false`。
    fn mcp_client_revoke(&self, id: &str, now: OffsetDateTime) -> Result<bool, StoreError>;
    fn mcp_client_touch_last_used(&self, id: &str, now: OffsetDateTime)
    -> Result<bool, StoreError>;
}

/// MCP 呼び出しログの永続状態（`SqliteStore` が実装する）。
pub trait McpCallStore: Send + Sync {
    fn mcp_call_record(&self, call: &McpCall) -> Result<(), StoreError>;
    /// `client_id` が `Some` ならその客だけ、`None` なら全客。新しい順に最大 `limit` 件。
    fn mcp_calls_list(
        &self,
        client_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<McpCall>, StoreError>;
}

impl McpClientStore for SqliteStore {
    fn mcp_client_create(&self, client: &McpClient) -> Result<(), StoreError> {
        let created_at = format_rfc3339(client.created_at)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO mcp_clients (id, name, token_hash, scopes, created_at, last_used_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL)",
            rusqlite::params![
                client.id,
                client.name,
                client.token_hash,
                scopes_to_string(&client.scopes),
                created_at,
            ],
        )?;
        Ok(())
    }

    fn mcp_client_get(&self, id: &str) -> Result<Option<McpClient>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!("{SELECT_MCP_CLIENT} WHERE id = ?1"),
                [id],
                row_to_mcp_client,
            )
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn mcp_client_by_token_hash(&self, token_hash: &str) -> Result<Option<McpClient>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!("{SELECT_MCP_CLIENT} WHERE token_hash = ?1"),
                [token_hash],
                row_to_mcp_client,
            )
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn mcp_client_list(&self) -> Result<Vec<McpClient>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "{SELECT_MCP_CLIENT} ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], row_to_mcp_client)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn mcp_client_revoke(&self, id: &str, now: OffsetDateTime) -> Result<bool, StoreError> {
        let ts = format_rfc3339(now)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE mcp_clients SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            rusqlite::params![id, ts],
        )?;
        Ok(changed > 0)
    }

    fn mcp_client_touch_last_used(
        &self,
        id: &str,
        now: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let ts = format_rfc3339(now)?;
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE mcp_clients SET last_used_at = ?2 WHERE id = ?1",
            rusqlite::params![id, ts],
        )?;
        Ok(changed > 0)
    }
}

const SELECT_MCP_CLIENT: &str =
    "SELECT id, name, token_hash, scopes, created_at, last_used_at, revoked_at FROM mcp_clients";

fn row_to_mcp_client(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<McpClient, StoreError>> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let token_hash: Option<String> = row.get(2)?;
    let scopes_col: String = row.get(3)?;
    let created_at: String = row.get(4)?;
    let last_used_at: Option<String> = row.get(5)?;
    let revoked_at: Option<String> = row.get(6)?;

    let created_at = match parse_rfc3339(&created_at) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let last_used_at = match last_used_at.map(|s| parse_rfc3339(&s)).transpose() {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let revoked_at = match revoked_at.map(|s| parse_rfc3339(&s)).transpose() {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    Ok(Ok(McpClient {
        id,
        name,
        token_hash,
        scopes: scopes_from_string(&scopes_col),
        created_at,
        last_used_at,
        revoked_at,
    }))
}

impl McpCallStore for SqliteStore {
    fn mcp_call_record(&self, call: &McpCall) -> Result<(), StoreError> {
        let at = format_rfc3339(call.at)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO mcp_calls (id, client_id, tool, ok, error_kind, latency_ms, at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                call.id,
                call.client_id,
                call.tool,
                call.ok,
                call.error_kind,
                call.latency_ms,
                at,
            ],
        )?;
        Ok(())
    }

    fn mcp_calls_list(
        &self,
        client_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<McpCall>, StoreError> {
        let conn = self.lock()?;
        let sql = "SELECT id, client_id, tool, ok, error_kind, latency_ms, at FROM mcp_calls \
             WHERE client_id IS ?1 OR ?1 IS NULL \
             ORDER BY at DESC, id DESC LIMIT ?2";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params![client_id, limit as i64], row_to_mcp_call)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }
}

fn row_to_mcp_call(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<McpCall, StoreError>> {
    let id: String = row.get(0)?;
    let client_id: String = row.get(1)?;
    let tool: String = row.get(2)?;
    let ok: bool = row.get(3)?;
    let error_kind: Option<String> = row.get(4)?;
    let latency_ms: i64 = row.get(5)?;
    let at: String = row.get(6)?;
    let at = match parse_rfc3339(&at) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    Ok(Ok(McpCall {
        id,
        client_id,
        tool,
        ok,
        error_kind,
        latency_ms,
        at,
    }))
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().expect("open")
    }

    #[test]
    fn scope_as_str_and_parse_round_trip() {
        for scope in [
            McpScope::KnowledgeRead,
            McpScope::KnowledgePropose,
            McpScope::TasksRead,
            McpScope::TasksInteract,
            McpScope::TasksControl,
            McpScope::TasksDecide,
            McpScope::ConsoleInstruct,
            McpScope::OrgRead,
            McpScope::OrgWrite,
            McpScope::SkillsRead,
            McpScope::SkillsWrite,
        ] {
            assert_eq!(McpScope::parse(scope.as_str()), Some(scope));
        }
        assert_eq!(McpScope::parse("bogus"), None);
    }

    #[test]
    fn scopes_string_round_trips_and_ignores_unknown_words() {
        let scopes = vec![McpScope::KnowledgeRead, McpScope::OrgWrite];
        let raw = scopes_to_string(&scopes);
        assert_eq!(raw, "knowledge:read,org:write");
        assert_eq!(scopes_from_string(&raw), scopes);
        assert_eq!(
            scopes_from_string("knowledge:read, bogus ,org:write"),
            scopes
        );
    }

    #[test]
    fn client_create_get_by_token_hash_and_list_round_trip() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        let client = McpClient {
            id: "c1".into(),
            name: "chatgpt".into(),
            token_hash: Some("deadbeef".into()),
            scopes: vec![McpScope::KnowledgeRead, McpScope::ConsoleInstruct],
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        store.mcp_client_create(&client).expect("create");
        assert_eq!(
            store.mcp_client_get("c1").expect("get"),
            Some(client.clone())
        );
        assert_eq!(
            store
                .mcp_client_by_token_hash("deadbeef")
                .expect("by token"),
            Some(client.clone())
        );
        assert_eq!(
            store
                .mcp_client_by_token_hash("nope")
                .expect("by token none"),
            None
        );
        assert_eq!(store.mcp_client_list().expect("list"), vec![client]);
    }

    #[test]
    fn touch_last_used_and_revoke() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        let client = McpClient {
            id: "c1".into(),
            name: "chatgpt".into(),
            token_hash: Some("deadbeef".into()),
            scopes: vec![],
            created_at: now,
            last_used_at: None,
            revoked_at: None,
        };
        store.mcp_client_create(&client).expect("create");
        assert!(
            store
                .mcp_client_touch_last_used("c1", now + time::Duration::seconds(1))
                .expect("touch")
        );
        let got = store.mcp_client_get("c1").expect("get").expect("some");
        assert!(got.last_used_at.is_some());
        assert!(!got.is_revoked());

        assert!(
            store
                .mcp_client_revoke("c1", now + time::Duration::seconds(2))
                .expect("revoke")
        );
        let got = store.mcp_client_get("c1").expect("get").expect("some");
        assert!(got.is_revoked());
        // 2 回目の revoke は何もしない。
        assert!(
            !store
                .mcp_client_revoke("c1", now + time::Duration::seconds(3))
                .expect("revoke again")
        );
        // 無いクライアントは false。
        assert!(
            !store
                .mcp_client_touch_last_used("nope", now)
                .expect("touch nope")
        );
        assert!(!store.mcp_client_revoke("nope", now).expect("revoke nope"));
    }

    #[test]
    fn calls_are_recorded_and_listed_newest_first_optionally_filtered_by_client() {
        let store = store();
        let now = OffsetDateTime::now_utc();
        for (i, client_id) in ["a", "b", "a"].iter().enumerate() {
            store
                .mcp_call_record(&McpCall {
                    id: format!("call-{i}"),
                    client_id: client_id.to_string(),
                    tool: "knowledge_search".into(),
                    ok: true,
                    error_kind: None,
                    latency_ms: 10,
                    at: now + time::Duration::seconds(i as i64),
                })
                .expect("record");
        }
        let all = store.mcp_calls_list(None, 100).expect("list all");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "call-2"); // newest first
        let a_only = store.mcp_calls_list(Some("a"), 100).expect("list a");
        assert_eq!(a_only.len(), 2);
        assert!(a_only.iter().all(|c| c.client_id == "a"));
        let limited = store.mcp_calls_list(None, 1).expect("limit 1");
        assert_eq!(limited.len(), 1);
    }
}
