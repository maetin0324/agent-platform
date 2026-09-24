//! `llm_proxy_requests`（migration 0022。ADR-0053 D1）への記録。**本文は書かない**
//! （id・時刻・source・account・要求/上流モデル・トークン数・レイテンシ・状態・エラー種別だけ）。
//!
//! celeris の `SqliteStore` の migration（`task-core`）が作った同じ DB ファイルに、このクレート専用の
//! 接続で書く（`TaskStore` トレイトには依存しない。責務を分けるため）。

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, params};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// 1 件分の記録（呼び出し側が組む。値そのもの・本文は含めない）。
#[derive(Debug, Clone)]
pub struct RequestLogRow {
    pub id: String,
    /// Unix 秒。
    pub ts: i64,
    /// 選ばれた source の種類（`claude-oauth` / `codex-oauth` / `openai-compatible:<id>`）。選べなかった
    /// ときは `None`。
    pub source: Option<String>,
    pub account: Option<String>,
    pub requested_model: String,
    pub upstream_model: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub latency_ms: u64,
    pub status: &'static str,
    pub error_kind: Option<String>,
}

/// この接続専用の `PRAGMA`（celeris の DB と同じ流儀。ADR-0013 D5）。
pub fn open(db_path: &Path, busy_timeout: Duration) -> Result<Connection, LogError> {
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(busy_timeout)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

pub fn insert(conn: &Connection, row: &RequestLogRow) -> Result<(), LogError> {
    conn.execute(
        "INSERT INTO llm_proxy_requests (
            id, ts, source, account, requested_model, upstream_model,
            prompt_tokens, completion_tokens, latency_ms, status, error_kind
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            row.id,
            row.ts,
            row.source,
            row.account,
            row.requested_model,
            row.upstream_model,
            row.prompt_tokens,
            row.completion_tokens,
            row.latency_ms,
            row.status,
            row.error_kind,
        ],
    )?;
    Ok(())
}

/// 直近 1 時間の要求数・トークン数（`GET /llm/sources` 用。source ごとに集計）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HourlyCounts {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

pub fn hourly_counts_by_source(
    conn: &Connection,
    since: i64,
) -> Result<std::collections::HashMap<String, HourlyCounts>, LogError> {
    let mut stmt = conn.prepare(
        "SELECT source, COUNT(*), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0)
         FROM llm_proxy_requests
         WHERE ts >= ?1 AND source IS NOT NULL
         GROUP BY source",
    )?;
    let mut out = std::collections::HashMap::new();
    let mut rows = stmt.query(params![since])?;
    while let Some(row) = rows.next()? {
        let source: String = row.get(0)?;
        out.insert(
            source,
            HourlyCounts {
                requests: row.get::<_, i64>(1)? as u64,
                prompt_tokens: row.get::<_, i64>(2)? as u64,
                completion_tokens: row.get::<_, i64>(3)? as u64,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_table(conn: &Connection) {
        conn.execute_batch(include_str!(
            "../../task-core/migrations/0022_llm_proxy_requests.sql"
        ))
        .unwrap();
    }

    #[test]
    fn insert_and_read_back_a_row() {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn);
        insert(
            &conn,
            &RequestLogRow {
                id: "req-1".into(),
                ts: 1000,
                source: Some("claude-oauth".into()),
                account: Some("acct-a".into()),
                requested_model: "celeris/cheap".into(),
                upstream_model: Some("claude-haiku-4-5".into()),
                prompt_tokens: Some(10),
                completion_tokens: Some(20),
                latency_ms: 150,
                status: "ok",
                error_kind: None,
            },
        )
        .unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM llm_proxy_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn hourly_counts_group_by_source_and_respect_the_window() {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn);
        for (id, ts, source, p, c) in [
            ("a", 1000, "claude-oauth", 10, 5),
            ("b", 1100, "claude-oauth", 3, 2),
            ("c", 1200, "codex-oauth", 7, 1),
            ("d", 100, "claude-oauth", 999, 999), // too old: excluded
        ] {
            insert(
                &conn,
                &RequestLogRow {
                    id: id.into(),
                    ts,
                    source: Some(source.into()),
                    account: None,
                    requested_model: "celeris/cheap".into(),
                    upstream_model: None,
                    prompt_tokens: Some(p),
                    completion_tokens: Some(c),
                    latency_ms: 1,
                    status: "ok",
                    error_kind: None,
                },
            )
            .unwrap();
        }
        let counts = hourly_counts_by_source(&conn, 500).unwrap();
        assert_eq!(counts["claude-oauth"].requests, 2);
        assert_eq!(counts["claude-oauth"].prompt_tokens, 13);
        assert_eq!(counts["codex-oauth"].requests, 1);
    }
}
