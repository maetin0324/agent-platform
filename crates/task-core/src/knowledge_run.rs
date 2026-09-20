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
    /// run が終端になったときに `state`/`applied_at`/`summary` を書く。
    fn knowledge_run_finish(
        &self,
        task_id: TaskId,
        state: KnowledgeRunState,
        applied_at: OffsetDateTime,
        summary: Option<&KnowledgeRunSummary>,
    ) -> Result<(), StoreError>;
    fn knowledge_run_get(&self, task_id: TaskId) -> Result<Option<KnowledgeRun>, StoreError>;
    fn knowledge_run_by_run_task(
        &self,
        run_task_id: TaskId,
    ) -> Result<Option<KnowledgeRun>, StoreError>;
    fn knowledge_run_exists(&self, task_id: TaskId) -> Result<bool, StoreError>;
    /// `applied_at`（無ければ `created_at`）の新しい順、最大 `limit` 件。Console の一覧に使う。
    fn knowledge_run_recent(&self, limit: usize) -> Result<Vec<KnowledgeRun>, StoreError>;
}

fn row_to_knowledge_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<KnowledgeRun, StoreError>> {
    let task_id: String = row.get(0)?;
    let run_task_id: String = row.get(1)?;
    let state: String = row.get(2)?;
    let created_at: String = row.get(3)?;
    let applied_at: Option<String> = row.get(4)?;
    let summary_json: Option<String> = row.get(5)?;
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
    }))
}

const SELECT_KNOWLEDGE_RUN: &str = "SELECT task_id, run_task_id, state, created_at, applied_at, summary_json FROM knowledge_runs";

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
    ) -> Result<(), StoreError> {
        let ts = format_rfc3339(applied_at)?;
        let summary_json = summary
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| StoreError::Invalid(format!("could not serialize summary: {e}")))?;
        let conn = self.lock()?;
        conn.execute(
            "UPDATE knowledge_runs SET state = ?2, applied_at = ?3, summary_json = ?4 WHERE task_id = ?1",
            rusqlite::params![task_id.to_string(), state.as_str(), ts, summary_json],
        )?;
        Ok(())
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
        assert_eq!(run.run_task_id, run_task_id, "2 回目の run_task_id は無視される");
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
        };
        let applied_at = now + time::Duration::minutes(5);
        store
            .knowledge_run_finish(task_id, KnowledgeRunState::Done, applied_at, Some(&summary))
            .expect("finish");
        let run = store
            .knowledge_run_get(task_id)
            .expect("get")
            .expect("some");
        assert_eq!(run.state, KnowledgeRunState::Done);
        assert!(run.applied_at.is_some());
        assert_eq!(run.summary, Some(summary));
        assert_eq!(run.summary.as_ref().unwrap().total(), 3);

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
            store
                .knowledge_run_by_run_task(TaskId::new())
                .expect("get"),
            None
        );
        assert!(store.knowledge_run_recent(10).expect("recent").is_empty());
    }
}
