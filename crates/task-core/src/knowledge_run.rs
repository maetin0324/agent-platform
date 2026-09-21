//! 知識整理 run の追跡（ADR-0047 D4。Phase 62）。
//!
//! ここは**純粋なデータ定義と SQL だけ**（判断は `celeris::knowledge_maint`、適用は
//! `task_ops::knowledge::apply_candidates`。DESIGN 原則 1 / ADR-0001 D2）。
//!
//! `knowledge_runs` は「そのタスクについて知識整理 run を高々 1 回だけ起こす」ための決定的な目印
//! （`task_id` が主キー = 元のタスク）。行を作る（`state = scheduled`）のは知識整理タスクを作った瞬間、
//! `state` を `done`/`failed` に進めて `applied_at`/`summary` を埋めるのは、その run が終端になり
//! 候補を適用し終えたとき。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::model::TaskId;
use crate::store::{SqliteStore, StoreError, format_rfc3339, parse_rfc3339};

/// `knowledge_runs.state`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRunState {
    /// 知識整理タスクを作った直後（run はまだ終端になっていない）。
    Scheduled,
    /// run が `done` になり、候補を適用し終えた。
    Done,
    /// run が `failed`/`cancelled` で終わった（候補は無い）。
    Failed,
}

impl KnowledgeRunState {
    pub fn as_str(self) -> &'static str {
        match self {
            KnowledgeRunState::Scheduled => "scheduled",
            KnowledgeRunState::Done => "done",
            KnowledgeRunState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "scheduled" => Some(KnowledgeRunState::Scheduled),
            "done" => Some(KnowledgeRunState::Done),
            "failed" => Some(KnowledgeRunState::Failed),
            _ => None,
        }
    }
}

/// `applied_at` が付いたときの `summary_json`（GUI の Console ブロック・タイムラインが読む）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeRunSummary {
    /// 候補の総数（`artifacts/knowledge-candidates.json` の件数）。
    #[serde(default)]
    pub candidates: u32,
    /// 直接 KB にコミットされた件数（`confidence = high` かつ `op in {create, update}`）。
    #[serde(default)]
    pub ingested: u32,
    /// `_inbox/` へ送った件数（`merge`/`retire`/`medium`/`low`/人の編集と衝突）。
    #[serde(default)]
    pub inbox: u32,
    /// 検査で落とした件数（境界違反・秘密・出典なし等）。
    #[serde(default)]
    pub discarded: u32,
    /// 落とした候補の `path` と理由（実機 2026-09-20: 件数だけでは、なぜ捨てられたかを後から追えなかった）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discarded_reasons: Vec<String>,
    /// ADR-0052 D2（Phase 64）: どの経路で抽出したか。`"langmem"`（Qwen）か `"fallback:<adapter>"`
    /// （Qwen に届かず tier cheap の汎用ハーネスに倒した）。`knowledge_runs.via` と同じ値。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

impl KnowledgeRunSummary {
    pub fn total(&self) -> u32 {
        self.ingested + self.inbox + self.discarded
    }
}

/// `knowledge_runs` の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeRun {
    /// 元のタスク（PK）。
    pub task_id: TaskId,
    /// 知識整理 run（裏方の支援タスク）。
    pub run_task_id: TaskId,
    pub state: KnowledgeRunState,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::knowledge_run::opt_rfc3339")]
    #[schemars(with = "Option<String>")]
    pub applied_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<KnowledgeRunSummary>,
    /// ADR-0052 D3（Phase 64）: 失敗した run を**一度だけ**作り直したときの時刻。`None` ならまだ
    /// やり直していない（次の tick で 1 回だけ作り直す対象）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::knowledge_run::opt_rfc3339")]
    #[schemars(with = "Option<String>")]
    pub retried_at: Option<OffsetDateTime>,
    /// ADR-0052 D2（Phase 64）: 実際に抽出した経路（`"langmem"` | `"fallback:<adapter>"`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// ADR-0052 D2: `knowledge_runs.via` の値。
pub const VIA_LANGMEM: &str = "langmem";

/// ADR-0052 D2: 汎用ハーネスに倒したときの `via`（`fallback:<adapter>`）。
pub fn via_fallback(adapter: &str) -> String {
    format!("fallback:{adapter}")
}

/// ADR-0052 D2: `via` が「フォールバックで抽出した」ことを表しているか（Console・タイムラインの表示）。
pub fn via_is_fallback(via: &str) -> bool {
    via.starts_with("fallback:")
}

mod opt_rfc3339 {
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

/// 知識整理 run の追跡（`SqliteStore` が実装する）。
pub trait KnowledgeRunStore: Send + Sync {
    /// 新しく作る（`state = scheduled`）。同じ `task_id` が既にあれば上書きしない
    /// （呼び出し側が [`KnowledgeRunStore::knowledge_run_exists`] で先に確かめる）。
    fn knowledge_run_create(
        &self,
        task_id: TaskId,
        run_task_id: TaskId,
        now: OffsetDateTime,
    ) -> Result<(), StoreError>;
    /// run が終端になったときに `state`/`applied_at`/`summary`/`via` を書く
    /// （ADR-0052 D2: `via` は `"langmem"` か `"fallback:<adapter>"`。分からなければ `None`）。
    fn knowledge_run_finish(
        &self,
        task_id: TaskId,
        state: KnowledgeRunState,
        applied_at: OffsetDateTime,
        summary: Option<&KnowledgeRunSummary>,
        via: Option<&str>,
    ) -> Result<(), StoreError>;
    /// ADR-0052 D3: 失敗した run を**一度だけ**作り直す。`run_task_id` を新しい支援タスクに差し替え、
    /// `state` を `scheduled` に戻し、`retried_at` を書く（`applied_at`/`summary_json`/`via` は消す）。
    /// `retried_at` が既に入っている行は**何もしない**（`false` を返す。二度目は無い）。
    fn knowledge_run_retry(
        &self,
        task_id: TaskId,
        run_task_id: TaskId,
        now: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// ADR-0052 D3: 人の手動やり直し（`celerisctl knowledge rerun`）。`state = failed` /
    /// `retried_at = NULL` に戻し、次の tick の [`KnowledgeRunStore::knowledge_run_retry`] に拾わせる。
    /// 行が無ければ `false`。
    fn knowledge_run_reset(&self, task_id: TaskId) -> Result<bool, StoreError>;
    fn knowledge_run_get(&self, task_id: TaskId) -> Result<Option<KnowledgeRun>, StoreError>;
    fn knowledge_run_by_run_task(
        &self,
        run_task_id: TaskId,
    ) -> Result<Option<KnowledgeRun>, StoreError>;
    fn knowledge_run_exists(&self, task_id: TaskId) -> Result<bool, StoreError>;
    /// `applied_at`（無ければ `created_at`）の新しい順、最大 `limit` 件。Console の一覧に使う。
    fn knowledge_run_recent(&self, limit: usize) -> Result<Vec<KnowledgeRun>, StoreError>;
}

fn row_to_knowledge_run(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<KnowledgeRun, StoreError>> {
    let task_id: String = row.get(0)?;
    let run_task_id: String = row.get(1)?;
    let state: String = row.get(2)?;
    let created_at: String = row.get(3)?;
    let applied_at: Option<String> = row.get(4)?;
    let summary_json: Option<String> = row.get(5)?;
    let retried_at: Option<String> = row.get(6)?;
    let via: Option<String> = row.get(7)?;
    let Ok(task_id) = task_id.parse::<TaskId>() else {
        return Ok(Err(StoreError::Invalid(format!(
            "invalid knowledge_runs.task_id: {task_id}"
        ))));
    };
    let Ok(run_task_id) = run_task_id.parse::<TaskId>() else {
        return Ok(Err(StoreError::Invalid(format!(
            "invalid knowledge_runs.run_task_id: {run_task_id}"
        ))));
    };
    let Some(state) = KnowledgeRunState::parse(&state) else {
        return Ok(Err(StoreError::Invalid(format!(
            "invalid knowledge_runs.state: {state}"
        ))));
    };
    let created_at = match parse_rfc3339(&created_at) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let applied_at = match applied_at.map(|s| parse_rfc3339(&s)).transpose() {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let retried_at = match retried_at.map(|s| parse_rfc3339(&s)).transpose() {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let summary = summary_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<KnowledgeRunSummary>(s).ok());
    Ok(Ok(KnowledgeRun {
        task_id,
        run_task_id,
        state,
        created_at,
        applied_at,
        summary,
        retried_at,
        via,
    }))
}

const SELECT_KNOWLEDGE_RUN: &str = "SELECT task_id, run_task_id, state, created_at, applied_at, \
     summary_json, retried_at, via FROM knowledge_runs";

impl KnowledgeRunStore for SqliteStore {
    fn knowledge_run_create(
        &self,
        task_id: TaskId,
        run_task_id: TaskId,
        now: OffsetDateTime,
    ) -> Result<(), StoreError> {
        let ts = format_rfc3339(now)?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO knowledge_runs (task_id, run_task_id, state, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                task_id.to_string(),
                run_task_id.to_string(),
                KnowledgeRunState::Scheduled.as_str(),
                ts,
            ],
        )?;
        Ok(())
    }

    fn knowledge_run_finish(
        &self,
        task_id: TaskId,
        state: KnowledgeRunState,
        applied_at: OffsetDateTime,
        summary: Option<&KnowledgeRunSummary>,
        via: Option<&str>,
    ) -> Result<(), StoreError> {
        let ts = format_rfc3339(applied_at)?;
        let summary_json = summary
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StoreError::Invalid(format!("could not serialize summary: {e}")))?;
        let conn = self.lock()?;
        conn.execute(
            "UPDATE knowledge_runs SET state = ?2, applied_at = ?3, summary_json = ?4, via = ?5 \
             WHERE task_id = ?1",
            rusqlite::params![task_id.to_string(), state.as_str(), ts, summary_json, via],
        )?;
        Ok(())
    }

    fn knowledge_run_retry(
        &self,
        task_id: TaskId,
        run_task_id: TaskId,
        now: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let ts = format_rfc3339(now)?;
        let conn = self.lock()?;
        // `retried_at IS NULL` が「まだ 1 回も作り直していない」の唯一の判定（ADR-0052 D3「一度だけ」）。
        let changed = conn.execute(
            "UPDATE knowledge_runs SET run_task_id = ?2, state = ?3, retried_at = ?4, \
             applied_at = NULL, summary_json = NULL, via = NULL \
             WHERE task_id = ?1 AND retried_at IS NULL",
            rusqlite::params![
                task_id.to_string(),
                run_task_id.to_string(),
                KnowledgeRunState::Scheduled.as_str(),
                ts,
            ],
        )?;
        Ok(changed > 0)
    }

    fn knowledge_run_reset(&self, task_id: TaskId) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE knowledge_runs SET state = ?2, retried_at = NULL, applied_at = NULL, \
             summary_json = NULL, via = NULL WHERE task_id = ?1",
            rusqlite::params![task_id.to_string(), KnowledgeRunState::Failed.as_str()],
        )?;
        Ok(changed > 0)
    }

    fn knowledge_run_get(&self, task_id: TaskId) -> Result<Option<KnowledgeRun>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!("{SELECT_KNOWLEDGE_RUN} WHERE task_id = ?1"),
                rusqlite::params![task_id.to_string()],
                row_to_knowledge_run,
            )
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn knowledge_run_by_run_task(
        &self,
        run_task_id: TaskId,
    ) -> Result<Option<KnowledgeRun>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!("{SELECT_KNOWLEDGE_RUN} WHERE run_task_id = ?1"),
                rusqlite::params![run_task_id.to_string()],
                row_to_knowledge_run,
            )
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn knowledge_run_exists(&self, task_id: TaskId) -> Result<bool, StoreError> {
        Ok(self.knowledge_run_get(task_id)?.is_some())
    }

    fn knowledge_run_recent(&self, limit: usize) -> Result<Vec<KnowledgeRun>, StoreError> {
        let limit = limit.clamp(1, 1_000);
        let sql = format!(
            "{SELECT_KNOWLEDGE_RUN} ORDER BY COALESCE(applied_at, created_at) DESC, task_id DESC LIMIT {limit}"
        );
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_knowledge_run)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
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
    fn create_is_idempotent_and_finish_updates_state_and_summary() {
        let store = store();
        let task_id = TaskId::new();
        let run_task_id = TaskId::new();
        let now = OffsetDateTime::now_utc();
        store
            .knowledge_run_create(task_id, run_task_id, now)
            .expect("create");
        // 2 回目は無視される（同じ task_id）。
        store
            .knowledge_run_create(task_id, TaskId::new(), now)
            .expect("create again");
        let run = store
            .knowledge_run_get(task_id)
            .expect("get")
            .expect("some");
        assert_eq!(
            run.run_task_id, run_task_id,
            "2 回目の run_task_id は無視される"
        );
        assert_eq!(run.state, KnowledgeRunState::Scheduled);
        assert!(run.applied_at.is_none());
        assert!(run.summary.is_none());
        assert!(store.knowledge_run_exists(task_id).expect("exists"));
        assert!(!store.knowledge_run_exists(TaskId::new()).expect("exists"));

        let summary = KnowledgeRunSummary {
            candidates: 3,
            ingested: 1,
            inbox: 2,
            discarded: 0,
            discarded_reasons: Vec::new(),
            via: Some(VIA_LANGMEM.to_string()),
        };
        let applied_at = now + time::Duration::minutes(5);
        store
            .knowledge_run_finish(
                task_id,
                KnowledgeRunState::Done,
                applied_at,
                Some(&summary),
                Some(VIA_LANGMEM),
            )
            .expect("finish");
        let run = store
            .knowledge_run_get(task_id)
            .expect("get")
            .expect("some");
        assert_eq!(run.state, KnowledgeRunState::Done);
        assert!(run.applied_at.is_some());
        assert_eq!(run.summary, Some(summary));
        assert_eq!(run.summary.as_ref().expect("summary").total(), 3);
        assert_eq!(run.via.as_deref(), Some(VIA_LANGMEM));
        assert!(run.retried_at.is_none());

        let by_run = store
            .knowledge_run_by_run_task(run_task_id)
            .expect("by run")
            .expect("some");
        assert_eq!(by_run.task_id, task_id);

        let recent = store.knowledge_run_recent(10).expect("recent");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].task_id, task_id);
    }

    #[test]
    fn unknown_task_returns_none() {
        let store = store();
        assert_eq!(store.knowledge_run_get(TaskId::new()).expect("get"), None);
        assert_eq!(
            store.knowledge_run_by_run_task(TaskId::new()).expect("get"),
            None
        );
        assert!(store.knowledge_run_recent(10).expect("recent").is_empty());
    }

    /// ADR-0052 D3: 失敗した run は**一度だけ**作り直せる（2 回目の `knowledge_run_retry` は false）。
    #[test]
    fn a_failed_run_is_retried_exactly_once() {
        let store = store();
        let task_id = TaskId::new();
        let first = TaskId::new();
        let now = OffsetDateTime::now_utc();
        store
            .knowledge_run_create(task_id, first, now)
            .expect("create");
        store
            .knowledge_run_finish(task_id, KnowledgeRunState::Failed, now, None, None)
            .expect("finish");

        let second = TaskId::new();
        assert!(
            store
                .knowledge_run_retry(task_id, second, now)
                .expect("retry"),
            "1 回目のやり直しは通る"
        );
        let run = store
            .knowledge_run_get(task_id)
            .expect("get")
            .expect("some");
        assert_eq!(run.run_task_id, second);
        assert_eq!(run.state, KnowledgeRunState::Scheduled);
        assert!(run.retried_at.is_some());
        assert!(run.applied_at.is_none());

        // 2 回目は通らない（`retried_at` が入っているので UPDATE が 0 行）。
        assert!(
            !store
                .knowledge_run_retry(task_id, TaskId::new(), now)
                .expect("retry again")
        );
        assert_eq!(
            store
                .knowledge_run_get(task_id)
                .expect("get")
                .expect("some")
                .run_task_id,
            second
        );
    }

    /// ADR-0052 D3: 人の手動やり直し（`celerisctl knowledge rerun`）は `retried_at` を消し、
    /// もう 1 回だけ自動のやり直しを許す。行が無ければ `false`。
    #[test]
    fn reset_clears_retried_at_and_reports_a_missing_row() {
        let store = store();
        let task_id = TaskId::new();
        let now = OffsetDateTime::now_utc();
        assert!(!store.knowledge_run_reset(task_id).expect("reset missing"));

        store
            .knowledge_run_create(task_id, TaskId::new(), now)
            .expect("create");
        store
            .knowledge_run_finish(
                task_id,
                KnowledgeRunState::Done,
                now,
                Some(&KnowledgeRunSummary::default()),
                Some(VIA_LANGMEM),
            )
            .expect("finish");
        assert!(
            store
                .knowledge_run_retry(task_id, TaskId::new(), now)
                .expect("retry")
        );
        assert!(store.knowledge_run_reset(task_id).expect("reset"));
        let run = store
            .knowledge_run_get(task_id)
            .expect("get")
            .expect("some");
        assert_eq!(run.state, KnowledgeRunState::Failed);
        assert!(run.retried_at.is_none());
        assert!(run.summary.is_none());
        assert!(run.via.is_none());
        // reset した後はもう 1 回だけ自動で作り直せる。
        assert!(
            store
                .knowledge_run_retry(task_id, TaskId::new(), now)
                .expect("retry after reset")
        );
    }

    #[test]
    fn via_helpers_are_deterministic() {
        assert_eq!(via_fallback("codex"), "fallback:codex");
        assert!(via_is_fallback(&via_fallback("claude-code")));
        assert!(!via_is_fallback(VIA_LANGMEM));
    }
}
