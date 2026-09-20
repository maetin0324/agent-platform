//! SQLite ベースの `TaskStore` 実装。DESIGN.md §5.1 / §4.3 準拠。
//!
//! - `events` テーブルは追記専用（append-only）の正典。このモジュールから
//!   `events` に対して UPDATE/DELETE を発行することはない。
//! - `tasks` テーブルは `Task` 全体を `json` 列に保持しつつ、検索・排他制御に
//!   使う列（`status` / `kind` / `parent_id` / `priority` / `created_at` /
//!   `lease_*`）を非正規化して複製する。
//!
//! 状態機械のロジック（`crate::transition`）には依存しない（意図的な疎結合。
//! Phase 3 のディスパッチャが `transition()` の結果をこのストアに書き戻す）。

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration as StdDuration;

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::comment::{CommentAuthorKind, TaskComment};
use crate::instance::{DaemonInstance, InstanceRole, SELECT_INSTANCE, row_to_instance};
use crate::integrations::{IntegrationId, IntegrationMethod, IntegrationState, TaskIntegration};
use crate::message::{Message, MessageId, MessageRole, is_conversation};
use crate::model::{Event, Status, Task, TaskId, TaskKind, WorkspaceSpec};
use crate::org::{
    Milestone, MilestoneId, MilestoneStatus, OrgError, OrgKind, OrgNode, Project, ProjectId,
    ProjectStatus,
};
use crate::repos::{ProjectRepo, RepoError, RepoId, RepoKind, RepoRun, RepoSync};
use crate::transition::{InvalidTransition, Outcome, StateView, Trigger, transition};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_events_global_id.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_tasks_list_columns.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_tasks_objective_column.sql");
const MIGRATION_0005: &str = include_str!("../migrations/0005_tasks_genre_column.sql");
const MIGRATION_0006: &str = include_str!("../migrations/0006_organization.sql");
const MIGRATION_0007: &str =
    include_str!("../migrations/0007_messages_task_id_and_reports_project.sql");
const MIGRATION_0008: &str = include_str!("../migrations/0008_notifications.sql");
const MIGRATION_0009: &str = include_str!("../migrations/0009_notifications_project_id.sql");
const MIGRATION_0010: &str = include_str!("../migrations/0010_projects_workspace.sql");
const MIGRATION_0011: &str = include_str!("../migrations/0011_daemon_instances.sql");
/// ADR-0043 D1/D2（Phase 52）: `project_repos` と `tasks.repos_json`。
/// この版を当てた直後に、同じトランザクションで `backfill_project_repos` が写しを作る。
const MIGRATION_0012: &str = include_str!("../migrations/0012_project_repos.sql");
/// ADR-0044 D2/D3（Phase 53）: `task_comments` と `tasks.labels_json` / `tasks.category`。
const MIGRATION_0013: &str = include_str!("../migrations/0013_task_comments.sql");
/// ADR-0043 D5（Phase 54）: `task_integrations`（取り込みの記録）。
const MIGRATION_0014: &str = include_str!("../migrations/0014_task_integrations.sql");
/// ADR-0044 D6（Phase 55）: 案件・途中目標の中止・一時停止・アーカイブ（`archived_at` / `paused_from`）。
const MIGRATION_0015: &str = include_str!("../migrations/0015_lifecycle.sql");
/// ADR-0046 D1/D2/D4（Phase 59）: `org_nodes.profile_json` と `tasks.skills_json` / `tasks.mode`。
const MIGRATION_0016: &str = include_str!("../migrations/0016_org_profiles.sql");

/// このバイナリが知っている最新のスキーマ版数（ADR-0013 D5）。DB の版数がこれより大きければ
/// `SqliteStore::open`/`open_with` は `StoreError::SchemaTooNew` で失敗する。
pub const SCHEMA_VERSION: u32 = 16;

/// `SqliteStore::open_with` に渡す接続オプション（ADR-0013 D5）。
#[derive(Debug, Clone, Copy)]
pub struct StoreOptions {
    /// `PRAGMA busy_timeout`。複数接続（ディスパッチャ・API・celerisctl）が同じファイルを
    /// 開くときにロック待ちする時間。既定は 5000 ms。
    pub busy_timeout: StdDuration,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            busy_timeout: StdDuration::from_millis(5000),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("time formatting error: {0}")]
    TimeFormat(#[from] time::error::Format),
    #[error("time parsing error: {0}")]
    TimeParse(#[from] time::error::Parse),
    #[error("mutex poisoned")]
    Poisoned,
    #[error("invalid stored data: {0}")]
    Invalid(String),
    #[error(transparent)]
    InvalidTransition(#[from] InvalidTransition),
    /// ADR-0013 D5: DB の `schema_migrations` の最大版数がこのバイナリの `SCHEMA_VERSION` より
    /// 大きい場合に返す。DB を書き換えずに `open`/`open_with` を失敗させる。
    #[error("db schema version {found} is newer than the {supported} this binary supports")]
    SchemaTooNew { found: u32, supported: u32 },
    /// ADR-0033 D1: 使用中のため消せない（組織のノードが未終了のタスクを抱えている）。API は 409。
    #[error("{kind} {id} is still in use: {detail}")]
    InUse {
        kind: &'static str,
        id: String,
        detail: String,
    },
    /// ADR-0033 D1: 組織の検証に落ちた（API は 422）。
    #[error(transparent)]
    Org(#[from] OrgError),
    /// ADR-0043 D1（Phase 52）: 案件のリポジトリの検証に落ちた（API は 422）。
    #[error(transparent)]
    Repo(#[from] RepoError),
}

/// ADR-0046 D1（Phase 59）: `org_nodes.profile_json` に書く値。空の profile は NULL
/// （導入前のノードの行と 1 バイトも変わらない）。
fn profile_json(profile: &crate::profile::Profile) -> Result<Option<String>, StoreError> {
    if profile.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(profile)?))
}

/// ADR-0046 D7: ノード id を持つ表と列（`celerisctl org migrate-v2` が触ってよいものだけ）。
fn is_known_node_ref(table: &str, column: &str) -> bool {
    matches!(
        (table, column),
        ("tasks", "assignee")
            | ("messages", "node_id")
            | ("reports", "node_id")
            | ("approvals", "node_id")
            | ("standing_rules", "node_id")
    )
}

/// ADR-0046 D7: 1 行のノード id を書き換える。`tasks` は `json`（正本）も直す。
fn set_node_ref(
    tx: &Connection,
    table: &str,
    column: &str,
    id: &str,
    value: &str,
) -> Result<(), StoreError> {
    if table == "tasks" {
        let json: Option<String> = tx
            .query_row("SELECT json FROM tasks WHERE id = ?1", params![id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(json) = json else { return Ok(()) };
        let mut task: Task = serde_json::from_str(&json)?;
        task.assignee = Some(value.to_string());
        let updated = serde_json::to_string(&task)?;
        tx.execute(
            "UPDATE tasks SET assignee = ?1, json = ?2 WHERE id = ?3",
            params![value, updated, id],
        )?;
        return Ok(());
    }
    let sql = format!("UPDATE {table} SET {column} = ?1 WHERE id = ?2");
    tx.execute(&sql, params![value, id])?;
    Ok(())
}

/// `events` テーブルの 1 行（ADR-0013 D6）。`id` はテーブル全体でのグローバル単調増加値。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventRow {
    pub id: u64,
    pub task_id: TaskId,
    pub seq: u64,
    /// DB に保存された RFC 3339 文字列そのまま。
    pub ts: String,
    pub event: Event,
}

/// 生成した `EventRow` の JSON Schema（`serde_json::Value`）。`docs/api/v1/event.schema.json` と
/// 一致することを `event_row_schema_matches_committed` で検証する。
pub fn event_row_schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(EventRow);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

/// `TaskStore::list_page` のフィルタ（ADR-0013 D10）。既定は絞り込み無し。
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// 空なら status で絞らない。
    pub statuses: Vec<Status>,
    /// 空なら kind で絞らない。
    pub kinds: Vec<TaskKind>,
    /// ADR-0027 D1: 空なら genre で絞らない。完全一致（`kinds` と同じ形）。
    pub genres: Vec<String>,
    pub parent_id: Option<TaskId>,
    /// ADR-0033 D2: 案件で絞る（案件の仕事の木 = `tasks WHERE project_id = ?`）。
    pub project_id: Option<ProjectId>,
    /// true なら `parent_id IS NULL` のタスクのみ（`parent_id` フィルタとは独立に AND で効く）。
    pub root_only: bool,
    /// `title` または `objective` に対する部分一致（SQLite の LIKE なので ASCII の大文字小文字は区別しない。ADR-0014 D2）。
    /// `%` / `_` はリテラルとして扱う。
    pub text_contains: Option<String>,
    /// ADR-0033 D4（Phase 33）: 担当（`org_nodes.id`）で絞る。完全一致。`None` なら絞らない。
    pub assignee: Option<String>,
    // ---- ADR-0044 D4（Phase 53）: ボードと検索のフィルタ。ここから ----
    /// ADR-0044 D4: ラベル。複数指定は **AND**（全部持つタスクだけ）。
    pub labels: Vec<String>,
    /// ADR-0044 D4: 種類。複数指定は IN（どれか）。
    pub categories: Vec<crate::model::TaskCategory>,
    /// ADR-0044 D4: 途中目標（`milestones.id`）。完全一致。
    pub milestone_id: Option<MilestoneId>,
    /// ADR-0044 D4: tier（`worker_hint.tier`）。複数指定は IN。
    pub tiers: Vec<crate::model::Tier>,
    /// ADR-0044 D4: 優先度（P0〜P3 を `i32` に写したもの）。複数指定は IN。
    pub priorities: Vec<i32>,
    /// ADR-0044 D4: `text_contains` を**コメント本文にも**広げる（`GET /tasks?q=`）。
    /// `false` なら従来どおり title / objective だけ（`celerisctl` の既存の挙動）。
    pub text_includes_comments: bool,
    // ---- ADR-0044 D4（Phase 53）: ここまで ----
    /// ADR-0044 D6（Phase 55）: **アーカイブされた案件のタスクを隠す**（`GET /tasks` の既定。
    /// `?archived=1` で `false`）。既定は `false`（＝隠さない）なので、ディスパッチャ・報告・
    /// 途中目標のレビューなど既存の呼び出し側の挙動は変わらない。
    pub hide_archived: bool,
}

/// `TaskStore::list_page` の並び順（ADR-0013 D10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOrder {
    /// `priority DESC, created_at ASC, id ASC`（ディスパッチ順）。
    Dispatch,
    /// `updated_at DESC, id DESC`。
    UpdatedDesc,
    /// `created_at DESC, id DESC`。
    CreatedDesc,
}

/// keyset ページングの 1 ページ。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// 次ページがあれば `Some`。`list_page` にそのまま渡せる不透明な文字列。
    pub next_cursor: Option<String>,
    /// 同じフィルタでの総件数（cursor に依らない）。
    pub total: u64,
}

/// `list_page` の cursor の内側の表現。`{priority, created_at, updated_at, id}` を JSON にして
/// バイト列を 16 進エンコードしたものが `cursor` 文字列（不透明・実装依存。`ListOrder` ごとに
/// 必要な列だけを使って keyset 述語を組み立てる）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorPayload {
    priority: i64,
    created_at: String,
    updated_at: String,
    id: String,
}

impl CursorPayload {
    fn from_task(task: &Task) -> Result<Self, StoreError> {
        Ok(Self {
            priority: task.priority as i64,
            created_at: format_rfc3339(task.created_at)?,
            updated_at: format_rfc3339(task.updated_at)?,
            id: task.id.to_string(),
        })
    }
}

fn encode_cursor(payload: &CursorPayload) -> Result<String, StoreError> {
    let json = serde_json::to_vec(payload)?;
    let mut out = String::with_capacity(json.len() * 2);
    for byte in json {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

fn decode_cursor(cursor: &str) -> Result<CursorPayload, StoreError> {
    let invalid = || StoreError::Invalid(format!("invalid cursor: {cursor}"));
    if cursor.is_empty()
        || !cursor.len().is_multiple_of(2)
        || !cursor.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    let bytes_chars: Vec<char> = cursor.chars().collect();
    let mut bytes = Vec::with_capacity(bytes_chars.len() / 2);
    for pair in bytes_chars.chunks(2) {
        let s: String = pair.iter().collect();
        let byte = u8::from_str_radix(&s, 16).map_err(|_| invalid())?;
        bytes.push(byte);
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

fn usize_to_i64(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn u64_to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

/// `ListFilter` を `WHERE` 述語と束縛パラメータに変換する（`cursor` の keyset 述語は含まない）。
fn filter_predicate(filter: &ListFilter) -> (String, Vec<SqlValue>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();

    if !filter.statuses.is_empty() {
        let placeholders = vec!["?"; filter.statuses.len()].join(", ");
        clauses.push(format!("status IN ({placeholders})"));
        for s in &filter.statuses {
            params.push(SqlValue::Text(status_str(*s).to_string()));
        }
    }
    if !filter.kinds.is_empty() {
        let placeholders = vec!["?"; filter.kinds.len()].join(", ");
        clauses.push(format!("kind IN ({placeholders})"));
        for k in &filter.kinds {
            params.push(SqlValue::Text(kind_str(*k).to_string()));
        }
    }
    if !filter.genres.is_empty() {
        let placeholders = vec!["?"; filter.genres.len()].join(", ");
        clauses.push(format!("genre IN ({placeholders})"));
        for g in &filter.genres {
            params.push(SqlValue::Text(g.clone()));
        }
    }
    if filter.root_only {
        clauses.push("parent_id IS NULL".to_string());
    }
    if let Some(parent_id) = filter.parent_id {
        clauses.push("parent_id = ?".to_string());
        params.push(SqlValue::Text(parent_id.to_string()));
    }
    if let Some(project_id) = filter.project_id {
        clauses.push("project_id = ?".to_string());
        params.push(SqlValue::Text(project_id.to_string()));
    }
    if let Some(assignee) = &filter.assignee {
        clauses.push("assignee = ?".to_string());
        params.push(SqlValue::Text(assignee.clone()));
    }
    // ---- ADR-0044 D4（Phase 53）----
    if let Some(milestone_id) = filter.milestone_id {
        clauses.push("milestone_id = ?".to_string());
        params.push(SqlValue::Text(milestone_id.to_string()));
    }
    if !filter.categories.is_empty() {
        let placeholders = vec!["?"; filter.categories.len()].join(", ");
        // 導入前の行（`category` が NULL）は `other` として扱う。
        clauses.push(format!("COALESCE(category, 'other') IN ({placeholders})"));
        for c in &filter.categories {
            params.push(SqlValue::Text(c.as_str().to_string()));
        }
    }
    for label in &filter.labels {
        // AND: 指定したラベルを**全部**持つタスクだけ。`labels_json` は `PATCH` でも書き直す写し。
        clauses.push(
            "EXISTS (SELECT 1 FROM json_each(COALESCE(tasks.labels_json, '[]')) WHERE json_each.value = ?)"
                .to_string(),
        );
        params.push(SqlValue::Text(label.clone()));
    }
    if !filter.tiers.is_empty() {
        // tier は `json` の中にしか無い（列を増やさない）。JSON1 の `json_extract` で決定的に引く。
        let placeholders = vec!["?"; filter.tiers.len()].join(", ");
        clauses.push(format!(
            "json_extract(json, '$.worker_hint.tier') IN ({placeholders})"
        ));
        for t in &filter.tiers {
            params.push(SqlValue::Text(tier_str(*t).to_string()));
        }
    }
    if !filter.priorities.is_empty() {
        let placeholders = vec!["?"; filter.priorities.len()].join(", ");
        clauses.push(format!("priority IN ({placeholders})"));
        for p in &filter.priorities {
            params.push(SqlValue::Integer(*p as i64));
        }
    }
    // ADR-0044 D6（Phase 55）: アーカイブされた案件のタスクを隠す（案件に属さないタスクは常に見える）。
    if filter.hide_archived {
        clauses.push(
            "NOT EXISTS (SELECT 1 FROM projects p WHERE p.id = tasks.project_id AND p.archived_at IS NOT NULL)"
                .to_string(),
        );
    }
    if let Some(needle) = &filter.text_contains {
        let pattern = format!("%{}%", escape_like(needle));
        if filter.text_includes_comments {
            // ADR-0044 D4: `q=` は title / objective / コメント本文の 3 つ（OR）。
            clauses.push(
                "(title LIKE ? ESCAPE '\\' OR objective LIKE ? ESCAPE '\\' OR EXISTS \
                 (SELECT 1 FROM task_comments c WHERE c.task_id = tasks.id AND c.body LIKE ? ESCAPE '\\'))"
                    .to_string(),
            );
            params.push(SqlValue::Text(pattern.clone()));
            params.push(SqlValue::Text(pattern.clone()));
            params.push(SqlValue::Text(pattern));
        } else {
            clauses.push("(title LIKE ? ESCAPE '\\' OR objective LIKE ? ESCAPE '\\')".to_string());
            params.push(SqlValue::Text(pattern.clone()));
            params.push(SqlValue::Text(pattern));
        }
    }

    if clauses.is_empty() {
        ("1=1".to_string(), params)
    } else {
        (clauses.join(" AND "), params)
    }
}

fn order_by_sql(order: ListOrder) -> &'static str {
    match order {
        ListOrder::Dispatch => "priority DESC, created_at ASC, id ASC",
        ListOrder::UpdatedDesc => "updated_at DESC, id DESC",
        ListOrder::CreatedDesc => "created_at DESC, id DESC",
    }
}

/// `cursor` より後（= 次ページ側）の行だけを選ぶ keyset 述語。`order_by_sql` と対にして使う。
fn keyset_predicate(order: ListOrder, cursor: &CursorPayload) -> (String, Vec<SqlValue>) {
    match order {
        ListOrder::Dispatch => (
            "(priority < ?) OR (priority = ? AND created_at > ?) OR \
             (priority = ? AND created_at = ? AND id > ?)"
                .to_string(),
            vec![
                SqlValue::Integer(cursor.priority),
                SqlValue::Integer(cursor.priority),
                SqlValue::Text(cursor.created_at.clone()),
                SqlValue::Integer(cursor.priority),
                SqlValue::Text(cursor.created_at.clone()),
                SqlValue::Text(cursor.id.clone()),
            ],
        ),
        ListOrder::UpdatedDesc => (
            "(updated_at < ?) OR (updated_at = ? AND id < ?)".to_string(),
            vec![
                SqlValue::Text(cursor.updated_at.clone()),
                SqlValue::Text(cursor.updated_at.clone()),
                SqlValue::Text(cursor.id.clone()),
            ],
        ),
        ListOrder::CreatedDesc => (
            "(created_at < ?) OR (created_at = ? AND id < ?)".to_string(),
            vec![
                SqlValue::Text(cursor.created_at.clone()),
                SqlValue::Text(cursor.created_at.clone()),
                SqlValue::Text(cursor.id.clone()),
            ],
        ),
    }
}

fn parse_status(s: &str) -> Result<Status, StoreError> {
    match s {
        "draft" => Ok(Status::Draft),
        "ready" => Ok(Status::Ready),
        "running" => Ok(Status::Running),
        "blocked" => Ok(Status::Blocked),
        "reviewing" => Ok(Status::Reviewing),
        "done" => Ok(Status::Done),
        "failed" => Ok(Status::Failed),
        "cancelled" => Ok(Status::Cancelled),
        other => Err(StoreError::Invalid(format!(
            "invalid status in tasks table: {other}"
        ))),
    }
}

/// ADR-0033 D3: 報告（`reports`）の読み書きは `crate::report::ReportStore` にあり、`TaskStore` はそれを
/// supertrait として要求する（ディスパッチャの `Arc<dyn TaskStore>` から報告を追記できるようにするため。
/// 実装は `report.rs` にあり、この表の SQL はここには無い）。
/// ADR-0033 D5（Phase 26）: 認可（`approvals` / `standing_rules`）も同じ形で `crate::approval::ApprovalStore`
/// にある。
/// ADR-0037 D1（Phase 39）: 通知の台帳（`notifications`）も同じ形で `crate::notify::NotificationStore` にある。
pub trait TaskStore:
    Send
    + Sync
    + crate::report::ReportStore
    + crate::approval::ApprovalStore
    + crate::notify::NotificationStore
{
    fn insert(&self, task: &Task) -> Result<(), StoreError>;
    fn get(&self, id: TaskId) -> Result<Option<Task>, StoreError>;
    fn list(&self, filter: Option<Status>) -> Result<Vec<Task>, StoreError>;
    /// イベントを追記し、割り当てられた `seq`（0始まり、task_id 内で単調増加）を返す。
    fn append_event(&self, task_id: TaskId, event: &Event) -> Result<u64, StoreError>;
    /// `task_id` に紐づく全イベントを `seq` 昇順で返す。**`seq` はタスクごとに 0 始まりで単調増加する
    /// ローカルな連番**であり、別のタスクの `seq` と大小比較することはできない（Phase 45 実機バグ:
    /// ADR-0021 D3 の「一度扱った失敗は数え直さない」判定で、親の `seq` と子の `seq` を比較していたため、
    /// 子の失敗が毎回「新規」と判定され、同じ質問が繰り返し出た）。タスクをまたいでイベントの前後関係を
    /// 比較したい場合は `events_for_with_global_ids` を使うこと。
    fn events_for(&self, task_id: TaskId) -> Result<Vec<(u64, Event)>, StoreError>;
    /// `task_id` に紐づく全イベントを、`events` テーブルの**グローバルに単調増加する id**（`seq` ではない）
    /// と一緒に id 昇順で返す（Phase 45）。この id は全タスクを横断して単調増加するため、
    /// 異なるタスクのイベント同士の前後関係を比較してよい（`events_for` の `seq` はタスクごとにローカルなので
    /// 比較できない）。
    fn events_for_with_global_ids(&self, task_id: TaskId) -> Result<Vec<(u64, Event)>, StoreError>;
    /// 排他的にリースを取得する。成功したら true を返し、task の status を Running にし、
    /// lease = Some{worker_run_id, expires_at: now + ttl} をDBに書く。
    /// 既にリースされている／status != Ready の場合は false を返す（エラーではない）。
    fn acquire_lease(
        &self,
        task_id: TaskId,
        worker_run_id: &str,
        ttl: StdDuration,
    ) -> Result<bool, StoreError>;
    /// worker_run_id が現在のリースと一致する場合のみ lease を None にする。status は変更しない。
    fn release_lease(&self, task_id: TaskId, worker_run_id: &str) -> Result<(), StoreError>;
    /// status=Ready かつ depends_on が全て Done かつ（親が存在し kind=Approval の場合は親が Done）
    /// を満たすタスクを priority DESC, created_at ASC で最大 limit 件返す。
    fn ready_tasks(&self, limit: usize) -> Result<Vec<Task>, StoreError>;
    /// `transition::transition()` で検証した任意のトリガーを適用する汎用の書き込み口
    /// （ADR-0004 D1）。タスクの取得・`transition()` の呼び出し・`tasks` 行の更新・
    /// `Event::Transitioned` の追記（と任意の `extra_event`）を単一トランザクションで行う。
    /// タスクが存在しない場合は `StoreError::Invalid`、遷移が無効な場合は
    /// `StoreError::InvalidTransition` を返し、いずれもタスクの状態は変更しない。
    fn apply_transition(
        &self,
        task_id: TaskId,
        trigger: Trigger,
        extra_event: Option<Event>,
    ) -> Result<Outcome, StoreError> {
        self.apply_transition_with_events(task_id, trigger, extra_event.into_iter().collect())
    }
    /// `apply_transition` の一般形（ADR-0005 D4）。`extra_events` を順に、`Event::Transitioned`
    /// の直後に同一トランザクションで追記する。ディスパッチャが `WorkerFinished` や
    /// 条件ごとの `ReviewVerdict` を遷移と原子的に記録するために使う。
    fn apply_transition_with_events(
        &self,
        task_id: TaskId,
        trigger: Trigger,
        extra_events: Vec<Event>,
    ) -> Result<Outcome, StoreError>;

    /// ADR-0007 D3: Plan の子タスク群を挿入（`Created`、`accept_children` なら続けて `Accept`）し、
    /// 親に `ReviewPass` を適用して `verdict_events` を追記する。全体が 1 トランザクション。
    fn complete_plan(
        &self,
        plan_id: TaskId,
        verdict_events: Vec<Event>,
        children: Vec<Task>,
        accept_children: bool,
    ) -> Result<Outcome, StoreError>;

    /// ADR-0010 D2: `insert` + `Event::Created` + `extra_events` を 1 トランザクションで行う。
    fn create_task(&self, task: &Task, extra_events: Vec<Event>) -> Result<(), StoreError>;

    /// ADR-0016 D2 / M2: 実行中の委譲。子タスク群を挿入（`Created` → `Accept` で `ready`）し、親に
    /// `Event::Delegated{run_id, task_ids}` を追記する。全体が 1 トランザクション。親の状態は変えない。
    /// 子の `parent_id` が `parent_id` と違えば `StoreError::Invalid`。
    fn delegate_children(
        &self,
        parent_id: TaskId,
        run_id: &str,
        children: Vec<Task>,
    ) -> Result<Vec<TaskId>, StoreError>;

    /// `parent_id` を親に持つタスク（終端を含む）を `created_at` 昇順（同時刻は挿入順）で返す（ADR-0016 M5 / M6）。
    fn children(&self, parent_id: TaskId) -> Result<Vec<Task>, StoreError>;

    /// ADR-0010 D2 / D7（P-7）: `status = running` かつリースの run_id が一致するときだけ `expires_at = now + ttl` に
    /// 延長して true を返す。状態遷移ではないのでイベントは追記しない。
    fn renew_lease(
        &self,
        task_id: TaskId,
        worker_run_id: &str,
        ttl: StdDuration,
    ) -> Result<bool, StoreError>;

    /// ADR-0013 D6: `events` を `id` 昇順で `after_id` より後、最大 `limit` 件返す。
    fn events_since(&self, after_id: u64, limit: usize) -> Result<Vec<EventRow>, StoreError>;
    /// ADR-0013 D6: `events` の現在の最大 `id`。行が無ければ 0。
    fn latest_event_id(&self) -> Result<u64, StoreError>;
    /// ADR-0013（Phase 9b）: 1 タスクのイベントを `seq` 昇順で、`after_seq` より後（`None` なら最初から）最大 `limit` 件、
    /// グローバル `id` と `ts` 付きで返す（API の `GET /tasks/{id}/events` と run の要約が使う）。
    fn event_rows_for(
        &self,
        task_id: TaskId,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<EventRow>, StoreError>;

    /// ADR-0013 D10: `filter` に一致する `tasks` を `order` で keyset ページングして返す。`cursor` は
    /// 前回の `Page::next_cursor`（不透明な文字列）。不正な `cursor` は `StoreError::Invalid`。
    fn list_page(
        &self,
        filter: &ListFilter,
        order: ListOrder,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Task>, StoreError>;
    /// ADR-0013 D10: `tasks` の件数を `status` ごとに集計する（0 件の status は含まない）。
    fn count_by_status(&self) -> Result<Vec<(Status, u64)>, StoreError>;

    // ---- ADR-0033 D1: 組織（`org_nodes`）。DB が正で、設定は空のときの種蒔きにしか使わない ----

    /// 全ノードを `position`、同値なら `id` の昇順で返す（木は呼び出し側が `parent_id` で組む）。
    fn org_list(&self) -> Result<Vec<OrgNode>, StoreError>;
    /// 1 ノード。無ければ `None`。
    fn org_get(&self, id: &str) -> Result<Option<OrgNode>, StoreError>;
    /// 挿入または更新。`crate::org::validate_upsert` を通してから書く（secretary は 1 つ、親は既存、
    /// 自分を祖先にできない、secretary > department > section）。`created_at` は既存行のものを保つ。
    fn org_upsert(&self, node: &OrgNode) -> Result<OrgNode, StoreError>;
    /// 監査 D-4: 設定からの種蒔き専用。`nodes` を渡された順に検証しながら**1 トランザクション**で書く
    /// （後の要素は前の要素を `existing` に含めて検証できるので、`secretary` → `department` → `section`
    /// の順に並んでいれば通る）。途中の 1 件でも `crate::org::validate_upsert` に落ちたら、それより前の
    /// 分も含めて何も書かない（部分的に蒔かれた組織が残ると、次回起動時は `org_list` が空でなくなり
    /// 補完されないため）。
    fn org_seed(&self, nodes: &[OrgNode]) -> Result<(), StoreError>;
    /// 削除。そのノードを `assignee` に持つ未終了タスクがあれば `StoreError::InUse`、
    /// 子ノードがあっても `StoreError::InUse`（木を宙ぶらりんにしない）。無い id は `Ok(false)`。
    fn org_delete(&self, id: &str) -> Result<bool, StoreError>;

    // ---- ADR-0033 D2: 案件（`projects`）と途中目標（`milestones`）----

    /// 案件を作る（`status` は呼び出し側が決める。API は `proposed`）。
    fn project_create(&self, project: &Project) -> Result<(), StoreError>;
    fn project_get(&self, id: ProjectId) -> Result<Option<Project>, StoreError>;
    /// `created_at` の降順（新しい案件が先）。
    fn project_list(&self) -> Result<Vec<Project>, StoreError>;
    /// 状態だけを変える（`updated_at` も更新）。無い案件は `Ok(false)`。
    fn project_set_status(&self, id: ProjectId, status: ProjectStatus) -> Result<bool, StoreError>;
    /// ADR-0039 D1: 作業場所だけを変える（`None` で消す。`updated_at` も更新）。無い案件は `Ok(false)`。
    fn project_set_workspace(
        &self,
        id: ProjectId,
        workspace: Option<&WorkspaceSpec>,
    ) -> Result<bool, StoreError>;
    /// ADR-0044 D6（Phase 55）: 状態と `paused_from` を**同時に**書く（`pause` / `resume` / `cancel`）。
    /// `paused_from` は `Some(None)` で消し、`None` なら触らない。無い案件は `Ok(false)`。
    fn project_set_lifecycle(
        &self,
        id: ProjectId,
        status: ProjectStatus,
        paused_from: Option<Option<ProjectStatus>>,
    ) -> Result<bool, StoreError>;
    /// ADR-0044 D6（Phase 55）: アーカイブの時刻を書く（`None` で解除）。無い案件は `Ok(false)`。
    fn project_set_archived_at(
        &self,
        id: ProjectId,
        at: Option<OffsetDateTime>,
    ) -> Result<bool, StoreError>;

    // ---- ADR-0043 D1（Phase 52）: 案件のリポジトリ（`project_repos`）----

    /// 案件のリポジトリを 1 件作る。`repos::validate_upsert` に落ちれば `StoreError::Repo`（API は 422）。
    /// `is_primary` を立てた行を作ると、同じ案件の他の行の `is_primary` は落ちる（1 案件に 1 つ）。
    fn repo_create(&self, repo: &ProjectRepo) -> Result<(), StoreError>;
    fn repo_get(&self, id: RepoId) -> Result<Option<ProjectRepo>, StoreError>;
    /// その案件のリポジトリ（primary が先、あとは作った順）。
    fn repo_list(&self, project_id: ProjectId) -> Result<Vec<ProjectRepo>, StoreError>;
    /// 既存の 1 件を差し替える（`id` と `project_id` は変えない）。無い id は `Ok(false)`。
    fn repo_update(&self, repo: &ProjectRepo) -> Result<bool, StoreError>;
    /// 消す。未終端のタスクが参照していれば `StoreError::InUse`（API は 409）。無い id は `Ok(false)`。
    fn repo_delete(&self, id: RepoId) -> Result<bool, StoreError>;
    /// その案件の primary をこの行にする。無い id は `Ok(false)`。
    fn repo_set_primary(&self, id: RepoId) -> Result<bool, StoreError>;
    /// そのリポジトリを参照している未終端のタスク（`DELETE` の 409 の理由に使う）。
    fn repo_active_tasks(&self, id: RepoId) -> Result<Vec<TaskId>, StoreError>;

    // ---- ADR-0043 D5（Phase 54）: 変更の取り込み（`task_integrations`）----

    /// 取り込みの記録を 1 件書く（同じ `id` があれば差し替える。`updated_at` は呼び出し側が入れる）。
    /// **人（管理系 API）だけが呼ぶ**。組織の「人」がここに届く経路は無い（SPEC §3.6）。
    fn integration_put(&self, integration: &TaskIntegration) -> Result<(), StoreError>;
    fn integration_get(&self, id: IntegrationId) -> Result<Option<TaskIntegration>, StoreError>;
    /// そのタスクの記録（新しい順）。ADR-0044 B1 の timeline はこれを読めばよい。
    fn integration_list_for_task(
        &self,
        task_id: TaskId,
    ) -> Result<Vec<TaskIntegration>, StoreError>;
    /// そのタスクのそのリポジトリの**最新の** 1 件（`GET /tasks/{id}/changes` の `integration`）。
    fn integration_latest(
        &self,
        task_id: TaskId,
        repo: &str,
    ) -> Result<Option<TaskIntegration>, StoreError>;
    /// 案件のタスクの取り込み（**タスク × リポジトリごとに最新の 1 件**。新しい順、`limit` 件まで）。
    /// 案件画面の「PR と取り込み」（ADR-0043 D5）。
    fn integration_list_for_project(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<TaskIntegration>, StoreError>;

    /// 途中目標を作る。`seq` はその案件の最大 + 1 をストアが採番し、確定した行を返す。
    /// 案件が無ければ `StoreError::Invalid`。
    fn milestone_create(
        &self,
        project_id: ProjectId,
        title: &str,
        description: &str,
        status: MilestoneStatus,
    ) -> Result<Milestone, StoreError>;
    /// その案件の途中目標を `seq` 昇順で返す。
    fn milestone_list(&self, project_id: ProjectId) -> Result<Vec<Milestone>, StoreError>;
    /// ADR-0044 D6（Phase 55）: 途中目標を id 1 つで引く（案件を知らなくてよい）。
    fn milestone_get(&self, id: MilestoneId) -> Result<Option<Milestone>, StoreError>;
    /// 状態だけを変える。無い途中目標は `Ok(false)`。
    fn milestone_set_status(
        &self,
        id: MilestoneId,
        status: MilestoneStatus,
    ) -> Result<bool, StoreError>;
    /// ADR-0044 D6（Phase 55）: 状態と `paused_from` を**同時に**書く（`pause` / `resume` / `cancel`）。
    /// `paused_from` は `Some(None)` で消し、`None` なら触らない。無い途中目標は `Ok(false)`。
    fn milestone_set_lifecycle(
        &self,
        id: MilestoneId,
        status: MilestoneStatus,
        paused_from: Option<Option<MilestoneStatus>>,
    ) -> Result<bool, StoreError>;

    // ---- ADR-0033 D4: 対話（`messages`）----

    /// 1 行を追記する（対話は追記専用。更新も削除もしない）。
    fn message_append(&self, message: &Message) -> Result<(), StoreError>;
    /// `node_id` とのやり取りを**古い順**（`created_at` 昇順、同時刻は `id` 昇順）で最大 `limit` 件返す。
    /// `project_id` が `Some` ならその案件の行だけ、`None` なら案件に紐づかない行だけ（雑談）。
    /// 件数が `limit` を超えるときは**新しい方**を残す（直近のやり取りを渡すため）。
    fn message_list(
        &self,
        node_id: &str,
        project_id: Option<ProjectId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError>;

    /// ADR-0048 D1（Phase 60a）: Console の一本の流れ用。`message_list` と違い **絞り込みは任意**で、
    /// `node_id` / `project_id` が `None` なら「その軸では絞らない」（`message_list` の `project_id: None`
    /// は「案件に紐づかない行だけ」なので意味が違う）。
    ///
    /// `after` は RFC 3339 の `created_at`。`Some` なら **その時刻以降（`>=`、閉区間）を古い順**に最大
    /// `limit` 件（同じ時刻に複数行あっても取りこぼさないため閉区間。呼び出し側がカーソルで重複を落とす）。
    /// `None` なら **いちばん新しい `limit` 件**を古い順に返す（Console の初期表示）。
    fn message_page(
        &self,
        node_id: Option<&str>,
        project_id: Option<ProjectId>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError>;

    // ---- Phase 31: 失敗した仕事をやり直す（実機の事故、2026-09-18）----

    /// `original` は `failed` または `cancelled` でなければ `StoreError::InvalidTransition`（`trigger = "retry"`）。
    /// `new_task` を挿入し（`Event::Created` + `Event::Retried{from: original}`)、`original` に依存していた
    /// 「未終端」または「`dependency_failed` で `cancelled` になった」タスクの `depends_on` を `new_task.id` に
    /// 張り替える（後者は `draft` に戻し、`Event::Transitioned{from: cancelled, to: draft, reason: "retried"}`
    /// を追記する）。全体を単一トランザクションで行い、張り替えたタスクの id を返す。
    fn retry_task(&self, original: TaskId, new_task: &Task) -> Result<Vec<TaskId>, StoreError>;

    // ---- ADR-0044 D1/D2（Phase 53）: 人の編集とタスク単位のコメント ----

    /// ADR-0044 D1: 人の編集を書き戻す（`json` と絞り込みの列、`Event::Edited` を単一トランザクションで）。
    /// **状態機械は通らない**: `status` / `attempts` / `lease` は渡された `task` の値を**使わず**、
    /// トランザクションの中で読み直した現在の行の値を書く（編集を組み立てている間にディスパッチャが
    /// リースを取っていても、その run を壊さないため。`acquire_lease` / `release_lease` と同じ規律）。
    /// 書き込んだ後の `Task`（= 読み直した状態を持つもの）を返す。無いタスクは `StoreError::Invalid`。
    fn update_task(&self, task: &Task, event: Event) -> Result<Task, StoreError>;

    /// ADR-0044 D2: コメントを 1 件追記する。`transition` が `Some((trigger, extra_events))` なら
    /// **同じトランザクション**で状態遷移も行い、その結果を返す（人のコメントの割り込みが
    /// 「コメントは残ったが run は止まらなかった」状態にならないようにするため）。
    fn comment_add(
        &self,
        comment: &TaskComment,
        transition: Option<(Trigger, Vec<Event>)>,
    ) -> Result<Option<Outcome>, StoreError>;

    /// ADR-0044 D2: そのタスクのコメントを古い順（`created_at`、同時刻は `id` 昇順）で返す。
    fn comments_for(&self, task_id: TaskId) -> Result<Vec<TaskComment>, StoreError>;

    // ---- ADR-0040 D4（Phase 47）: celeris のインスタンスの役割（`daemon_instances`）----
    //
    // ここにあるのは「行を読み書きする」だけの操作で、役割を決める規則（誰が active になるか、いつ
    // drain するか）は celeris 側（`celeris::instance`）にある。`verify` のインスタンスはこの表に触れない。

    /// 自分の行を作る（既にあれば上書きする＝同じ `instance_id` で起動し直したとき）。
    /// `handoff_requested_at` / `drained_at` は NULL に戻る。
    fn instance_register(&self, instance: &DaemonInstance) -> Result<(), StoreError>;
    /// 自分の行の `heartbeat_at` を更新する。行が無ければ `Ok(false)`（呼び出し側は登録し直す）。
    fn instance_heartbeat(&self, instance_id: &str, at: OffsetDateTime)
    -> Result<bool, StoreError>;
    /// `instance_id` の行に `handoff_requested_at` を書く（既に入っていれば**上書きしない**。
    /// 引き継ぎの要求は 1 回だけ）。書いたら `Ok(true)`。
    fn instance_request_handoff(
        &self,
        instance_id: &str,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// 役割を変える（`heartbeat_at` も同時に更新する）。行が無ければ `Ok(false)`。
    fn instance_set_role(
        &self,
        instance_id: &str,
        role: InstanceRole,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// 手元の run が 0 になったので `drained_at` を書く（役割は `draining` のまま）。行が無ければ `Ok(false)`。
    fn instance_mark_drained(
        &self,
        instance_id: &str,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError>;
    /// 全インスタンスを `started_at` 昇順（同時刻は `instance_id` 昇順）で返す。
    fn instance_list(&self) -> Result<Vec<DaemonInstance>, StoreError>;
    /// 1 行消す。無い id は `Ok(false)`。
    fn instance_delete(&self, instance_id: &str) -> Result<bool, StoreError>;
    /// 終わった・死んだ他のインスタンスの行を消す（`keep` は消さない）。対象は `drained_at` が入っている
    /// 行と、`heartbeat_at` が `heartbeat_before` より古い行。消した `instance_id` を昇順で返す。
    fn instance_delete_stale(
        &self,
        keep: &str,
        heartbeat_before: OffsetDateTime,
    ) -> Result<Vec<String>, StoreError>;
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

/// ADR-0043 D1: 既存の作業場所を写すとき・API が `kind` を省略したときの `kind` の決め方
/// （決定的。LLM は使わない）。
///
/// - `Local` — `<path>/.git` があれば `git`（ディレクトリでも worktree の gitfile でもよい）、無ければ `dir`
/// - `Remote` — `git`。クラスタ側のファイルシステムは celeris からは見えないが、ADR-0018 / ADR-0019 の
///   リモートの作業場所は git リポジトリを前提にした同期をするため。違えば人が `PATCH /repos/{id}` で直す
pub fn detect_repo_kind(location: &WorkspaceSpec) -> RepoKind {
    match location {
        WorkspaceSpec::Local { path, .. } => {
            if path.join(".git").exists() {
                RepoKind::Git
            } else {
                RepoKind::Dir
            }
        }
        WorkspaceSpec::Remote { .. } => RepoKind::Git,
    }
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Draft => "draft",
        Status::Ready => "ready",
        Status::Running => "running",
        Status::Blocked => "blocked",
        Status::Reviewing => "reviewing",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
    }
}

fn kind_str(k: TaskKind) -> &'static str {
    match k {
        TaskKind::Plan => "plan",
        TaskKind::Execute => "execute",
        TaskKind::Review => "review",
        TaskKind::Approval => "approval",
    }
}

/// ADR-0044 D4: `worker_hint.tier` の JSON 表現（`json_extract` の照合に使う）。
fn tier_str(t: crate::model::Tier) -> &'static str {
    match t {
        crate::model::Tier::Frontier => "frontier",
        crate::model::Tier::Standard => "standard",
        crate::model::Tier::Cheap => "cheap",
    }
}

pub(crate) fn format_rfc3339(t: OffsetDateTime) -> Result<String, StoreError> {
    Ok(t.format(&Rfc3339)?)
}

pub(crate) fn parse_rfc3339(s: &str) -> Result<OffsetDateTime, StoreError> {
    Ok(OffsetDateTime::parse(s, &Rfc3339)?)
}

impl SqliteStore {
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn, &StoreOptions::default())
    }

    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with(path, StoreOptions::default())
    }

    /// ADR-0013 D5: `path` の DB を `options` の PRAGMA 設定で開き、マイグレーションを適用する。
    /// DB の版数がこのバイナリの知る `SCHEMA_VERSION` より新しければ `StoreError::SchemaTooNew` を
    /// 返し、DB には何も書かない。
    pub fn open_with(path: &Path, options: StoreOptions) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn, &options)
    }

    // ---- ADR-0046 D7（Phase 59）: `celerisctl org migrate-v2` のための低レベルの書き換え。
    // 通常の経路（`org_upsert` / `update_task`）は状態機械と検証を通すが、移行は「id の付け替え」だけを
    // まとめて行うので、ここに専用の関数を置く（`celerisctl` からしか呼ばない）。

    /// ノード id を参照している行を `from` → `to` に書き換え、書き換えた行の主キーを返す。
    /// `tasks` は `assignee` 列と `json`（正本）の両方を直す。
    pub fn migrate_node_refs(
        &self,
        table: &str,
        column: &str,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, StoreError> {
        if !is_known_node_ref(table, column) {
            return Err(StoreError::Invalid(format!(
                "unknown node reference: {table}.{column}"
            )));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids: Vec<String> = {
            let sql = format!("SELECT id FROM {table} WHERE {column} = ?1");
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(params![from], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        for id in &ids {
            set_node_ref(&tx, table, column, id, to)?;
        }
        tx.commit()?;
        Ok(ids)
    }

    /// `migrate_node_refs` の逆（`--rollback`）。1 行だけを元の値に戻す。
    pub fn restore_node_ref(
        &self,
        table: &str,
        column: &str,
        id: &str,
        old: &str,
    ) -> Result<(), StoreError> {
        if !is_known_node_ref(table, column) {
            return Err(StoreError::Invalid(format!(
                "unknown node reference: {table}.{column}"
            )));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        set_node_ref(&tx, table, column, id, old)?;
        tx.commit()?;
        Ok(())
    }

    /// `org_nodes` をまるごと差し替える（親が先に来る並びで渡すこと）。1 トランザクション。
    pub fn replace_org_nodes(&self, nodes: &[OrgNode]) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM org_nodes", [])?;
        let mut existing: Vec<OrgNode> = Vec::with_capacity(nodes.len());
        for node in nodes {
            crate::org::validate_upsert(&existing, node)?;
            tx.execute(
                "INSERT INTO org_nodes (id, parent_id, name, kind, genre, brief, position, created_at, updated_at, \
                 profile_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    node.id,
                    node.parent_id,
                    node.name,
                    node.kind.as_str(),
                    node.genre,
                    node.brief,
                    node.position,
                    format_rfc3339(node.created_at)?,
                    format_rfc3339(node.updated_at)?,
                    profile_json(&node.profile)?,
                ],
            )?;
            existing.push(node.clone());
        }
        tx.commit()?;
        Ok(())
    }

    /// 現在の DB のスキーマ版数（`schema_migrations` の最大 `version`。行が無ければ 0）。
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let conn = self.lock()?;
        let v: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        Ok(v as u32)
    }

    fn from_connection(mut conn: Connection, options: &StoreOptions) -> Result<Self, StoreError> {
        Self::configure_pragmas(&conn, options)?;
        Self::migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// ADR-0013 D5: WAL・busy_timeout・synchronous=NORMAL を設定する。`foreign_keys` は変えない。
    /// インメモリ DB では `journal_mode` が `memory` のまま返ることがあるが、エラーにはしない。
    fn configure_pragmas(conn: &Connection, options: &StoreOptions) -> Result<(), StoreError> {
        conn.busy_timeout(options.busy_timeout)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let _journal_mode: String =
            conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        Ok(())
    }

    fn table_exists(conn: &Connection, name: &str) -> Result<bool, StoreError> {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            params![name],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    /// ADR-0013 D5: `schema_migrations` を導入し、未適用の版を 1 つずつ 1 トランザクションで
    /// 適用する。既存 DB（`schema_migrations` が無く `tasks` がある）は版数 1 とみなす。
    fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
        let migrations_table_existed = Self::table_exists(conn, "schema_migrations")?;
        if !migrations_table_existed {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (\
                 version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
            )?;
        }

        let mut current: u32 = if migrations_table_existed {
            let v: i64 = conn.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )?;
            v as u32
        } else {
            0
        };

        if current > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }

        if current == 0 && !migrations_table_existed && Self::table_exists(conn, "tasks")? {
            // 既存 DB（Phase 1〜8 で作られた、schema_migrations の無い DB）は版数 1 が
            // 適用済みとみなす。0001_init.sql は再実行しない。
            Self::mark_migration_applied(conn, 1)?;
            current = 1;
        }

        for version in (current + 1)..=SCHEMA_VERSION {
            Self::apply_migration_version(conn, version)?;
        }

        Ok(())
    }

    fn migration_sql(version: u32) -> Result<&'static str, StoreError> {
        match version {
            1 => Ok(MIGRATION_0001),
            2 => Ok(MIGRATION_0002),
            3 => Ok(MIGRATION_0003),
            4 => Ok(MIGRATION_0004),
            5 => Ok(MIGRATION_0005),
            6 => Ok(MIGRATION_0006),
            7 => Ok(MIGRATION_0007),
            8 => Ok(MIGRATION_0008),
            9 => Ok(MIGRATION_0009),
            10 => Ok(MIGRATION_0010),
            11 => Ok(MIGRATION_0011),
            12 => Ok(MIGRATION_0012),
            13 => Ok(MIGRATION_0013),
            14 => Ok(MIGRATION_0014),
            15 => Ok(MIGRATION_0015),
            16 => Ok(MIGRATION_0016),
            other => Err(StoreError::Invalid(format!(
                "unknown migration version: {other}"
            ))),
        }
    }

    fn apply_migration_version(conn: &mut Connection, version: u32) -> Result<(), StoreError> {
        let sql = Self::migration_sql(version)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        // ADR-0043 D1（Phase 52）: 既存の `projects.workspace` を `is_primary = 1` のリポジトリ 1 件に
        // 写す。id が ULID で、`kind` の判定にファイルシステムを見る必要があるので SQL では書けない
        // （migration 0012 のコメント参照）。同じトランザクションの中で 1 度だけ走る。
        if version == 12 {
            Self::backfill_project_repos(&tx)?;
        }
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, ts],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn mark_migration_applied(conn: &Connection, version: u32) -> Result<(), StoreError> {
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, ts],
        )?;
        Ok(())
    }

    // ---- ADR-0033 D1/D2: 組織・案件の行と型の間の変換（rusqlite の行変換は `rusqlite::Error` しか
    // 返せないので、解析の失敗は内側の `Result<_, StoreError>` に載せて返す）----

    fn org_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<OrgNode, StoreError>> {
        let id: String = row.get(0)?;
        let kind_col: String = row.get(3)?;
        let created_at: String = row.get(7)?;
        let updated_at: String = row.get(8)?;
        // ADR-0046 D1（Phase 59）: 10 列目は `profile_json`（NULL なら空の profile）。
        let profile_col: Option<String> = row.get(9)?;
        let Some(kind) = OrgKind::parse(&kind_col) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid org node kind in org_nodes: {kind_col}"
            ))));
        };
        let profile = match profile_col.as_deref() {
            Some(raw) if !raw.trim().is_empty() => {
                match serde_json::from_str::<crate::profile::Profile>(raw) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        return Ok(Err(StoreError::Invalid(format!(
                            "invalid org profile for {id}: {e}"
                        ))));
                    }
                }
            }
            _ => crate::profile::Profile::default(),
        };
        Ok((|| {
            Ok(OrgNode {
                id,
                parent_id: row.get(1)?,
                name: row.get(2)?,
                kind,
                genre: row.get(4)?,
                brief: row.get(5)?,
                profile,
                position: row.get(6)?,
                created_at: parse_rfc3339(&created_at)?,
                updated_at: parse_rfc3339(&updated_at)?,
            })
        })())
    }

    fn org_list_tx(conn: &Connection) -> Result<Vec<OrgNode>, StoreError> {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id, name, kind, genre, brief, position, created_at, updated_at, profile_json \
             FROM org_nodes ORDER BY position ASC, id ASC",
        )?;
        let rows = stmt.query_map([], Self::org_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn project_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Project, StoreError>> {
        let id: String = row.get(0)?;
        let status_col: String = row.get(3)?;
        let created_at: String = row.get(5)?;
        let updated_at: String = row.get(6)?;
        // ADR-0039 D1 / ADR-0043 D1: 7 列目は **primary のリポジトリの `location_json`**、無ければ
        // 従来の `projects.workspace` 列（`COALESCE`。導入前の行と作業場所を決めていない案件は NULL）。
        let workspace_col: Option<String> = row.get(7)?;
        // ADR-0044 D6（Phase 55）: 8 列目 `archived_at`、9 列目 `paused_from`。
        let archived_at_col: Option<String> = row.get(8)?;
        let paused_from_col: Option<String> = row.get(9)?;
        let (Ok(id), Some(status)) = (id.parse::<ProjectId>(), ProjectStatus::parse(&status_col))
        else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid project row: id={id} status={status_col}"
            ))));
        };
        let paused_from = match paused_from_col.as_deref() {
            Some(raw) => match ProjectStatus::parse(raw) {
                Some(parsed) => Some(parsed),
                None => {
                    return Ok(Err(StoreError::Invalid(format!(
                        "invalid project paused_from for {id}: {raw}"
                    ))));
                }
            },
            None => None,
        };
        let workspace = match workspace_col.as_deref() {
            Some(raw) => match serde_json::from_str::<WorkspaceSpec>(raw) {
                Ok(spec) => Some(spec),
                Err(e) => {
                    return Ok(Err(StoreError::Invalid(format!(
                        "invalid project workspace for {id}: {e}"
                    ))));
                }
            },
            None => None,
        };
        Ok((|| {
            Ok(Project {
                id,
                title: row.get(1)?,
                request: row.get(2)?,
                status,
                secretary_summary: row.get(4)?,
                workspace,
                archived_at: match archived_at_col.as_deref() {
                    Some(raw) => Some(parse_rfc3339(raw)?),
                    None => None,
                },
                paused_from,
                created_at: parse_rfc3339(&created_at)?,
                updated_at: parse_rfc3339(&updated_at)?,
            })
        })())
    }

    /// ADR-0039 D1: 案件の作業場所を DB の列に入れる形（JSON か NULL）にする。
    fn project_workspace_column(
        workspace: Option<&WorkspaceSpec>,
    ) -> Result<Option<String>, StoreError> {
        match workspace {
            Some(spec) => serde_json::to_string(spec)
                .map(Some)
                .map_err(|e| StoreError::Invalid(format!("cannot serialize workspace: {e}"))),
            None => Ok(None),
        }
    }

    // ---- ADR-0043 D1（Phase 52）: 案件のリポジトリ（`project_repos`）----

    /// `project_repos` の 1 行。
    fn repo_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<ProjectRepo, StoreError>> {
        let id: String = row.get(0)?;
        let project_id: String = row.get(1)?;
        let kind_col: String = row.get(3)?;
        let location_col: String = row.get(4)?;
        let run_col: String = row.get(7)?;
        let created_at: String = row.get(9)?;
        let (Ok(id), Ok(project_id), Some(kind), Some(run)) = (
            id.parse::<RepoId>(),
            project_id.parse::<ProjectId>(),
            RepoKind::parse(&kind_col),
            RepoRun::parse(&run_col),
        ) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid project_repos row: id={id} project_id={project_id} kind={kind_col} run={run_col}"
            ))));
        };
        let location = match serde_json::from_str::<WorkspaceSpec>(&location_col) {
            Ok(spec) => spec,
            Err(e) => {
                return Ok(Err(StoreError::Invalid(format!(
                    "invalid project_repos location for {id}: {e}"
                ))));
            }
        };
        let sync_col: Option<String> = row.get(6)?;
        let sync = match sync_col.as_deref() {
            None => None,
            Some(raw) => match RepoSync::parse(raw) {
                Some(v) => Some(v),
                None => {
                    return Ok(Err(StoreError::Invalid(format!(
                        "invalid project_repos sync for {id}: {raw:?}"
                    ))));
                }
            },
        };
        Ok((|| {
            let is_primary: i64 = row.get(8)?;
            Ok(ProjectRepo {
                id,
                project_id,
                name: row.get(2)?,
                kind,
                location,
                default_branch: row.get(5)?,
                sync,
                run,
                is_primary: is_primary != 0,
                created_at: parse_rfc3339(&created_at)?,
            })
        })())
    }

    const REPO_COLUMNS: &'static str = "id, project_id, name, kind, location_json, default_branch, sync, run, is_primary, created_at";

    fn repo_list_tx(
        conn: &Connection,
        project_id: ProjectId,
    ) -> Result<Vec<ProjectRepo>, StoreError> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM project_repos WHERE project_id = ?1 \
             ORDER BY is_primary DESC, created_at ASC, id ASC",
            Self::REPO_COLUMNS
        ))?;
        let rows = stmt.query_map(params![project_id.to_string()], Self::repo_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn repo_get_tx(conn: &Connection, id: RepoId) -> Result<Option<ProjectRepo>, StoreError> {
        conn.query_row(
            &format!(
                "SELECT {} FROM project_repos WHERE id = ?1",
                Self::REPO_COLUMNS
            ),
            params![id.to_string()],
            Self::repo_row,
        )
        .optional()?
        .transpose()
    }

    /// 行を 1 件書く（INSERT OR REPLACE）。検証は呼び出し側で済ませておくこと。
    fn repo_write_tx(conn: &Connection, repo: &ProjectRepo) -> Result<(), StoreError> {
        let location = serde_json::to_string(&repo.location)
            .map_err(|e| StoreError::Invalid(format!("cannot serialize repo location: {e}")))?;
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO project_repos ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                Self::REPO_COLUMNS
            ),
            params![
                repo.id.to_string(),
                repo.project_id.to_string(),
                repo.name,
                repo.kind.as_str(),
                location,
                repo.default_branch,
                repo.sync.map(|s| s.as_str()),
                repo.run.as_str(),
                i64::from(repo.is_primary),
                format_rfc3339(repo.created_at)?,
            ],
        )?;
        Ok(())
    }

    /// 1 案件に primary は 1 つ（ADR-0043 D1）。`keep` 以外の `is_primary` を落とす。
    fn repo_clear_other_primaries_tx(
        conn: &Connection,
        project_id: ProjectId,
        keep: RepoId,
    ) -> Result<(), StoreError> {
        conn.execute(
            "UPDATE project_repos SET is_primary = 0 WHERE project_id = ?1 AND id <> ?2",
            params![project_id.to_string(), keep.to_string()],
        )?;
        Ok(())
    }

    /// `projects.workspace` 列を primary のリポジトリの写しに保つ（migration 0012 のコメント参照。
    /// ADR-0043 D1 は「書かない」だが、N-1 互換〈旧バイナリが新スキーマを読む〉のために写しを残す）。
    fn sync_project_workspace_tx(
        conn: &Connection,
        project_id: ProjectId,
    ) -> Result<(), StoreError> {
        let primary: Option<String> = conn
            .query_row(
                "SELECT location_json FROM project_repos WHERE project_id = ?1 AND is_primary = 1 \
                 ORDER BY created_at ASC, id ASC LIMIT 1",
                params![project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        // primary が消えたら NULL に戻す（案件を「作業場所なし」に戻したとき）。
        conn.execute(
            "UPDATE projects SET workspace = ?1 WHERE id = ?2",
            params![primary, project_id.to_string()],
        )?;
        Ok(())
    }

    /// そのリポジトリを参照している**未終端**のタスク（ADR-0043 D1: `DELETE /repos/{id}` の 409）。
    /// 索引は migration 0012 で足した `tasks.repos_json`（ULID は一意なので部分一致で足りる）。
    fn repo_active_tasks_tx(conn: &Connection, id: RepoId) -> Result<Vec<TaskId>, StoreError> {
        let mut stmt = conn.prepare(&format!(
            "SELECT id FROM tasks WHERE {} AND repos_json IS NOT NULL AND repos_json LIKE ?1 \
             ORDER BY id ASC",
            Self::NON_TERMINAL_SQL
        ))?;
        let pattern = format!("%{id}%");
        let rows = stmt.query_map(params![pattern], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(Self::parse_id(&row?)?);
        }
        Ok(out)
    }

    // ---- ADR-0043 D5（Phase 54）: 変更の取り込み（`task_integrations`）----

    const INTEGRATION_COLUMNS: &'static str = "id, task_id, repo_id, repo_name, method, state, pr_number, \
         pr_url, merged_at, detail, created_at, updated_at";

    fn integration_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Result<TaskIntegration, StoreError>> {
        let id: String = row.get(0)?;
        let task_id: String = row.get(1)?;
        let repo_id: Option<String> = row.get(2)?;
        let method_col: String = row.get(4)?;
        let state_col: String = row.get(5)?;
        let (Ok(id), Ok(task_id), Some(method), Some(state)) = (
            id.parse::<IntegrationId>(),
            task_id.parse::<TaskId>(),
            IntegrationMethod::parse(&method_col),
            IntegrationState::parse(&state_col),
        ) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid task_integrations row: id={id} task_id={task_id} method={method_col} state={state_col}"
            ))));
        };
        let repo_id = match repo_id.as_deref() {
            None => None,
            Some(raw) => match raw.parse::<RepoId>() {
                Ok(v) => Some(v),
                Err(_) => {
                    return Ok(Err(StoreError::Invalid(format!(
                        "invalid task_integrations repo_id for {id}: {raw:?}"
                    ))));
                }
            },
        };
        Ok((|| {
            let merged_at: Option<String> = row.get(8)?;
            let created_at: String = row.get(10)?;
            let updated_at: String = row.get(11)?;
            Ok(TaskIntegration {
                id,
                task_id,
                repo_id,
                repo: row.get(3)?,
                method,
                state,
                pr_number: row.get(6)?,
                pr_url: row.get(7)?,
                merged_at: merged_at.as_deref().map(parse_rfc3339).transpose()?,
                detail: row.get(9)?,
                created_at: parse_rfc3339(&created_at)?,
                updated_at: parse_rfc3339(&updated_at)?,
            })
        })())
    }

    fn integration_put_tx(
        conn: &Connection,
        integration: &TaskIntegration,
    ) -> Result<(), StoreError> {
        conn.execute(
            &format!(
                "INSERT OR REPLACE INTO task_integrations ({}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                Self::INTEGRATION_COLUMNS
            ),
            params![
                integration.id.to_string(),
                integration.task_id.to_string(),
                integration.repo_id.map(|r| r.to_string()),
                integration.repo,
                integration.method.as_str(),
                integration.state.as_str(),
                integration.pr_number,
                integration.pr_url,
                integration.merged_at.map(format_rfc3339).transpose()?,
                integration.detail,
                format_rfc3339(integration.created_at)?,
                format_rfc3339(integration.updated_at)?,
            ],
        )?;
        Ok(())
    }

    fn integration_query_tx(
        conn: &Connection,
        where_sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<TaskIntegration>, StoreError> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM task_integrations WHERE {where_sql} ORDER BY created_at DESC, id DESC",
            Self::INTEGRATION_COLUMNS
        ))?;
        let rows = stmt.query_map(params, Self::integration_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    /// migration 0012 の写し（ADR-0043 D1）。`projects.workspace` がある案件ごとに `is_primary = 1` の
    /// リポジトリを 1 件作る。`kind` は「パスが git なら git、でなければ dir」だが、SQL からは
    /// ファイルシステムを見られないのでここ（Rust）で決める。
    fn backfill_project_repos(conn: &Connection) -> Result<(), StoreError> {
        let mut stmt = conn.prepare(
            "SELECT id, workspace, created_at FROM projects WHERE workspace IS NOT NULL",
        )?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for (project_id, workspace, created_at) in rows {
            let Ok(project_id) = project_id.parse::<ProjectId>() else {
                continue;
            };
            let Ok(location) = serde_json::from_str::<WorkspaceSpec>(&workspace) else {
                continue;
            };
            let repo = ProjectRepo {
                id: RepoId::new(),
                project_id,
                name: crate::repos::default_repo_name(&location),
                kind: detect_repo_kind(&location),
                location,
                default_branch: None,
                sync: None,
                run: RepoRun::Auto,
                is_primary: true,
                created_at: parse_rfc3339(&created_at)
                    .unwrap_or_else(|_| OffsetDateTime::now_utc()),
            };
            Self::repo_write_tx(conn, &repo)?;
        }
        Ok(())
    }

    /// ADR-0044 D6（Phase 55）: いま dispatch を止めている案件の id（`paused` / `cancelled` /
    /// アーカイブ済み）。`ready_tasks` が 1 tick に 1 回だけ引く。
    fn halted_projects_locked(
        conn: &Connection,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        let mut stmt = conn.prepare(
            "SELECT id FROM projects WHERE status IN ('paused', 'cancelled') OR archived_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// ADR-0044 D6（Phase 55）: いま dispatch を止めている途中目標の id（`paused` / `cancelled`）。
    fn halted_milestones_locked(
        conn: &Connection,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        let mut stmt =
            conn.prepare("SELECT id FROM milestones WHERE status IN ('paused', 'cancelled')")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    fn milestone_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Milestone, StoreError>> {
        let id: String = row.get(0)?;
        let project_id: String = row.get(1)?;
        let status_col: String = row.get(5)?;
        let created_at: String = row.get(6)?;
        let updated_at: String = row.get(7)?;
        // ADR-0044 D6（Phase 55）: 8 列目 `paused_from`。
        let paused_from_col: Option<String> = row.get(8)?;
        let (Ok(id), Ok(project_id), Some(status)) = (
            id.parse::<MilestoneId>(),
            project_id.parse::<ProjectId>(),
            MilestoneStatus::parse(&status_col),
        ) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid milestone row: id={id} project_id={project_id} status={status_col}"
            ))));
        };
        let paused_from = match paused_from_col.as_deref() {
            Some(raw) => match MilestoneStatus::parse(raw) {
                Some(parsed) => Some(parsed),
                None => {
                    return Ok(Err(StoreError::Invalid(format!(
                        "invalid milestone paused_from for {id}: {raw}"
                    ))));
                }
            },
            None => None,
        };
        Ok((|| {
            Ok(Milestone {
                id,
                project_id,
                seq: row.get(2)?,
                title: row.get(3)?,
                description: row.get(4)?,
                status,
                paused_from,
                created_at: parse_rfc3339(&created_at)?,
                updated_at: parse_rfc3339(&updated_at)?,
            })
        })())
    }

    /// ADR-0033 D4（Phase 24）: `messages` の 1 行。
    fn message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Message, StoreError>> {
        let id: String = row.get(0)?;
        let project_id: Option<String> = row.get(2)?;
        let role_col: String = row.get(3)?;
        let created_at: String = row.get(6)?;
        let (Ok(id), Some(role)) = (id.parse::<MessageId>(), MessageRole::parse(&role_col)) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid message row: id={id} role={role_col}"
            ))));
        };
        let project_id = match project_id {
            Some(raw) => match raw.parse::<ProjectId>() {
                Ok(p) => Some(p),
                Err(_) => {
                    return Ok(Err(StoreError::Invalid(format!(
                        "invalid message row: id={id} project_id={raw}"
                    ))));
                }
            },
            None => None,
        };
        // migration 0007（R4）: 対話用タスクの id。導入前の行は NULL。
        let task_id: Option<String> = row.get(7)?;
        let task_id = match task_id {
            Some(raw) => match raw.parse::<TaskId>() {
                Ok(t) => Some(t),
                Err(_) => {
                    return Ok(Err(StoreError::Invalid(format!(
                        "invalid message row: id={id} task_id={raw}"
                    ))));
                }
            },
            None => None,
        };
        Ok((|| {
            Ok(Message {
                id,
                node_id: row.get(1)?,
                project_id,
                role,
                text: row.get(4)?,
                run_id: row.get(5)?,
                task_id,
                created_at: parse_rfc3339(&created_at)?,
            })
        })())
    }

    /// ADR-0033 D3/D5: `report.rs`・`approval.rs`（`reports`・`approvals`・`standing_rules` 表の SQL）
    /// も同じ接続を使うので crate 内に公開する。
    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    fn row_to_task(json: String) -> Result<Task, StoreError> {
        Ok(serde_json::from_str(&json)?)
    }

    /// 既にロック済みの connection を使ってタスクを取得する内部ヘルパー。
    /// `get()` が再度 Mutex をロックしないようにするために分離してある。
    fn get_locked(conn: &Connection, id: TaskId) -> Result<Option<Task>, StoreError> {
        let json: Option<String> = conn
            .query_row(
                "SELECT json FROM tasks WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match json {
            Some(j) => Ok(Some(Self::row_to_task(j)?)),
            None => Ok(None),
        }
    }

    /// `insert` の本体（トランザクション内でも使えるよう `Connection` を受ける）。
    fn insert_tx(conn: &Connection, task: &Task) -> Result<(), StoreError> {
        let json = serde_json::to_string(task)?;
        let created_at = format_rfc3339(task.created_at)?;
        let updated_at = format_rfc3339(task.updated_at)?;
        let (lease_worker_run_id, lease_expires_at) = match &task.lease {
            Some(lease) => (
                Some(lease.worker_run_id.clone()),
                Some(format_rfc3339(lease.expires_at)?),
            ),
            None => (None, None),
        };
        // ADR-0043 D2: `repos_json` は索引（正は `json` の中の `repos`）。空なら NULL。
        let repos_json = if task.repos.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&task.repos)?)
        };
        conn.execute(
            "INSERT INTO tasks (id, status, kind, parent_id, priority, created_at, \
             lease_worker_run_id, lease_expires_at, json, title, updated_at, objective, genre, \
             project_id, milestone_id, assignee, repos_json, labels_json, category, skills_json, mode) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                task.id.to_string(),
                status_str(task.status),
                kind_str(task.kind),
                task.parent_id.map(|p| p.to_string()),
                task.priority,
                created_at,
                lease_worker_run_id,
                lease_expires_at,
                json,
                task.title,
                updated_at,
                // objective / genre は作成後に変わらないので、列を書くのは挿入時だけ（ADR-0014 D2, ADR-0027 D1）。
                task.objective,
                task.genre,
                // ADR-0033 D2: 案件・途中目標・担当も作成後に変わらないので、列を書くのは挿入時だけ。
                task.project_id.map(|p| p.to_string()),
                task.milestone_id.map(|m| m.to_string()),
                task.assignee.clone(),
                repos_json,
                // ADR-0044 D3（Phase 53）: ラベルと種類は `PATCH /tasks/{id}` で変わるので、
                // `update_task_tx` が同じ 2 列を書き直す。
                serde_json::to_string(&task.labels)?,
                task.category.as_str(),
                // ADR-0046 D2 / D4（Phase 59）: skills と mode も `PATCH /tasks/{id}` で変わるので、
                // `update_task_tx` が同じ 2 列を書き直す。
                serde_json::to_string(&task.skills)?,
                task.mode.as_str(),
            ],
        )?;
        Ok(())
    }

    /// ADR-0044 D1（Phase 53）: `PATCH /tasks/{id}` の書き戻し。`json`（正本）と、絞り込みのための
    /// 写しの列を**全部**書き直す（挿入時にしか書いていなかった `objective` / `genre` / `project_id` /
    /// `milestone_id` / `assignee` も、編集で変わりうるのでここで揃える）。状態機械は通らない
    /// （`status` / `attempts` / `lease` は触らない）。
    fn update_task_tx(tx: &Connection, task: &Task) -> Result<(), StoreError> {
        let json = serde_json::to_string(task)?;
        let updated_at = format_rfc3339(task.updated_at)?;
        tx.execute(
            "UPDATE tasks SET json = ?1, title = ?2, updated_at = ?3, objective = ?4, genre = ?5, \
             priority = ?6, parent_id = ?7, project_id = ?8, milestone_id = ?9, assignee = ?10, \
             labels_json = ?11, category = ?12, skills_json = ?13, mode = ?14 WHERE id = ?15",
            params![
                json,
                task.title,
                updated_at,
                task.objective,
                task.genre,
                task.priority,
                task.parent_id.map(|p| p.to_string()),
                task.project_id.map(|p| p.to_string()),
                task.milestone_id.map(|m| m.to_string()),
                task.assignee.clone(),
                serde_json::to_string(&task.labels)?,
                task.category.as_str(),
                serde_json::to_string(&task.skills)?,
                task.mode.as_str(),
                task.id.to_string(),
            ],
        )?;
        Ok(())
    }

    /// `apply_transition_with_events` の本体（ADR-0004 D1 / ADR-0005 D4）。`tx` 内で任意のトリガーを
    /// 検証し、tasks の更新と Event::Transitioned (+ extra_events) の追記を行う。commit は呼び出し側。
    fn apply_transition_tx(
        tx: &Connection,
        task_id: TaskId,
        trigger: Trigger,
        extra_events: Vec<Event>,
    ) -> Result<Outcome, StoreError> {
        // ADR-0004 D1 / ADR-0005 D4: 任意のトリガーを検証し、tasks の更新と
        // Event::Transitioned (+ extra_events) の追記を単一トランザクションで行う。

        let json: Option<String> = tx
            .query_row(
                "SELECT json FROM tasks WHERE id = ?1",
                params![task_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let json = match json {
            Some(j) => j,
            None => return Err(StoreError::Invalid(format!("task not found: {task_id}"))),
        };
        let mut task = Self::row_to_task(json)?;

        let view = StateView {
            kind: task.kind,
            status: task.status,
            attempts: task.attempts,
            max_retries: task.budget.max_retries,
        };
        let outcome = transition(&view, &trigger)?;

        let now = OffsetDateTime::now_utc();
        // ADR-0002 D1: running から出る全遷移でリースを解放する。
        let leaving_running = view.status == Status::Running && outcome.next != Status::Running;
        task.status = outcome.next;
        task.attempts = outcome.attempts;
        task.updated_at = now;
        if leaving_running {
            task.lease = None;
        }

        let new_json = serde_json::to_string(&task)?;
        let updated_at_str = format_rfc3339(task.updated_at)?;
        if leaving_running {
            tx.execute(
                "UPDATE tasks SET status = ?1, lease_worker_run_id = NULL, \
                 lease_expires_at = NULL, json = ?2, title = ?3, updated_at = ?4 WHERE id = ?5",
                params![
                    status_str(task.status),
                    new_json,
                    task.title,
                    updated_at_str,
                    task_id.to_string()
                ],
            )?;
        } else {
            tx.execute(
                "UPDATE tasks SET status = ?1, json = ?2, title = ?3, updated_at = ?4 WHERE id = ?5",
                params![status_str(task.status), new_json, task.title, updated_at_str, task_id.to_string()],
            )?;
        }

        let mut next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| row.get(0),
        )?;

        let transitioned = Event::Transitioned {
            from: view.status,
            to: outcome.next,
            reason: outcome.reason.to_string(),
        };
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        tx.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![
                task_id.to_string(),
                next_seq,
                ts,
                serde_json::to_string(&transitioned)?
            ],
        )?;
        next_seq += 1;

        for event in extra_events {
            let ts = format_rfc3339(OffsetDateTime::now_utc())?;
            tx.execute(
                "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    task_id.to_string(),
                    next_seq,
                    ts,
                    serde_json::to_string(&event)?
                ],
            )?;
            next_seq += 1;
        }

        Self::cascade_after_transition_tx(tx, &task, view.status, outcome.next)?;

        Ok(outcome)
    }

    /// 終端化に伴う伝播（ADR-0010 D2）。呼び出し元と同一トランザクションで、再帰的に行う。
    /// 1. `Approval` が failed/cancelled → 終端でない直接の子を `Cancel`（ADR-0008 D1 を reject 以外にも拡張）
    /// 2. `Approval` 以外が終端 → 終端でない直接の `Approval` 子を `Cancel`（P-37）
    /// 3. failed/cancelled → 終端でない後続（`depends_on` に含むタスク）を `DependencyFailed`（P-9、推移的）
    fn cascade_after_transition_tx(
        tx: &Connection,
        task: &Task,
        from: Status,
        to: Status,
    ) -> Result<(), StoreError> {
        if from.is_terminal() || !to.is_terminal() {
            return Ok(());
        }
        let unsuccessful = matches!(to, Status::Failed | Status::Cancelled);
        if task.kind == TaskKind::Approval {
            if unsuccessful {
                for child in Self::non_terminal_children_tx(tx, task.id, None)? {
                    Self::transition_if_non_terminal_tx(tx, child, Trigger::Cancel)?;
                }
            }
        } else {
            for child in Self::non_terminal_children_tx(tx, task.id, Some(TaskKind::Approval))? {
                Self::transition_if_non_terminal_tx(tx, child, Trigger::Cancel)?;
            }
        }
        if unsuccessful {
            for dependent in Self::non_terminal_dependents_tx(tx, task.id)? {
                // P-78（ADR-0033 D4 / Phase 28）: 対話タスクは `DependencyFailed` の対象から外す
                // （直列化の順番待ちだけなので、前の対話タスクの失敗を理由に次を `cancelled` にしない。
                // `ready_tasks` 側が「終端に達していれば進めてよい」を見る）。
                if let Some(dep_task) = Self::get_locked(tx, dependent)?
                    && is_conversation(&dep_task)
                {
                    continue;
                }
                Self::transition_if_non_terminal_tx(tx, dependent, Trigger::DependencyFailed)?;
            }
        }
        Ok(())
    }

    /// 伝播の途中で既に終端になったタスク（例: 子でもあり後続でもある）は飛ばす。
    fn transition_if_non_terminal_tx(
        tx: &Connection,
        id: TaskId,
        trigger: Trigger,
    ) -> Result<(), StoreError> {
        match Self::get_locked(tx, id)? {
            Some(t) if !t.status.is_terminal() => {
                Self::apply_transition_tx(tx, id, trigger, vec![])?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    const NON_TERMINAL_SQL: &'static str = "status NOT IN ('done', 'failed', 'cancelled')";

    fn parse_id(id_str: &str) -> Result<TaskId, StoreError> {
        id_str
            .parse()
            .map_err(|_| StoreError::Invalid(format!("invalid task id in tasks table: {id_str}")))
    }

    /// `parent_id` の直接の子のうち終端でないもの（`kind` 指定があればその kind だけ）。
    fn non_terminal_children_tx(
        tx: &Connection,
        parent_id: TaskId,
        kind: Option<TaskKind>,
    ) -> Result<Vec<TaskId>, StoreError> {
        let sql = format!(
            "SELECT id FROM tasks WHERE parent_id = ?1 AND {} AND (?2 IS NULL OR kind = ?2)",
            Self::NON_TERMINAL_SQL
        );
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params![parent_id.to_string(), kind.map(kind_str)], |row| {
            row.get::<_, String>(0)
        })?;
        let ids: Vec<String> = rows.collect::<Result<_, _>>()?;
        ids.iter().map(|s| Self::parse_id(s)).collect()
    }

    /// `depends_on` に `dep_id` を含む、終端でないタスク。JSON 列を `LIKE` で絞ってから型で確認する。
    fn non_terminal_dependents_tx(
        tx: &Connection,
        dep_id: TaskId,
    ) -> Result<Vec<TaskId>, StoreError> {
        let sql = format!(
            "SELECT json FROM tasks WHERE {} AND json LIKE ?1",
            Self::NON_TERMINAL_SQL
        );
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params![format!("%{dep_id}%")], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            let t = Self::row_to_task(row?)?;
            if t.id != dep_id && t.depends_on.contains(&dep_id) {
                out.push(t.id);
            }
        }
        Ok(out)
    }

    /// `depends_on` に `dep_id` を含むタスク（状態を問わない）。Phase 31（やり直し）が使う。
    fn dependents_of_tx(tx: &Connection, dep_id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        let mut stmt = tx.prepare("SELECT json FROM tasks WHERE json LIKE ?1")?;
        let rows = stmt.query_map(params![format!("%{dep_id}%")], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            let t = Self::row_to_task(row?)?;
            if t.id != dep_id && t.depends_on.contains(&dep_id) {
                out.push(t.id);
            }
        }
        Ok(out)
    }

    /// `task_id` の最後の `Event::Transitioned` の `reason`。無ければ `None`。Phase 31 が「`cancelled` が
    /// `dependency_failed` 由来か」を見分けるのに使う。
    fn last_transitioned_reason_tx(
        tx: &Connection,
        task_id: TaskId,
    ) -> Result<Option<String>, StoreError> {
        let mut stmt = tx.prepare(
            "SELECT json FROM events WHERE task_id = ?1 AND json LIKE '%\"type\":\"transitioned\"%' ORDER BY seq DESC LIMIT 1",
        )?;
        let json: Option<String> = stmt
            .query_row(params![task_id.to_string()], |row| row.get(0))
            .optional()?;
        let Some(json) = json else {
            return Ok(None);
        };
        let event: Event = serde_json::from_str(&json)?;
        match event {
            Event::Transitioned { reason, .. } => Ok(Some(reason)),
            _ => Ok(None),
        }
    }

    /// `task` の `status` / `depends_on`（json 全体）/ `updated_at` を書き戻す（リースは変えない）。
    /// Phase 31 の張り替えが使う低レベルの書き込み（`transition()` を経由しない）。
    fn rewrite_task_tx(tx: &Connection, task: &Task) -> Result<(), StoreError> {
        let json = serde_json::to_string(task)?;
        let updated_at = format_rfc3339(task.updated_at)?;
        tx.execute(
            "UPDATE tasks SET status = ?1, json = ?2, title = ?3, updated_at = ?4 WHERE id = ?5",
            params![
                status_str(task.status),
                json,
                task.title,
                updated_at,
                task.id.to_string()
            ],
        )?;
        Ok(())
    }

    /// ADR-0044 D2: `task_comments` の 1 行を `TaskComment` にする。
    fn comment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<TaskComment, StoreError>> {
        let id: String = row.get(0)?;
        let task_id: String = row.get(1)?;
        let author_kind: String = row.get(2)?;
        let author: Option<String> = row.get(3)?;
        let body: String = row.get(4)?;
        let run_id: Option<String> = row.get(5)?;
        let created_at: String = row.get(6)?;
        Ok((|| {
            let Some(author_kind) = CommentAuthorKind::parse(&author_kind) else {
                return Err(StoreError::Invalid(format!(
                    "invalid author_kind in task_comments: {author_kind}"
                )));
            };
            Ok(TaskComment {
                id: id
                    .parse()
                    .map_err(|_| StoreError::Invalid(format!("invalid comment id: {id}")))?,
                task_id: Self::parse_id(&task_id)?,
                author_kind,
                author,
                body,
                run_id,
                created_at: parse_rfc3339(&created_at)?,
            })
        })())
    }

    fn append_event_tx(
        conn: &Connection,
        task_id: TaskId,
        event: &Event,
    ) -> Result<u64, StoreError> {
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| row.get(0),
        )?;
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        let json = serde_json::to_string(event)?;
        conn.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![task_id.to_string(), next_seq, ts, json],
        )?;
        Ok(next_seq as u64)
    }
}

impl TaskStore for SqliteStore {
    fn insert(&self, task: &Task) -> Result<(), StoreError> {
        let conn = self.lock()?;
        Self::insert_tx(&conn, task)
    }

    fn get(&self, id: TaskId) -> Result<Option<Task>, StoreError> {
        let conn = self.lock()?;
        Self::get_locked(&conn, id)
    }

    fn list(&self, filter: Option<Status>) -> Result<Vec<Task>, StoreError> {
        let conn = self.lock()?;
        let mut tasks = Vec::new();
        match filter {
            Some(status) => {
                let mut stmt = conn.prepare("SELECT json FROM tasks WHERE status = ?1")?;
                let rows =
                    stmt.query_map(params![status_str(status)], |row| row.get::<_, String>(0))?;
                for row in rows {
                    tasks.push(Self::row_to_task(row?)?);
                }
            }
            None => {
                let mut stmt = conn.prepare("SELECT json FROM tasks")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                for row in rows {
                    tasks.push(Self::row_to_task(row?)?);
                }
            }
        }
        Ok(tasks)
    }

    fn append_event(&self, task_id: TaskId, event: &Event) -> Result<u64, StoreError> {
        let mut conn = self.lock()?;
        // seq の採番（SELECT）と INSERT を 1 つの IMMEDIATE トランザクションにする。別接続（celerisctl / API）が同じタスクに
        // 追記しても seq が衝突せず、書き込みロックは busy_timeout で待つ（Phase 9 監査）。
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let seq = Self::append_event_tx(&tx, task_id, event)?;
        tx.commit()?;
        Ok(seq)
    }

    fn events_for(&self, task_id: TaskId) -> Result<Vec<(u64, Event)>, StoreError> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT seq, json FROM events WHERE task_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![task_id.to_string()], |row| {
            let seq: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            Ok((seq, json))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, json) = row?;
            let event: Event = serde_json::from_str(&json)?;
            events.push((seq as u64, event));
        }
        Ok(events)
    }

    fn events_for_with_global_ids(&self, task_id: TaskId) -> Result<Vec<(u64, Event)>, StoreError> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT id, json FROM events WHERE task_id = ?1 ORDER BY id ASC")?;
        let rows = stmt.query_map(params![task_id.to_string()], |row| {
            let id: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            Ok((id, json))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, json) = row?;
            let event: Event = serde_json::from_str(&json)?;
            events.push((id as u64, event));
        }
        Ok(events)
    }

    fn acquire_lease(
        &self,
        task_id: TaskId,
        worker_run_id: &str,
        ttl: StdDuration,
    ) -> Result<bool, StoreError> {
        // ADR-0002 D2: 遷移（ready -> running, Trigger::Dispatch）の結果は
        // `Event::Transitioned` と同一トランザクションで追記する。D8: `Dispatch`
        // は `kind == Approval` では無効（Approval は running に入らない）。
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let current: Option<(String, String, String)> = tx
            .query_row(
                "SELECT status, kind, json FROM tasks WHERE id = ?1",
                params![task_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;

        let (status_col, kind_col, json) = match current {
            Some(v) => v,
            None => return Ok(false),
        };
        if status_col != status_str(Status::Ready) || kind_col == kind_str(TaskKind::Approval) {
            return Ok(false);
        }

        let mut task = Self::row_to_task(json)?;

        let dur = time::Duration::new(ttl.as_secs() as i64, ttl.subsec_nanos() as i32);
        let now = OffsetDateTime::now_utc();
        let expires_at = now + dur;

        task.status = Status::Running;
        task.lease = Some(crate::model::Lease {
            worker_run_id: worker_run_id.to_string(),
            expires_at,
        });
        task.updated_at = now;

        let new_json = serde_json::to_string(&task)?;
        let expires_at_str = format_rfc3339(expires_at)?;
        let updated_at_str = format_rfc3339(now)?;

        let affected = tx.execute(
            "UPDATE tasks SET status = ?1, lease_worker_run_id = ?2, lease_expires_at = ?3, \
             json = ?4, updated_at = ?5 WHERE id = ?6 AND status = ?7",
            params![
                status_str(Status::Running),
                worker_run_id,
                expires_at_str,
                new_json,
                updated_at_str,
                task_id.to_string(),
                status_str(Status::Ready),
            ],
        )?;

        if affected != 1 {
            return Ok(false);
        }

        let event = Event::Transitioned {
            from: Status::Ready,
            to: Status::Running,
            reason: "dispatch".to_string(),
        };
        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| row.get(0),
        )?;
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        let event_json = serde_json::to_string(&event)?;
        tx.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![task_id.to_string(), next_seq, ts, event_json],
        )?;

        tx.commit()?;
        Ok(true)
    }

    fn release_lease(&self, task_id: TaskId, worker_run_id: &str) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        // 読んだ json を書き戻すので、間に別接続（celerisctl / API）の書き込みが挟まらないよう IMMEDIATE で囲む（Phase 9 監査）。
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let current: Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT lease_worker_run_id, json FROM tasks WHERE id = ?1",
                params![task_id.to_string()],
                |row| Ok((row.get(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        let (lease_worker_run_id, json) = match current {
            Some(v) => v,
            None => return Ok(()),
        };

        if lease_worker_run_id.as_deref() != Some(worker_run_id) {
            return Ok(());
        }

        let mut task = Self::row_to_task(json)?;
        task.lease = None;
        task.updated_at = OffsetDateTime::now_utc();
        let new_json = serde_json::to_string(&task)?;
        let updated_at_str = format_rfc3339(task.updated_at)?;

        tx.execute(
            "UPDATE tasks SET lease_worker_run_id = NULL, lease_expires_at = NULL, json = ?1, \
             updated_at = ?2 WHERE id = ?3 AND lease_worker_run_id = ?4",
            params![new_json, updated_at_str, task_id.to_string(), worker_run_id],
        )?;
        tx.commit()?;

        Ok(())
    }

    fn ready_tasks(&self, limit: usize) -> Result<Vec<Task>, StoreError> {
        let conn = self.lock()?;

        // ADR-0044 D6（Phase 55）: **一時停止・中止・アーカイブされた案件／途中目標のタスクは
        // dispatch しない**（`ready` のまま。状態機械は触らない）。案件の支援 run（計画・レビュー・
        // まとめ・報告の圧縮）も同じ `tasks` の行なので、この 1 か所で全部が止まる。
        // **対話（`is_conversation`）だけは例外**: 人が「なぜ止めたのか」を秘書と話せなくなるため、
        // 止まっている案件でも対話は起こす（判断は下の Rust 側。`Task` を読まないと見分けられない）。
        let halted_projects = Self::halted_projects_locked(&conn)?;
        let halted_milestones = Self::halted_milestones_locked(&conn)?;

        // ADR-0010 D2（P-36）: dispatch されない Approval は取得件数を占有しないよう SQL 段階で除外する。
        let mut stmt = conn.prepare(
            "SELECT json FROM tasks WHERE status = ?1 AND kind != ?2 ORDER BY priority DESC, created_at ASC",
        )?;
        let rows = stmt.query_map(
            params![status_str(Status::Ready), kind_str(TaskKind::Approval)],
            |row| row.get::<_, String>(0),
        )?;

        let mut result = Vec::new();
        for row in rows {
            let task = Self::row_to_task(row?)?;

            // ADR-0044 D6（Phase 55）: 止まっている案件・途中目標のタスクは見送る（対話は除く）。
            if !is_conversation(&task) {
                let halted = task
                    .project_id
                    .is_some_and(|p| halted_projects.contains(&p.to_string()))
                    || task
                        .milestone_id
                        .is_some_and(|m| halted_milestones.contains(&m.to_string()));
                if halted {
                    continue;
                }
            }

            // P-78（ADR-0033 D4 / Phase 28）: 対話タスクの `depends_on` は返事を送った順に返すための
            // 直列化だけが目的で、前の対話タスクの成否には意味が無い。前の対話タスクが終端に達していれば
            // （`done` だけでなく `failed` / `cancelled` でも）次の対話タスクへ進めてよい。
            let is_conv = is_conversation(&task);
            let mut deps_done = true;
            for dep_id in &task.depends_on {
                match Self::get_locked(&conn, *dep_id)? {
                    Some(dep) if dep.status == Status::Done => {}
                    Some(dep) if is_conv && dep.status.is_terminal() => {}
                    _ => {
                        deps_done = false;
                        break;
                    }
                }
            }
            if !deps_done {
                continue;
            }

            if let Some(parent_id) = task.parent_id
                && let Some(parent) = Self::get_locked(&conn, parent_id)?
                && parent.kind == TaskKind::Approval
                && parent.status != Status::Done
            {
                continue;
            }

            result.push(task);
            if result.len() >= limit {
                break;
            }
        }

        Ok(result)
    }

    fn apply_transition_with_events(
        &self,
        task_id: TaskId,
        trigger: Trigger,
        extra_events: Vec<Event>,
    ) -> Result<Outcome, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = Self::apply_transition_tx(&tx, task_id, trigger, extra_events)?;
        tx.commit()?;
        Ok(outcome)
    }

    fn complete_plan(
        &self,
        plan_id: TaskId,
        verdict_events: Vec<Event>,
        children: Vec<Task>,
        accept_children: bool,
    ) -> Result<Outcome, StoreError> {
        // ADR-0007 D3: 子の insert + Created (+ Accept) と親の ReviewPass を単一トランザクションで行う。
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for child in &children {
            if child.parent_id != Some(plan_id) {
                return Err(StoreError::Invalid(format!(
                    "child {} does not belong to plan {plan_id}",
                    child.id
                )));
            }
            Self::insert_tx(&tx, child)?;
            Self::append_event_tx(
                &tx,
                child.id,
                &Event::Created {
                    task: Box::new(child.clone()),
                },
            )?;
            if accept_children {
                Self::apply_transition_tx(&tx, child.id, Trigger::Accept, vec![])?;
            }
        }
        let outcome = Self::apply_transition_tx(&tx, plan_id, Trigger::ReviewPass, verdict_events)?;
        tx.commit()?;
        Ok(outcome)
    }

    fn create_task(&self, task: &Task, extra_events: Vec<Event>) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::insert_tx(&tx, task)?;
        Self::append_event_tx(
            &tx,
            task.id,
            &Event::Created {
                task: Box::new(task.clone()),
            },
        )?;
        for event in &extra_events {
            Self::append_event_tx(&tx, task.id, event)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn delegate_children(
        &self,
        parent_id: TaskId,
        run_id: &str,
        children: Vec<Task>,
    ) -> Result<Vec<TaskId>, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if Self::get_locked(&tx, parent_id)?.is_none() {
            return Err(StoreError::Invalid(format!("task not found: {parent_id}")));
        }
        let mut ids = Vec::with_capacity(children.len());
        for child in &children {
            if child.parent_id != Some(parent_id) {
                return Err(StoreError::Invalid(format!(
                    "child {} does not belong to task {parent_id}",
                    child.id
                )));
            }
            Self::insert_tx(&tx, child)?;
            Self::append_event_tx(
                &tx,
                child.id,
                &Event::Created {
                    task: Box::new(child.clone()),
                },
            )?;
            if child.status == Status::Draft {
                Self::apply_transition_tx(&tx, child.id, Trigger::Accept, vec![])?;
            }
            ids.push(child.id);
        }
        Self::append_event_tx(
            &tx,
            parent_id,
            &Event::Delegated {
                run_id: run_id.to_string(),
                task_ids: ids.clone(),
            },
        )?;
        tx.commit()?;
        Ok(ids)
    }

    fn retry_task(&self, original: TaskId, new_task: &Task) -> Result<Vec<TaskId>, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(orig) = Self::get_locked(&tx, original)? else {
            return Err(StoreError::Invalid(format!("task not found: {original}")));
        };
        if !matches!(orig.status, Status::Failed | Status::Cancelled) {
            return Err(StoreError::InvalidTransition(InvalidTransition {
                status: orig.status,
                kind: orig.kind,
                trigger: "retry",
            }));
        }

        Self::insert_tx(&tx, new_task)?;
        Self::append_event_tx(
            &tx,
            new_task.id,
            &Event::Created {
                task: Box::new(new_task.clone()),
            },
        )?;
        Self::append_event_tx(&tx, new_task.id, &Event::Retried { from: original })?;

        let mut rewired = Vec::new();
        for dep_id in Self::dependents_of_tx(&tx, original)? {
            let Some(dep) = Self::get_locked(&tx, dep_id)? else {
                continue;
            };
            let eligible = match dep.status {
                Status::Draft | Status::Ready | Status::Blocked => true,
                Status::Cancelled => {
                    Self::last_transitioned_reason_tx(&tx, dep_id)?.as_deref()
                        == Some(Trigger::DependencyFailed.name())
                }
                _ => false,
            };
            if !eligible {
                continue;
            }
            let mut updated = dep.clone();
            updated.depends_on = updated
                .depends_on
                .iter()
                .map(|d| if *d == original { new_task.id } else { *d })
                .collect();
            let was_cancelled = updated.status == Status::Cancelled;
            if was_cancelled {
                updated.status = Status::Draft;
            }
            updated.updated_at = OffsetDateTime::now_utc();
            Self::rewrite_task_tx(&tx, &updated)?;
            if was_cancelled {
                Self::append_event_tx(
                    &tx,
                    dep_id,
                    &Event::Transitioned {
                        from: Status::Cancelled,
                        to: Status::Draft,
                        reason: "retried".to_string(),
                    },
                )?;
            }
            rewired.push(dep_id);
        }

        tx.commit()?;
        Ok(rewired)
    }

    fn children(&self, parent_id: TaskId) -> Result<Vec<Task>, StoreError> {
        let conn = self.lock()?;
        // 同じトランザクションで挿入した子（created_at が同じ）は挿入順（rowid）で返す。
        let mut stmt = conn.prepare(
            "SELECT json FROM tasks WHERE parent_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![parent_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(Self::row_to_task(row?)?);
        }
        Ok(out)
    }

    fn renew_lease(
        &self,
        task_id: TaskId,
        worker_run_id: &str,
        ttl: StdDuration,
    ) -> Result<bool, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(mut task) = Self::get_locked(&tx, task_id)? else {
            return Ok(false);
        };
        let ours = task.status == Status::Running
            && task.lease.as_ref().map(|l| l.worker_run_id.as_str()) == Some(worker_run_id);
        if !ours {
            return Ok(false);
        }
        let expires_at = OffsetDateTime::now_utc()
            + time::Duration::new(ttl.as_secs() as i64, ttl.subsec_nanos() as i32);
        task.lease = Some(crate::model::Lease {
            worker_run_id: worker_run_id.to_string(),
            expires_at,
        });
        let affected = tx.execute(
            "UPDATE tasks SET lease_expires_at = ?1, json = ?2 WHERE id = ?3 AND status = ?4 AND lease_worker_run_id = ?5",
            params![
                format_rfc3339(expires_at)?,
                serde_json::to_string(&task)?,
                task_id.to_string(),
                status_str(Status::Running),
                worker_run_id,
            ],
        )?;
        tx.commit()?;
        Ok(affected == 1)
    }

    fn events_since(&self, after_id: u64, limit: usize) -> Result<Vec<EventRow>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, seq, ts, json FROM events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![u64_to_i64(after_id), usize_to_i64(limit)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, task_id, seq, ts, json) = row?;
            let task_id = Self::parse_id(&task_id)?;
            let event: Event = serde_json::from_str(&json)?;
            out.push(EventRow {
                id: id as u64,
                task_id,
                seq: seq as u64,
                ts,
                event,
            });
        }
        Ok(out)
    }

    fn latest_event_id(&self) -> Result<u64, StoreError> {
        let conn = self.lock()?;
        let id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
            row.get(0)
        })?;
        Ok(id as u64)
    }

    fn event_rows_for(
        &self,
        task_id: TaskId,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<Vec<EventRow>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, seq, ts, json FROM events WHERE task_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
        )?;
        let after: i64 = after_seq.map(u64_to_i64).unwrap_or(-1);
        let rows = stmt.query_map(
            params![task_id.to_string(), after, usize_to_i64(limit)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (id, seq, ts, json) = row?;
            let event: Event = serde_json::from_str(&json)?;
            out.push(EventRow {
                id: id as u64,
                task_id,
                seq: seq as u64,
                ts,
                event,
            });
        }
        Ok(out)
    }

    fn list_page(
        &self,
        filter: &ListFilter,
        order: ListOrder,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Page<Task>, StoreError> {
        let conn = self.lock()?;
        let (filter_sql, filter_params) = filter_predicate(filter);

        let total: i64 = {
            let sql = format!("SELECT COUNT(*) FROM tasks WHERE {filter_sql}");
            conn.query_row(&sql, params_from_iter(filter_params.iter()), |row| {
                row.get(0)
            })?
        };

        let mut where_sql = format!("({filter_sql})");
        let mut query_params = filter_params;
        if let Some(c) = cursor {
            let payload = decode_cursor(c)?;
            let (keyset_sql, keyset_params) = keyset_predicate(order, &payload);
            where_sql.push_str(&format!(" AND ({keyset_sql})"));
            query_params.extend(keyset_params);
        }

        let order_sql = order_by_sql(order);
        let fetch_limit = usize_to_i64(limit.saturating_add(1));
        let sql = format!("SELECT json FROM tasks WHERE {where_sql} ORDER BY {order_sql} LIMIT ?");
        query_params.push(SqlValue::Integer(fetch_limit));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(query_params.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(Self::row_to_task(row?)?);
        }

        let has_more = items.len() > limit;
        if has_more {
            items.truncate(limit);
        }
        let next_cursor = if has_more {
            match items.last() {
                Some(last) => Some(encode_cursor(&CursorPayload::from_task(last)?)?),
                None => None,
            }
        } else {
            None
        };

        Ok(Page {
            items,
            next_cursor,
            total: total as u64,
        })
    }

    fn count_by_status(&self) -> Result<Vec<(Status, u64)>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM tasks GROUP BY status")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (s, c) = row?;
            out.push((parse_status(&s)?, c as u64));
        }
        Ok(out)
    }

    // ---- ADR-0033 D1: 組織 ----

    fn org_list(&self) -> Result<Vec<OrgNode>, StoreError> {
        let conn = self.lock()?;
        Self::org_list_tx(&conn)
    }

    fn org_get(&self, id: &str) -> Result<Option<OrgNode>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id, parent_id, name, kind, genre, brief, position, created_at, updated_at, profile_json \
                 FROM org_nodes WHERE id = ?1",
                params![id],
                Self::org_row,
            )
            .optional()?;
        row.transpose()
    }

    fn org_upsert(&self, node: &OrgNode) -> Result<OrgNode, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = Self::org_list_tx(&tx)?;
        crate::org::validate_upsert(&existing, node)?;
        let previous = existing.iter().find(|n| n.id == node.id);
        let mut stored = node.clone();
        if let Some(previous) = previous {
            stored.created_at = previous.created_at;
        }
        tx.execute(
            "INSERT INTO org_nodes (id, parent_id, name, kind, genre, brief, position, created_at, updated_at, \
             profile_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(id) DO UPDATE SET parent_id = excluded.parent_id, name = excluded.name, \
             kind = excluded.kind, genre = excluded.genre, brief = excluded.brief, \
             position = excluded.position, updated_at = excluded.updated_at, \
             profile_json = excluded.profile_json",
            params![
                stored.id,
                stored.parent_id,
                stored.name,
                stored.kind.as_str(),
                stored.genre,
                stored.brief,
                stored.position,
                format_rfc3339(stored.created_at)?,
                format_rfc3339(stored.updated_at)?,
                profile_json(&stored.profile)?,
            ],
        )?;
        tx.commit()?;
        Ok(stored)
    }

    fn org_seed(&self, nodes: &[OrgNode]) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut existing = Self::org_list_tx(&tx)?;
        for node in nodes {
            crate::org::validate_upsert(&existing, node)?;
            tx.execute(
                "INSERT INTO org_nodes (id, parent_id, name, kind, genre, brief, position, created_at, updated_at, \
                 profile_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(id) DO UPDATE SET parent_id = excluded.parent_id, name = excluded.name, \
                 kind = excluded.kind, genre = excluded.genre, brief = excluded.brief, \
                 position = excluded.position, updated_at = excluded.updated_at, \
                 profile_json = excluded.profile_json",
                params![
                    node.id,
                    node.parent_id,
                    node.name,
                    node.kind.as_str(),
                    node.genre,
                    node.brief,
                    node.position,
                    format_rfc3339(node.created_at)?,
                    format_rfc3339(node.updated_at)?,
                    profile_json(&node.profile)?,
                ],
            )?;
            existing.push(node.clone());
        }
        tx.commit()?;
        Ok(())
    }

    fn org_delete(&self, id: &str) -> Result<bool, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM org_nodes WHERE id = ?1)",
            params![id],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        // ADR-0033 D1: 「消すときに仕事を抱えていたら 409」。抱えている＝未終了のタスクの assignee。
        let open_tasks: i64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM tasks WHERE assignee = ?1 AND {}",
                Self::NON_TERMINAL_SQL
            ),
            params![id],
            |row| row.get(0),
        )?;
        if open_tasks > 0 {
            return Err(StoreError::InUse {
                kind: "org node",
                id: id.to_string(),
                detail: format!("{open_tasks} task(s) assigned to it have not finished"),
            });
        }
        let children: i64 = tx.query_row(
            "SELECT COUNT(*) FROM org_nodes WHERE parent_id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        if children > 0 {
            return Err(StoreError::InUse {
                kind: "org node",
                id: id.to_string(),
                detail: format!("{children} child node(s) still report to it"),
            });
        }
        tx.execute("DELETE FROM org_nodes WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(true)
    }

    // ---- ADR-0033 D2: 案件と途中目標 ----

    fn project_create(&self, project: &Project) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO projects (id, title, request, status, secretary_summary, created_at, updated_at, workspace, \
             archived_at, paused_from) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                project.id.to_string(),
                project.title,
                project.request,
                project.status.as_str(),
                project.secretary_summary,
                format_rfc3339(project.created_at)?,
                format_rfc3339(project.updated_at)?,
                Self::project_workspace_column(project.workspace.as_ref())?,
                project.archived_at.map(format_rfc3339).transpose()?,
                project.paused_from.map(|s| s.as_str()),
            ],
        )?;
        // ADR-0043 D1: 案件の作業場所は `is_primary = 1` のリポジトリ 1 件として持つ
        // （`Project.workspace` はその写し）。`POST /projects {workspace}`（従来のフォーム）も
        // これで複数リポジトリの世界に入る。
        if let Some(location) = &project.workspace {
            let repo = ProjectRepo {
                id: RepoId::new(),
                project_id: project.id,
                name: crate::repos::default_repo_name(location),
                kind: detect_repo_kind(location),
                location: location.clone(),
                default_branch: None,
                sync: None,
                run: RepoRun::Auto,
                is_primary: true,
                created_at: project.created_at,
            };
            crate::repos::validate_upsert(&[], &repo)?;
            Self::repo_write_tx(&tx, &repo)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn project_get(&self, id: ProjectId) -> Result<Option<Project>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT p.id, p.title, p.request, p.status, p.secretary_summary, p.created_at, p.updated_at, \
                 COALESCE((SELECT r.location_json FROM project_repos r \
                           WHERE r.project_id = p.id AND r.is_primary = 1 \
                           ORDER BY r.created_at ASC, r.id ASC LIMIT 1), p.workspace), \
                 p.archived_at, p.paused_from \
                 FROM projects p WHERE p.id = ?1",
                params![id.to_string()],
                Self::project_row,
            )
            .optional()?;
        row.transpose()
    }

    fn project_list(&self) -> Result<Vec<Project>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT p.id, p.title, p.request, p.status, p.secretary_summary, p.created_at, p.updated_at, \
                 COALESCE((SELECT r.location_json FROM project_repos r \
                           WHERE r.project_id = p.id AND r.is_primary = 1 \
                           ORDER BY r.created_at ASC, r.id ASC LIMIT 1), p.workspace), \
                 p.archived_at, p.paused_from \
             FROM projects p ORDER BY p.created_at DESC, p.id DESC",
        )?;
        let rows = stmt.query_map([], Self::project_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn project_set_status(&self, id: ProjectId, status: ProjectStatus) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let affected = conn.execute(
            "UPDATE projects SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                status.as_str(),
                format_rfc3339(OffsetDateTime::now_utc())?,
                id.to_string()
            ],
        )?;
        Ok(affected == 1)
    }

    /// ADR-0044 D6（Phase 55）: `pause` / `resume` / `cancel` は状態と `paused_from` を 1 回の UPDATE で書く
    /// （`resume` が「戻り先」を読んだ後に別の書き込みが割り込まないように）。
    fn project_set_lifecycle(
        &self,
        id: ProjectId,
        status: ProjectStatus,
        paused_from: Option<Option<ProjectStatus>>,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let now = format_rfc3339(OffsetDateTime::now_utc())?;
        let affected = match paused_from {
            Some(from) => conn.execute(
                "UPDATE projects SET status = ?1, paused_from = ?2, updated_at = ?3 WHERE id = ?4",
                params![
                    status.as_str(),
                    from.map(|s| s.as_str()),
                    now,
                    id.to_string()
                ],
            )?,
            None => conn.execute(
                "UPDATE projects SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.as_str(), now, id.to_string()],
            )?,
        };
        Ok(affected == 1)
    }

    fn project_set_archived_at(
        &self,
        id: ProjectId,
        at: Option<OffsetDateTime>,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let affected = conn.execute(
            "UPDATE projects SET archived_at = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                at.map(format_rfc3339).transpose()?,
                format_rfc3339(OffsetDateTime::now_utc())?,
                id.to_string()
            ],
        )?;
        Ok(affected == 1)
    }

    fn project_set_workspace(
        &self,
        id: ProjectId,
        workspace: Option<&WorkspaceSpec>,
    ) -> Result<bool, StoreError> {
        let column = Self::project_workspace_column(workspace)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let affected = tx.execute(
            "UPDATE projects SET workspace = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                column,
                format_rfc3339(OffsetDateTime::now_utc())?,
                id.to_string()
            ],
        )?;
        if affected != 1 {
            return Ok(false);
        }
        // ADR-0043 D1: 従来の `PATCH /projects {workspace}` は **primary のリポジトリ**を書き換える。
        let existing = Self::repo_list_tx(&tx, id)?;
        let primary = existing.iter().find(|r| r.is_primary).cloned();
        match (workspace, primary) {
            // 差し替え: primary の場所（と kind）だけを直す。名前・run・default_branch は人の設定を残す。
            (Some(location), Some(mut repo)) => {
                repo.location = location.clone();
                repo.kind = detect_repo_kind(location);
                if repo.kind == RepoKind::Dir {
                    repo.default_branch = None;
                }
                if matches!(location, WorkspaceSpec::Local { .. }) {
                    repo.sync = None;
                }
                let others: Vec<ProjectRepo> = existing
                    .iter()
                    .filter(|r| r.id != repo.id)
                    .cloned()
                    .collect();
                crate::repos::validate_upsert(&others, &repo)?;
                Self::repo_write_tx(&tx, &repo)?;
            }
            // 新規: primary がまだ無い案件に作業場所を付けた。
            (Some(location), None) => {
                let mut name = crate::repos::default_repo_name(location);
                if existing.iter().any(|r| r.name == name) {
                    name = format!("{name}-2");
                }
                let repo = ProjectRepo {
                    id: RepoId::new(),
                    project_id: id,
                    name,
                    kind: detect_repo_kind(location),
                    location: location.clone(),
                    default_branch: None,
                    sync: None,
                    run: RepoRun::Auto,
                    is_primary: true,
                    created_at: OffsetDateTime::now_utc(),
                };
                crate::repos::validate_upsert(&existing, &repo)?;
                Self::repo_write_tx(&tx, &repo)?;
                Self::repo_clear_other_primaries_tx(&tx, id, repo.id)?;
            }
            // 消す: `"workspace": null` は「案件を作業場所なしに戻す」なので primary の行を消す。
            // 未終端のタスクが使っていれば 409（`DELETE /repos/{id}` と同じ規律）。
            (None, Some(repo)) => {
                let open = Self::repo_active_tasks_tx(&tx, repo.id)?;
                if !open.is_empty() {
                    return Err(StoreError::InUse {
                        kind: "project repo",
                        id: repo.id.to_string(),
                        detail: format!("{} task(s) using it have not finished", open.len()),
                    });
                }
                tx.execute(
                    "DELETE FROM project_repos WHERE id = ?1",
                    params![repo.id.to_string()],
                )?;
            }
            (None, None) => {}
        }
        Self::sync_project_workspace_tx(&tx, id)?;
        tx.commit()?;
        Ok(true)
    }

    // ---- ADR-0043 D1（Phase 52）: 案件のリポジトリ ----

    fn repo_create(&self, repo: &ProjectRepo) -> Result<(), StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            params![repo.project_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::Invalid(format!(
                "project not found: {}",
                repo.project_id
            )));
        }
        let existing = Self::repo_list_tx(&tx, repo.project_id)?;
        crate::repos::validate_upsert(&existing, repo)?;
        // 最初の 1 件は自動的に primary（案件に「主なリポジトリ」が無い状態を作らない）。
        let mut repo = repo.clone();
        if existing.is_empty() {
            repo.is_primary = true;
        }
        Self::repo_write_tx(&tx, &repo)?;
        if repo.is_primary {
            Self::repo_clear_other_primaries_tx(&tx, repo.project_id, repo.id)?;
        }
        Self::sync_project_workspace_tx(&tx, repo.project_id)?;
        tx.commit()?;
        Ok(())
    }

    fn repo_get(&self, id: RepoId) -> Result<Option<ProjectRepo>, StoreError> {
        let conn = self.lock()?;
        Self::repo_get_tx(&conn, id)
    }

    fn repo_list(&self, project_id: ProjectId) -> Result<Vec<ProjectRepo>, StoreError> {
        let conn = self.lock()?;
        Self::repo_list_tx(&conn, project_id)
    }

    fn repo_update(&self, repo: &ProjectRepo) -> Result<bool, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(current) = Self::repo_get_tx(&tx, repo.id)? else {
            return Ok(false);
        };
        // `project_id` と `created_at` は動かさない（付け替えは作り直し）。
        let repo = ProjectRepo {
            project_id: current.project_id,
            created_at: current.created_at,
            ..repo.clone()
        };
        let others: Vec<ProjectRepo> = Self::repo_list_tx(&tx, repo.project_id)?
            .into_iter()
            .filter(|r| r.id != repo.id)
            .collect();
        crate::repos::validate_upsert(&others, &repo)?;
        Self::repo_write_tx(&tx, &repo)?;
        if repo.is_primary {
            Self::repo_clear_other_primaries_tx(&tx, repo.project_id, repo.id)?;
        }
        Self::sync_project_workspace_tx(&tx, repo.project_id)?;
        tx.commit()?;
        Ok(true)
    }

    fn repo_delete(&self, id: RepoId) -> Result<bool, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(repo) = Self::repo_get_tx(&tx, id)? else {
            return Ok(false);
        };
        let open = Self::repo_active_tasks_tx(&tx, id)?;
        if !open.is_empty() {
            return Err(StoreError::InUse {
                kind: "project repo",
                id: id.to_string(),
                detail: format!("{} task(s) using it have not finished", open.len()),
            });
        }
        tx.execute(
            "DELETE FROM project_repos WHERE id = ?1",
            params![id.to_string()],
        )?;
        // primary を消したら、残りのうち一番古いものを primary にする（案件に主なリポジトリを残す）。
        if repo.is_primary
            && let Some(next) = Self::repo_list_tx(&tx, repo.project_id)?.first()
        {
            tx.execute(
                "UPDATE project_repos SET is_primary = 1 WHERE id = ?1",
                params![next.id.to_string()],
            )?;
        }
        Self::sync_project_workspace_tx(&tx, repo.project_id)?;
        tx.commit()?;
        Ok(true)
    }

    fn repo_set_primary(&self, id: RepoId) -> Result<bool, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(repo) = Self::repo_get_tx(&tx, id)? else {
            return Ok(false);
        };
        tx.execute(
            "UPDATE project_repos SET is_primary = 1 WHERE id = ?1",
            params![id.to_string()],
        )?;
        Self::repo_clear_other_primaries_tx(&tx, repo.project_id, id)?;
        Self::sync_project_workspace_tx(&tx, repo.project_id)?;
        tx.commit()?;
        Ok(true)
    }

    fn repo_active_tasks(&self, id: RepoId) -> Result<Vec<TaskId>, StoreError> {
        let conn = self.lock()?;
        Self::repo_active_tasks_tx(&conn, id)
    }

    // ---- ADR-0043 D5（Phase 54）: 変更の取り込み ----

    fn integration_put(&self, integration: &TaskIntegration) -> Result<(), StoreError> {
        let conn = self.lock()?;
        Self::integration_put_tx(&conn, integration)
    }

    fn integration_get(&self, id: IntegrationId) -> Result<Option<TaskIntegration>, StoreError> {
        let conn = self.lock()?;
        Ok(
            Self::integration_query_tx(&conn, "id = ?1", params![id.to_string()])?
                .into_iter()
                .next(),
        )
    }

    fn integration_list_for_task(
        &self,
        task_id: TaskId,
    ) -> Result<Vec<TaskIntegration>, StoreError> {
        let conn = self.lock()?;
        Self::integration_query_tx(&conn, "task_id = ?1", params![task_id.to_string()])
    }

    fn integration_latest(
        &self,
        task_id: TaskId,
        repo: &str,
    ) -> Result<Option<TaskIntegration>, StoreError> {
        let conn = self.lock()?;
        Ok(Self::integration_query_tx(
            &conn,
            "task_id = ?1 AND repo_name = ?2",
            params![task_id.to_string(), repo],
        )?
        .into_iter()
        .next())
    }

    fn integration_list_for_project(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<TaskIntegration>, StoreError> {
        let conn = self.lock()?;
        // タスク × リポジトリごとに最新の 1 件（`created_at` が同じなら `id`〈ULID〉で決める）。
        let mut stmt = conn.prepare(&format!(
            "SELECT {cols} FROM task_integrations i \
             JOIN tasks t ON t.id = i.task_id \
             WHERE t.project_id = ?1 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM task_integrations n \
                 WHERE n.task_id = i.task_id AND n.repo_name = i.repo_name \
                   AND (n.created_at > i.created_at OR (n.created_at = i.created_at AND n.id > i.id)) \
               ) \
             ORDER BY i.created_at DESC, i.id DESC LIMIT ?2",
            cols = Self::INTEGRATION_COLUMNS
                .split(", ")
                .map(|c| format!("i.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(
            params![
                project_id.to_string(),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            Self::integration_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn milestone_create(
        &self,
        project_id: ProjectId,
        title: &str,
        description: &str,
        status: MilestoneStatus,
    ) -> Result<Milestone, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            params![project_id.to_string()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::Invalid(format!(
                "project not found: {project_id}"
            )));
        }
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM milestones WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?;
        let now = OffsetDateTime::now_utc();
        let milestone = Milestone {
            id: MilestoneId::new(),
            project_id,
            seq,
            title: title.to_string(),
            description: description.to_string(),
            status,
            paused_from: None,
            created_at: now,
            updated_at: now,
        };
        tx.execute(
            "INSERT INTO milestones (id, project_id, seq, title, description, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                milestone.id.to_string(),
                milestone.project_id.to_string(),
                milestone.seq,
                milestone.title,
                milestone.description,
                milestone.status.as_str(),
                format_rfc3339(milestone.created_at)?,
                format_rfc3339(milestone.updated_at)?,
            ],
        )?;
        tx.commit()?;
        Ok(milestone)
    }

    fn milestone_list(&self, project_id: ProjectId) -> Result<Vec<Milestone>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, seq, title, description, status, created_at, updated_at, paused_from \
             FROM milestones WHERE project_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![project_id.to_string()], Self::milestone_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn milestone_get(&self, id: MilestoneId) -> Result<Option<Milestone>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id, project_id, seq, title, description, status, created_at, updated_at, paused_from \
                 FROM milestones WHERE id = ?1",
                params![id.to_string()],
                Self::milestone_row,
            )
            .optional()?;
        row.transpose()
    }

    fn milestone_set_status(
        &self,
        id: MilestoneId,
        status: MilestoneStatus,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let affected = conn.execute(
            "UPDATE milestones SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![
                status.as_str(),
                format_rfc3339(OffsetDateTime::now_utc())?,
                id.to_string()
            ],
        )?;
        Ok(affected == 1)
    }

    /// ADR-0044 D6（Phase 55）: 状態と `paused_from` を 1 回の UPDATE で書く（`project_set_lifecycle` と同じ）。
    fn milestone_set_lifecycle(
        &self,
        id: MilestoneId,
        status: MilestoneStatus,
        paused_from: Option<Option<MilestoneStatus>>,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let now = format_rfc3339(OffsetDateTime::now_utc())?;
        let affected = match paused_from {
            Some(from) => conn.execute(
                "UPDATE milestones SET status = ?1, paused_from = ?2, updated_at = ?3 WHERE id = ?4",
                params![status.as_str(), from.map(|s| s.as_str()), now, id.to_string()],
            )?,
            None => conn.execute(
                "UPDATE milestones SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.as_str(), now, id.to_string()],
            )?,
        };
        Ok(affected == 1)
    }

    // ---- ADR-0033 D4（Phase 24）: 対話 ----

    fn message_append(&self, message: &Message) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO messages (id, node_id, project_id, role, text, run_id, created_at, task_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id.to_string(),
                message.node_id,
                message.project_id.map(|p| p.to_string()),
                message.role.as_str(),
                message.text,
                message.run_id,
                format_rfc3339(message.created_at)?,
                message.task_id.map(|t| t.to_string()),
            ],
        )?;
        Ok(())
    }

    fn message_list(
        &self,
        node_id: &str,
        project_id: Option<ProjectId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError> {
        let conn = self.lock()?;
        // 新しい順に `limit` 件取ってから古い順に戻す（直近のやり取りを時系列で渡すため）。
        let sql = match project_id {
            Some(_) => {
                "SELECT id, node_id, project_id, role, text, run_id, created_at, task_id FROM messages \
                 WHERE node_id = ?1 AND project_id = ?2 ORDER BY created_at DESC, id DESC LIMIT ?3"
            }
            None => {
                "SELECT id, node_id, project_id, role, text, run_id, created_at, task_id FROM messages \
                 WHERE node_id = ?1 AND project_id IS NULL ORDER BY created_at DESC, id DESC LIMIT ?3"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let project = project_id.map(|p| p.to_string()).unwrap_or_default();
        let rows = stmt.query_map(params![node_id, project, limit as i64], Self::message_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        out.reverse();
        Ok(out)
    }

    /// ADR-0048 D1（Phase 60a）: Console の一本の流れ用（絞り込みは任意、`after` は閉区間）。
    fn message_page(
        &self,
        node_id: Option<&str>,
        project_id: Option<ProjectId>,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError> {
        let conn = self.lock()?;
        let mut where_sql = String::from("1 = 1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(node_id) = node_id {
            where_sql.push_str(" AND node_id = ?");
            args.push(Box::new(node_id.to_string()));
        }
        if let Some(project_id) = project_id {
            where_sql.push_str(" AND project_id = ?");
            args.push(Box::new(project_id.to_string()));
        }
        if let Some(after) = after {
            where_sql.push_str(" AND created_at >= ?");
            args.push(Box::new(after.to_string()));
        }
        // `after` 有り = 古い順にその先から、無し = 新しい順に `limit` 件取って戻す。
        let order = if after.is_some() { "ASC" } else { "DESC" };
        let sql = format!(
            "SELECT id, node_id, project_id, role, text, run_id, created_at, task_id FROM messages \
             WHERE {where_sql} ORDER BY created_at {order}, id {order} LIMIT ?"
        );
        args.push(Box::new(limit as i64));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params_from_iter(args.iter().map(|a| a.as_ref())),
            Self::message_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        if after.is_none() {
            out.reverse();
        }
        Ok(out)
    }

    // ---- ADR-0040 D4（Phase 47）: `daemon_instances` ----

    // ---- ADR-0044 D1/D2（Phase 53）----

    fn update_task(&self, task: &Task, event: Event) -> Result<Task, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(current) = Self::get_locked(&tx, task.id)? else {
            return Err(StoreError::Invalid(format!("task not found: {}", task.id)));
        };
        // 状態機械が持つ 3 つ（`status` / `attempts` / `lease`）だけは**この tx の中で読んだ行**の値を使う。
        // 編集を組み立てている間にディスパッチャが `acquire_lease` を通していたら、渡された `task` は
        // 古い `ready` / `lease: None` を持っている。そのまま書くと `json` と `status` 列が食い違い、
        // そのタスクは二度と dispatch されず run の結果も捨てられる（Phase 53 の監査で発見）。
        let merged = Task {
            status: current.status,
            attempts: current.attempts,
            lease: current.lease.clone(),
            ..task.clone()
        };
        Self::update_task_tx(&tx, &merged)?;
        Self::append_event_tx(&tx, task.id, &event)?;
        tx.commit()?;
        Ok(merged)
    }

    fn comment_add(
        &self,
        comment: &TaskComment,
        transition: Option<(Trigger, Vec<Event>)>,
    ) -> Result<Option<Outcome>, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM tasks WHERE id = ?1",
                params![comment.task_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(StoreError::Invalid(format!(
                "task not found: {}",
                comment.task_id
            )));
        }
        tx.execute(
            "INSERT INTO task_comments (id, task_id, author_kind, author, body, run_id, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                comment.id.to_string(),
                comment.task_id.to_string(),
                comment.author_kind.as_str(),
                comment.author.clone(),
                comment.body,
                comment.run_id.clone(),
                format_rfc3339(comment.created_at)?,
            ],
        )?;
        let outcome = match transition {
            Some((trigger, extra_events)) => Some(Self::apply_transition_tx(
                &tx,
                comment.task_id,
                trigger,
                extra_events,
            )?),
            None => None,
        };
        tx.commit()?;
        Ok(outcome)
    }

    fn comments_for(&self, task_id: TaskId) -> Result<Vec<TaskComment>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, author_kind, author, body, run_id, created_at FROM task_comments \
             WHERE task_id = ?1 ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![task_id.to_string()], Self::comment_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn instance_register(&self, instance: &DaemonInstance) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR REPLACE INTO daemon_instances \
             (instance_id, \"release\", pid, role, started_at, heartbeat_at, handoff_requested_at, drained_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                instance.instance_id,
                instance.release,
                i64::from(instance.pid),
                instance.role.as_str(),
                format_rfc3339(instance.started_at)?,
                format_rfc3339(instance.heartbeat_at)?,
                instance.handoff_requested_at.map(format_rfc3339).transpose()?,
                instance.drained_at.map(format_rfc3339).transpose()?,
            ],
        )?;
        Ok(())
    }

    fn instance_heartbeat(
        &self,
        instance_id: &str,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let affected = conn.execute(
            "UPDATE daemon_instances SET heartbeat_at = ?1 WHERE instance_id = ?2",
            params![format_rfc3339(at)?, instance_id],
        )?;
        Ok(affected == 1)
    }

    fn instance_request_handoff(
        &self,
        instance_id: &str,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let affected = conn.execute(
            "UPDATE daemon_instances SET handoff_requested_at = ?1 \
             WHERE instance_id = ?2 AND handoff_requested_at IS NULL",
            params![format_rfc3339(at)?, instance_id],
        )?;
        Ok(affected == 1)
    }

    fn instance_set_role(
        &self,
        instance_id: &str,
        role: InstanceRole,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let affected = conn.execute(
            "UPDATE daemon_instances SET role = ?1, heartbeat_at = ?2 WHERE instance_id = ?3",
            params![role.as_str(), format_rfc3339(at)?, instance_id],
        )?;
        Ok(affected == 1)
    }

    fn instance_mark_drained(
        &self,
        instance_id: &str,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let ts = format_rfc3339(at)?;
        let affected = conn.execute(
            "UPDATE daemon_instances SET drained_at = ?1, heartbeat_at = ?2 WHERE instance_id = ?3",
            params![ts.clone(), ts, instance_id],
        )?;
        Ok(affected == 1)
    }

    fn instance_list(&self) -> Result<Vec<DaemonInstance>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "{SELECT_INSTANCE} ORDER BY started_at ASC, instance_id ASC"
        ))?;
        let rows = stmt.query_map([], row_to_instance)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn instance_delete(&self, instance_id: &str) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let affected = conn.execute(
            "DELETE FROM daemon_instances WHERE instance_id = ?1",
            params![instance_id],
        )?;
        Ok(affected == 1)
    }

    fn instance_delete_stale(
        &self,
        keep: &str,
        heartbeat_before: OffsetDateTime,
    ) -> Result<Vec<String>, StoreError> {
        let mut conn = self.lock()?;
        let before = format_rfc3339(heartbeat_before)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut removed: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT instance_id FROM daemon_instances \
                 WHERE instance_id <> ?1 AND (drained_at IS NOT NULL OR heartbeat_at < ?2)",
            )?;
            let rows = stmt.query_map(params![keep, before], |row| row.get::<_, String>(0))?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row?);
            }
            ids
        };
        removed.sort();
        for id in &removed {
            tx.execute(
                "DELETE FROM daemon_instances WHERE instance_id = ?1",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtifactRef, Budget, Check, Criterion, Tier, WorkerHint, WorkspaceSpec};
    use crate::org::OrgError;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::sync::Barrier;

    fn sample_task(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            acceptance: vec![Criterion {
                text: "tests pass".to_string(),
                check: Check::Command {
                    cmd: "true".to_string(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![ArtifactRef {
                name: "spec".to_string(),
                path: "spec.md".to_string(),
                sha256: "abc".to_string(),
                kind: "doc".to_string(),
            }],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp/workspace".into(),
                mode: None,
            },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 600,
                max_retries: 2,
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
            assignee: None,
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    #[test]
    fn insert_then_get_roundtrips() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched, task);
    }

    #[test]
    fn list_filters_by_status() {
        let store = SqliteStore::open_in_memory().expect("open");
        let ready = sample_task(Status::Ready);
        let draft = sample_task(Status::Draft);
        store.insert(&ready).expect("insert ready");
        store.insert(&draft).expect("insert draft");

        let ready_list = store.list(Some(Status::Ready)).expect("list ready");
        assert_eq!(ready_list.len(), 1);
        assert_eq!(ready_list[0].id, ready.id);

        let all = store.list(None).expect("list all");
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn append_event_reads_back_in_seq_order() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");

        let e0 = Event::ApprovalRequested;
        let e1 = Event::Transitioned {
            from: Status::Draft,
            to: Status::Ready,
            reason: "ready to go".to_string(),
        };

        let seq0 = store.append_event(task.id, &e0).expect("append e0");
        let seq1 = store.append_event(task.id, &e1).expect("append e1");
        assert_eq!(seq0, 0);
        assert_eq!(seq1, 1);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], (0, e0));
        assert_eq!(events[1], (1, e1));
    }

    #[test]
    fn acquire_lease_is_exclusive_under_concurrency() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");

        let barrier = Arc::new(Barrier::new(2));

        let store_a = Arc::clone(&store);
        let barrier_a = Arc::clone(&barrier);
        let task_id = task.id;
        let handle_a = std::thread::spawn(move || {
            barrier_a.wait();
            store_a.acquire_lease(task_id, "worker-a", StdDuration::from_secs(60))
        });

        let store_b = Arc::clone(&store);
        let barrier_b = Arc::clone(&barrier);
        let handle_b = std::thread::spawn(move || {
            barrier_b.wait();
            store_b.acquire_lease(task_id, "worker-b", StdDuration::from_secs(60))
        });

        let result_a = handle_a.join().expect("join a").expect("acquire a");
        let result_b = handle_b.join().expect("join b").expect("acquire b");

        assert_ne!(result_a, result_b, "exactly one acquisition must succeed");
        assert!(result_a || result_b);
    }

    #[test]
    fn release_lease_then_reacquire_succeeds() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");

        let acquired = store
            .acquire_lease(task.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire");
        assert!(acquired);

        // status is now Running, so a second acquire must fail.
        let second = store
            .acquire_lease(task.id, "worker-b", StdDuration::from_secs(60))
            .expect("acquire second");
        assert!(!second);

        store.release_lease(task.id, "worker-a").expect("release");

        let after_release = store.get(task.id).expect("get").expect("some");
        assert!(after_release.lease.is_none());

        // release_lease intentionally does not change status (still Running,
        // per its contract): that is the dispatcher's job via transition().
        // To verify "release then reacquire succeeds" on the very same task,
        // we simulate that dispatcher step here with a direct status update
        // (test-only; this module owns `conn` so it may reach in directly).
        {
            let conn = store.conn.lock().expect("lock");
            let mut readied = after_release.clone();
            readied.status = Status::Ready;
            let json = serde_json::to_string(&readied).expect("serialize");
            conn.execute(
                "UPDATE tasks SET status = 'ready', json = ?1 WHERE id = ?2",
                params![json, task.id.to_string()],
            )
            .expect("reset to ready");
        }

        let reacquired = store
            .acquire_lease(task.id, "worker-c", StdDuration::from_secs(60))
            .expect("acquire third");
        assert!(reacquired);
    }

    #[test]
    fn ready_tasks_excludes_incomplete_dependencies_and_pending_approval_parent() {
        let store = SqliteStore::open_in_memory().expect("open");

        // Case 1: dependency not done -> excluded.
        let dep = sample_task(Status::Running);
        store.insert(&dep).expect("insert dep");
        let mut blocked_by_dep = sample_task(Status::Ready);
        blocked_by_dep.depends_on = vec![dep.id];
        store.insert(&blocked_by_dep).expect("insert blocked");

        // Case 2: dependency done -> included.
        let done_dep = sample_task(Status::Done);
        store.insert(&done_dep).expect("insert done dep");
        let mut unblocked = sample_task(Status::Ready);
        unblocked.depends_on = vec![done_dep.id];
        store.insert(&unblocked).expect("insert unblocked");

        // Case 3: parent is Approval and not Done -> excluded.
        let mut approval_parent = sample_task(Status::Reviewing);
        approval_parent.kind = TaskKind::Approval;
        store.insert(&approval_parent).expect("insert approval");
        let mut child_of_pending_approval = sample_task(Status::Ready);
        child_of_pending_approval.parent_id = Some(approval_parent.id);
        store
            .insert(&child_of_pending_approval)
            .expect("insert child pending");

        let ready = store.ready_tasks(10).expect("ready_tasks");
        let ready_ids: Vec<TaskId> = ready.iter().map(|t| t.id).collect();

        assert!(!ready_ids.contains(&blocked_by_dep.id));
        assert!(ready_ids.contains(&unblocked.id));
        assert!(!ready_ids.contains(&child_of_pending_approval.id));
    }

    /// ADR-0002 D8: `Dispatch`（ready -> running）は `kind == Approval` では
    /// 無効。`Approval` タスクは `acquire_lease` によって running に入っては
    /// ならない（DESIGN §4.2）。
    #[test]
    fn acquire_lease_rejects_approval_kind_even_when_ready() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut approval = sample_task(Status::Ready);
        approval.kind = TaskKind::Approval;
        store.insert(&approval).expect("insert approval");

        let acquired = store
            .acquire_lease(approval.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire_lease call");
        assert!(!acquired);

        let fetched = store.get(approval.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);
        assert!(fetched.lease.is_none());
    }

    /// ADR-0002 D2: 成功した遷移は `Event::Transitioned` を同一トランザクションで
    /// 追記する。`acquire_lease` が実質的に駆動する ready->running 遷移でも
    /// この不変条件が成り立つことを確認する。
    #[test]
    fn acquire_lease_appends_transitioned_event_atomically() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");

        let acquired = store
            .acquire_lease(task.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire");
        assert!(acquired);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Transitioned { from, to, reason } => {
                assert_eq!(*from, Status::Ready);
                assert_eq!(*to, Status::Running);
                assert_eq!(reason, "dispatch");
            }
            other => panic!("expected Transitioned event, got {other:?}"),
        }

        // 失敗した acquire_lease（既にリース済み）はイベントを追加しない。
        let second = store
            .acquire_lease(task.id, "worker-b", StdDuration::from_secs(60))
            .expect("second acquire call");
        assert!(!second);
        let events_after = store.events_for(task.id).expect("events_for after");
        assert_eq!(events_after.len(), 1);
    }

    /// ADR-0004 D1: `apply_transition` は draft -> ready (Accept) を適用し、
    /// tasks の status と Event::Transitioned を同一トランザクションで反映する。
    #[test]
    fn apply_transition_accept_moves_draft_to_ready_and_appends_event() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");

        let outcome = store
            .apply_transition(task.id, Trigger::Accept, None)
            .expect("apply_transition");
        assert_eq!(outcome.next, Status::Ready);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Transitioned { from, to, reason } => {
                assert_eq!(*from, Status::Draft);
                assert_eq!(*to, Status::Ready);
                assert_eq!(reason, "accept");
            }
            other => panic!("expected Transitioned event, got {other:?}"),
        }
    }

    /// ADR-0004 D1: 無効な遷移は `StoreError::InvalidTransition` を返し、
    /// タスクの状態もイベントログも変更しない。
    #[test]
    fn apply_transition_rejects_invalid_trigger_without_side_effects() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Done);
        store.insert(&task).expect("insert");

        let result = store.apply_transition(task.id, Trigger::Accept, None);
        assert!(matches!(result, Err(StoreError::InvalidTransition(_))));

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Done);
        assert!(store.events_for(task.id).expect("events_for").is_empty());
    }

    /// ADR-0004 D2: `Approve`/`Reject` は `extra_event` として
    /// `Event::ApprovalDecided` を同一トランザクションで追記できる。
    #[test]
    fn apply_transition_appends_extra_event_atomically() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut approval = sample_task(Status::Ready);
        approval.kind = TaskKind::Approval;
        store.insert(&approval).expect("insert");

        let extra = Event::ApprovalDecided {
            by: "human".to_string(),
            approved: true,
            note: None,
        };
        let outcome = store
            .apply_transition(approval.id, Trigger::Approve, Some(extra.clone()))
            .expect("apply_transition");
        assert_eq!(outcome.next, Status::Done);

        let events = store.events_for(approval.id).expect("events_for");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].1, Event::Transitioned { .. }));
        assert_eq!(events[1].1, extra);
    }

    /// ADR-0008 D1: `Approval` を reject すると、まだ終端でない直接の子だけが cancelled になる。
    /// 既に done/failed/cancelled の子や、他タスクの子は触らない。
    #[test]
    fn reject_cascades_cancel_to_non_terminal_direct_children_only() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut approval = sample_task(Status::Ready);
        approval.kind = TaskKind::Approval;
        store.insert(&approval).expect("insert approval");

        let mut pending = sample_task(Status::Draft);
        pending.parent_id = Some(approval.id);
        store.insert(&pending).expect("insert pending child");

        let mut running = sample_task(Status::Running);
        running.parent_id = Some(approval.id);
        running.lease = Some(crate::model::Lease {
            worker_run_id: "run-x".into(),
            expires_at: OffsetDateTime::now_utc() + time::Duration::seconds(60),
        });
        store.insert(&running).expect("insert running child");

        let mut already_done = sample_task(Status::Done);
        already_done.parent_id = Some(approval.id);
        store.insert(&already_done).expect("insert done child");

        let unrelated = sample_task(Status::Draft);
        store.insert(&unrelated).expect("insert unrelated");

        let outcome = store
            .apply_transition(
                approval.id,
                Trigger::Reject,
                Some(Event::ApprovalDecided {
                    by: "human".to_string(),
                    approved: false,
                    note: None,
                }),
            )
            .expect("apply_transition reject");
        assert_eq!(outcome.next, Status::Failed);

        let pending_after = store.get(pending.id).expect("get").expect("some");
        assert_eq!(pending_after.status, Status::Cancelled);
        assert!(
            store
                .events_for(pending.id)
                .expect("events_for")
                .iter()
                .any(|(_, e)| matches!(e, Event::Transitioned { to: Status::Cancelled, reason, .. } if reason == "cancel"))
        );

        let running_after = store.get(running.id).expect("get").expect("some");
        assert_eq!(running_after.status, Status::Cancelled);
        assert!(
            running_after.lease.is_none(),
            "leaving running must release the lease"
        );

        // 既に done の子は触らない。
        assert_eq!(
            store
                .get(already_done.id)
                .expect("get")
                .expect("some")
                .status,
            Status::Done
        );
        // 他タスクの子（parent_id が違う）も触らない。
        assert_eq!(
            store.get(unrelated.id).expect("get").expect("some").status,
            Status::Draft
        );
    }

    /// apply_transition が存在しない task_id に対して呼ばれた場合の扱い。
    #[test]
    fn apply_transition_missing_task_returns_invalid_error() {
        let store = SqliteStore::open_in_memory().expect("open");
        let result = store.apply_transition(TaskId::new(), Trigger::Accept, None);
        assert!(matches!(result, Err(StoreError::Invalid(_))));
    }

    /// ADR-0002 D1: running から出る全遷移でリースを解放する。`apply_transition` が
    /// 駆動する遷移（ここでは WorkerDone）でも `lease` が None になり、
    /// `lease_worker_run_id`/`lease_expires_at` 列も NULL になることを確認する。
    #[test]
    fn apply_transition_releases_lease_when_leaving_running() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");
        store
            .acquire_lease(task.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire_lease")
            .then_some(())
            .expect("lease should be acquired");

        let running = store.get(task.id).expect("get").expect("some");
        assert!(running.lease.is_some());

        store
            .apply_transition(task.id, Trigger::WorkerDone, None)
            .expect("apply_transition");

        let after = store.get(task.id).expect("get").expect("some");
        assert_eq!(after.status, Status::Reviewing);
        assert!(after.lease.is_none());

        let (lease_worker_run_id, lease_expires_at): (Option<String>, Option<String>) = {
            let conn = store.conn.lock().expect("lock");
            conn.query_row(
                "SELECT lease_worker_run_id, lease_expires_at FROM tasks WHERE id = ?1",
                params![task.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query lease columns")
        };
        assert!(lease_worker_run_id.is_none());
        assert!(lease_expires_at.is_none());
    }

    #[test]
    fn apply_transition_with_events_appends_transitioned_then_extras_in_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task(Status::Ready);
        store.insert(&task).unwrap();
        assert!(
            store
                .acquire_lease(task.id, "run-1", StdDuration::from_secs(60))
                .unwrap()
        );
        let extras = vec![
            Event::WorkerFinished {
                run_id: "run-1".into(),
                outcome: "done: x".into(),
                usage: None,
                role: None,
            },
            Event::worker_progress("run-1", "extra"),
        ];
        let outcome = store
            .apply_transition_with_events(task.id, Trigger::WorkerDone, extras)
            .unwrap();
        assert_eq!(outcome.next, Status::Reviewing);
        let events = store.events_for(task.id).unwrap();
        let tail: Vec<(u64, String)> = events[events.len() - 3..]
            .iter()
            .map(|(seq, e)| {
                (
                    *seq,
                    match e {
                        Event::Transitioned { to, reason, .. } => {
                            format!("transitioned:{to:?}:{reason}")
                        }
                        Event::WorkerFinished { outcome, .. } => format!("finished:{outcome}"),
                        Event::WorkerProgress { msg, .. } => format!("progress:{msg}"),
                        _ => "other".into(),
                    },
                )
            })
            .collect();
        assert_eq!(
            tail,
            vec![
                (1, "transitioned:Reviewing:worker_done".to_string()),
                (2, "finished:done: x".to_string()),
                (3, "progress:extra".to_string()),
            ]
        );
        assert!(store.get(task.id).unwrap().unwrap().lease.is_none());
        // 無効な遷移では何も追記されない。
        let before = store.events_for(task.id).unwrap().len();
        assert!(
            store
                .apply_transition_with_events(
                    task.id,
                    Trigger::WorkerDone,
                    vec![Event::ApprovalRequested]
                )
                .is_err()
        );
        assert_eq!(store.events_for(task.id).unwrap().len(), before);
    }

    /// ADR-0007 D3: 子の挿入と親の ReviewPass が同一トランザクションで、accept_children に応じて
    /// 子が draft / ready になる。親の遷移が無効なら子も挿入されない。
    #[test]
    fn complete_plan_inserts_children_and_passes_parent_atomically() {
        use crate::model::TaskKind;
        let store = SqliteStore::open_in_memory().expect("open");
        let mut plan = sample_task(Status::Reviewing);
        plan.kind = TaskKind::Plan;
        store.insert(&plan).expect("insert plan");
        let mut c1 = sample_task(Status::Draft);
        c1.parent_id = Some(plan.id);
        let mut c2 = sample_task(Status::Draft);
        c2.parent_id = Some(plan.id);
        c2.depends_on = vec![c1.id];
        let verdict = Event::ReviewVerdict {
            run_id: "r".into(),
            criterion_idx: 0,
            pass: true,
            reason: "plan ok".into(),
        };
        let outcome = store
            .complete_plan(plan.id, vec![verdict], vec![c1.clone(), c2.clone()], false)
            .expect("complete_plan");
        assert_eq!(outcome.next, Status::Done);
        assert_eq!(store.get(plan.id).unwrap().unwrap().status, Status::Done);
        let ev: Vec<Event> = store
            .events_for(plan.id)
            .unwrap()
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        assert!(
            matches!(&ev[0], Event::Transitioned { to: Status::Done, reason, .. } if reason == "review_pass")
        );
        assert!(matches!(&ev[1], Event::ReviewVerdict { pass: true, .. }));
        for c in [&c1, &c2] {
            let got = store.get(c.id).unwrap().expect("child inserted");
            assert_eq!(got.status, Status::Draft);
            let ev = store.events_for(c.id).unwrap();
            assert_eq!(ev.len(), 1);
            assert!(matches!(&ev[0].1, Event::Created { .. }));
        }
        // draft の子は ready_tasks に出ない。
        assert!(store.ready_tasks(10).unwrap().is_empty());

        // accept_children = true: 子は ready、Created + Transitioned(accept)。
        let mut plan2 = sample_task(Status::Reviewing);
        plan2.kind = TaskKind::Plan;
        store.insert(&plan2).expect("insert plan2");
        let mut c3 = sample_task(Status::Draft);
        c3.parent_id = Some(plan2.id);
        store
            .complete_plan(plan2.id, vec![], vec![c3.clone()], true)
            .expect("complete_plan 2");
        let got = store.get(c3.id).unwrap().unwrap();
        assert_eq!(got.status, Status::Ready);
        let ev = store.events_for(c3.id).unwrap();
        assert_eq!(ev.len(), 2);
        assert!(
            matches!(&ev[1].1, Event::Transitioned { from: Status::Draft, to: Status::Ready, reason } if reason == "accept")
        );
        assert_eq!(store.ready_tasks(10).unwrap().len(), 1);

        // 親が reviewing でなければ全体がロールバックされ、子は挿入されない。
        let mut not_reviewing = sample_task(Status::Ready);
        not_reviewing.kind = TaskKind::Plan;
        store.insert(&not_reviewing).unwrap();
        let mut c4 = sample_task(Status::Draft);
        c4.parent_id = Some(not_reviewing.id);
        let err = store
            .complete_plan(not_reviewing.id, vec![], vec![c4.clone()], true)
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidTransition(_)));
        assert!(store.get(c4.id).unwrap().is_none());
        assert!(store.events_for(c4.id).unwrap().is_empty());

        // 親子関係が違う子は拒否。
        let stranger = sample_task(Status::Draft);
        let mut plan3 = sample_task(Status::Reviewing);
        plan3.kind = TaskKind::Plan;
        store.insert(&plan3).unwrap();
        assert!(matches!(
            store.complete_plan(plan3.id, vec![], vec![stranger], false),
            Err(StoreError::Invalid(_))
        ));
    }

    fn reasons(store: &SqliteStore, id: TaskId) -> Vec<String> {
        store
            .events_for(id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::Transitioned { reason, .. } => Some(reason),
                _ => None,
            })
            .collect()
    }

    /// ADR-0010 D2: create_task は insert + Created + extra を 1 トランザクションで行い、失敗時は何も残さない。
    /// ADR-0016 M2: 委譲された子は Created → Accept で ready になり、親に Delegated が残る。全て 1 トランザクション。
    #[test]
    fn delegate_children_inserts_ready_children_and_records_delegated_on_the_parent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task(Status::Running);
        store.insert(&parent).unwrap();
        let mut a = sample_task(Status::Draft);
        a.parent_id = Some(parent.id);
        let mut b = sample_task(Status::Draft);
        b.parent_id = Some(parent.id);
        b.depends_on = vec![a.id];
        let ids = store
            .delegate_children(parent.id, "run-1", vec![a.clone(), b.clone()])
            .unwrap();
        assert_eq!(ids, vec![a.id, b.id]);
        for id in &ids {
            let t = store.get(*id).unwrap().unwrap();
            assert_eq!(t.status, Status::Ready);
            assert_eq!(reasons(&store, *id), vec!["accept"]);
        }
        let children = store.children(parent.id).unwrap();
        assert_eq!(children.iter().map(|t| t.id).collect::<Vec<_>>(), ids);
        let events = store.events_for(parent.id).unwrap();
        assert!(
            matches!(&events.last().unwrap().1, Event::Delegated { run_id, task_ids } if run_id == "run-1" && *task_ids == ids)
        );
        assert_eq!(
            store.get(parent.id).unwrap().unwrap().status,
            Status::Running
        );
        // 親が違う子は拒否され、何も挿入されない。
        let mut stray = sample_task(Status::Draft);
        stray.parent_id = Some(a.id);
        assert!(
            store
                .delegate_children(parent.id, "run-2", vec![stray.clone()])
                .is_err()
        );
        assert!(store.get(stray.id).unwrap().is_none());
        assert_eq!(store.children(parent.id).unwrap().len(), 2);
        // ready_tasks は a を返し、b は a が done になるまで返さない。
        let ready = store.ready_tasks(10).unwrap();
        assert_eq!(ready.iter().map(|t| t.id).collect::<Vec<_>>(), vec![a.id]);
    }

    #[test]
    fn create_task_inserts_task_and_events_atomically() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task(Status::Ready);
        store
            .create_task(&task, vec![Event::ApprovalRequested])
            .unwrap();
        assert_eq!(store.get(task.id).unwrap().unwrap(), task);
        let ev = store.events_for(task.id).unwrap();
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[0].1, Event::Created { task: t } if t.id == task.id));
        assert_eq!(ev[1].1, Event::ApprovalRequested);
        // 同じ id の再作成は insert で失敗し、イベントも追記されない。
        assert!(
            store
                .create_task(&task, vec![Event::ApprovalRequested])
                .is_err()
        );
        assert_eq!(store.events_for(task.id).unwrap().len(), 2);
    }

    /// ADR-0010 D7（P-7）: renew_lease は running かつ run_id が一致するときだけ期限を更新する。
    #[test]
    fn renew_lease_extends_only_the_matching_running_lease() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task(Status::Ready);
        store.insert(&task).unwrap();
        assert!(
            store
                .acquire_lease(task.id, "run-a", StdDuration::from_secs(5))
                .unwrap()
        );
        let before = store
            .get(task.id)
            .unwrap()
            .unwrap()
            .lease
            .unwrap()
            .expires_at;
        assert!(
            store
                .renew_lease(task.id, "run-a", StdDuration::from_secs(3600))
                .unwrap()
        );
        let after = store.get(task.id).unwrap().unwrap().lease.unwrap();
        assert_eq!(after.worker_run_id, "run-a");
        assert!(after.expires_at > before + time::Duration::seconds(3000));
        let col: String = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT lease_expires_at FROM tasks WHERE id = ?1",
                params![task.id.to_string()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(col, format_rfc3339(after.expires_at).unwrap());
        // 別 run_id や running でないタスクには効かない。
        assert!(
            !store
                .renew_lease(task.id, "run-b", StdDuration::from_secs(1))
                .unwrap()
        );
        store
            .apply_transition(task.id, Trigger::WorkerDone, None)
            .unwrap();
        assert!(
            !store
                .renew_lease(task.id, "run-a", StdDuration::from_secs(1))
                .unwrap()
        );
        assert!(
            !store
                .renew_lease(TaskId::new(), "run-a", StdDuration::from_secs(1))
                .unwrap()
        );
        // 状態遷移ではないのでイベントは増えない（dispatch と worker_done の 2 件だけ）。
        assert_eq!(reasons(&store, task.id), vec!["dispatch", "worker_done"]);
    }

    /// ADR-0010 D2（P-36）: ready_tasks は Approval を返さない。
    #[test]
    fn ready_tasks_excludes_approval_kind() {
        let store = SqliteStore::open_in_memory().unwrap();
        for _ in 0..3 {
            let mut a = sample_task(Status::Ready);
            a.kind = TaskKind::Approval;
            store.insert(&a).unwrap();
        }
        let exec = sample_task(Status::Ready);
        store.insert(&exec).unwrap();
        let ready = store.ready_tasks(1).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, exec.id);
    }

    /// ADR-0010 D1（P-4）: 終端タスクへの Cancel は無効で、状態もイベントも変わらない。
    #[test]
    fn cancel_is_invalid_for_terminal_tasks() {
        let store = SqliteStore::open_in_memory().unwrap();
        for status in [Status::Done, Status::Failed, Status::Cancelled] {
            let t = sample_task(status);
            store.insert(&t).unwrap();
            assert!(matches!(
                store.apply_transition(t.id, Trigger::Cancel, None),
                Err(StoreError::InvalidTransition(_))
            ));
            assert_eq!(store.get(t.id).unwrap().unwrap().status, status);
            assert!(store.events_for(t.id).unwrap().is_empty());
        }
    }

    /// ADR-0010 D2: Approval が cancel された場合も（reject と同じく）終端でない直接の子が cancelled になる。
    #[test]
    fn cancelling_an_approval_cascades_to_its_children() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut approval = sample_task(Status::Ready);
        approval.kind = TaskKind::Approval;
        store.insert(&approval).unwrap();
        let mut child = sample_task(Status::Ready);
        child.parent_id = Some(approval.id);
        store.insert(&child).unwrap();
        store
            .apply_transition(approval.id, Trigger::Cancel, None)
            .unwrap();
        assert_eq!(
            store.get(child.id).unwrap().unwrap().status,
            Status::Cancelled
        );
        assert_eq!(reasons(&store, child.id), vec!["cancel"]);
    }

    /// ADR-0010 D2（P-37）: Approval 以外のタスクが終端になると、未決の Approval 子だけが cancelled になる。
    #[test]
    fn terminal_task_cancels_its_pending_approval_children_only() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task(Status::Reviewing);
        store.insert(&parent).unwrap();
        let mut pending_approval = sample_task(Status::Ready);
        pending_approval.kind = TaskKind::Approval;
        pending_approval.parent_id = Some(parent.id);
        store.insert(&pending_approval).unwrap();
        let mut decided_approval = sample_task(Status::Done);
        decided_approval.kind = TaskKind::Approval;
        decided_approval.parent_id = Some(parent.id);
        store.insert(&decided_approval).unwrap();
        let mut exec_child = sample_task(Status::Draft);
        exec_child.parent_id = Some(parent.id);
        store.insert(&exec_child).unwrap();

        store
            .apply_transition(parent.id, Trigger::ReviewPass, None)
            .unwrap();
        assert_eq!(
            store.get(pending_approval.id).unwrap().unwrap().status,
            Status::Cancelled
        );
        assert_eq!(
            store.get(decided_approval.id).unwrap().unwrap().status,
            Status::Done
        );
        assert_eq!(
            store.get(exec_child.id).unwrap().unwrap().status,
            Status::Draft
        );
    }

    /// ADR-0010 D2（P-9）: 先行タスクが failed になると、終端でない後続が推移的に cancelled（dependency_failed）になる。
    #[test]
    fn dependency_failure_cancels_dependents_transitively() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut a = sample_task(Status::Ready);
        a.budget.max_retries = 0;
        store.insert(&a).unwrap();
        let mut b = sample_task(Status::Draft);
        b.depends_on = vec![a.id];
        store.insert(&b).unwrap();
        let mut c = sample_task(Status::Ready);
        c.depends_on = vec![b.id];
        store.insert(&c).unwrap();
        let mut d = sample_task(Status::Done);
        d.depends_on = vec![a.id];
        store.insert(&d).unwrap();
        let unrelated = sample_task(Status::Ready);
        store.insert(&unrelated).unwrap();

        assert!(
            store
                .acquire_lease(a.id, "run-a", StdDuration::from_secs(60))
                .unwrap()
        );
        let outcome = store
            .apply_transition(a.id, Trigger::WorkerError { retryable: false }, None)
            .unwrap();
        assert_eq!(outcome.next, Status::Failed);

        for id in [b.id, c.id] {
            let t = store.get(id).unwrap().unwrap();
            assert_eq!(t.status, Status::Cancelled);
            assert_eq!(t.attempts, 0);
            assert_eq!(reasons(&store, id), vec!["dependency_failed"]);
        }
        assert_eq!(store.get(d.id).unwrap().unwrap().status, Status::Done);
        assert_eq!(
            store.get(unrelated.id).unwrap().unwrap().status,
            Status::Ready
        );
    }

    /// ADR-0012 D1: `WorkerStarted.provider` は任意。導入前に記録されたイベント（provider 無し）も読める。
    #[test]
    fn worker_started_without_provider_still_deserializes() {
        let old = r#"{"type":"worker_started","run_id":"r","adapter":"fake","model":"m"}"#;
        let ev: Event = serde_json::from_str(old).unwrap();
        assert_eq!(
            ev,
            Event::WorkerStarted {
                run_id: "r".into(),
                adapter: "fake".into(),
                model: "m".into(),
                provider: None,
                account: None,
                role: None,
                task_role: None,
            }
        );
        let new = Event::WorkerStarted {
            run_id: "r".into(),
            adapter: "fake".into(),
            model: "m".into(),
            provider: Some("acct-a".into()),
            account: None,
            role: None,
            task_role: None,
        };
        assert!(
            serde_json::to_string(&new)
                .unwrap()
                .contains(r#""provider":"acct-a""#)
        );
        assert_eq!(serde_json::to_string(&ev).unwrap(), old);
    }

    /// ADR-0024 D4: `WorkerStarted.account` は任意。導入前のイベント（account 無し）も読め、
    /// 新しいイベントは `account` を含めて往復する。
    #[test]
    fn worker_started_account_round_trips_and_old_events_still_deserialize() {
        let old = r#"{"type":"worker_started","run_id":"r","adapter":"claude-code","model":"m","provider":"claude-pool"}"#;
        let ev: Event = serde_json::from_str(old).unwrap();
        assert_eq!(
            ev,
            Event::WorkerStarted {
                run_id: "r".into(),
                adapter: "claude-code".into(),
                model: "m".into(),
                provider: Some("claude-pool".into()),
                account: None,
                role: None,
                task_role: None,
            }
        );
        assert_eq!(serde_json::to_string(&ev).unwrap(), old);

        let with_account = Event::WorkerStarted {
            run_id: "r2".into(),
            adapter: "claude-code".into(),
            model: "m".into(),
            provider: Some("claude-pool".into()),
            account: Some("acct-a".into()),
            role: None,
            task_role: None,
        };
        let json = serde_json::to_string(&with_account).unwrap();
        assert!(json.contains(r#""account":"acct-a""#), "{json}");
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), with_account);
    }

    /// ADR-0014 D1: `role` は任意。無ければワーカー run で、既存のイベントの直列化は変わらない。Reviewer run は `"role":"reviewer"`。
    #[test]
    fn run_events_role_is_optional_and_reviewer_serializes_explicitly() {
        let old = r#"{"type":"worker_finished","run_id":"r","outcome":"done: x","usage":null}"#;
        let ev: Event = serde_json::from_str(old).unwrap();
        assert_eq!(
            ev,
            Event::WorkerFinished {
                run_id: "r".into(),
                outcome: "done: x".into(),
                usage: None,
                role: None
            }
        );
        assert_eq!(serde_json::to_string(&ev).unwrap(), old);
        let reviewer = Event::WorkerStarted {
            run_id: "rv".into(),
            adapter: "fake".into(),
            model: "m".into(),
            provider: Some("acct-a".into()),
            account: None,
            role: Some(crate::RunRole::Reviewer),
            task_role: None,
        };
        let json = serde_json::to_string(&reviewer).unwrap();
        assert!(json.contains(r#""role":"reviewer""#), "{json}");
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), reviewer);
    }

    // ---- ADR-0013 D5/D6/D9/D10: schema migrations, PRAGMA, events の global id, list_page ----

    /// 版数 1 の DB（`schema_migrations` が無く `tasks`/`events` だけがある）を素の `Connection` で
    /// 作る。`0001_init.sql` だけを適用し、`insert_tx`/`append_event_tx` を経由しない生の SQL で
    /// タスクと events を書く。
    fn insert_legacy_task(conn: &Connection, task: &Task) {
        let json = serde_json::to_string(task).unwrap();
        let created_at = format_rfc3339(task.created_at).unwrap();
        let (lease_worker_run_id, lease_expires_at): (Option<String>, Option<String>) =
            match &task.lease {
                Some(l) => (
                    Some(l.worker_run_id.clone()),
                    Some(format_rfc3339(l.expires_at).unwrap()),
                ),
                None => (None, None),
            };
        conn.execute(
            "INSERT INTO tasks (id, status, kind, parent_id, priority, created_at, \
             lease_worker_run_id, lease_expires_at, json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                task.id.to_string(),
                status_str(task.status),
                kind_str(task.kind),
                task.parent_id.map(|p| p.to_string()),
                task.priority,
                created_at,
                lease_worker_run_id,
                lease_expires_at,
                json,
            ],
        )
        .unwrap();
    }

    fn insert_legacy_event(conn: &Connection, task_id: TaskId, seq: u64, event: &Event) {
        let ts = format_rfc3339(OffsetDateTime::now_utc()).unwrap();
        let json = serde_json::to_string(event).unwrap();
        conn.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![task_id.to_string(), seq, ts, json],
        )
        .unwrap();
    }

    /// ADR-0013 D5/D6: 版数 1 の DB を `open` すると版数 3 に上がり、`events` の rowid 順が
    /// `events_since` の id 順に保たれ、`events_for` は移行前と同一の結果を返し、`title`/
    /// `updated_at` 列が埋まる。2 回目の `open` は何も再適用しない（`schema_migrations` の
    /// `applied_at` が変わらない）。
    #[test]
    fn open_migrates_legacy_v1_db_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");

        let a = sample_task(Status::Draft);
        let b = sample_task(Status::Ready);
        let mut expected_a: Vec<(u64, Event)> = Vec::new();
        let mut expected_b: Vec<(u64, Event)> = Vec::new();
        let mut expected_global: Vec<(TaskId, u64)> = Vec::new();

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_0001).unwrap();
            insert_legacy_task(&conn, &a);
            insert_legacy_task(&conn, &b);

            // events は task をまたいで rowid 順をばらして挿入する（各 task 内の seq 順は保つ）。
            let ev = Event::Created {
                task: Box::new(a.clone()),
            };
            insert_legacy_event(&conn, a.id, 0, &ev);
            expected_a.push((0, ev));
            expected_global.push((a.id, 0));

            let ev = Event::Created {
                task: Box::new(b.clone()),
            };
            insert_legacy_event(&conn, b.id, 0, &ev);
            expected_b.push((0, ev));
            expected_global.push((b.id, 0));

            let ev = Event::Transitioned {
                from: Status::Draft,
                to: Status::Ready,
                reason: "accept".into(),
            };
            insert_legacy_event(&conn, a.id, 1, &ev);
            expected_a.push((1, ev));
            expected_global.push((a.id, 1));

            let ev = Event::ApprovalRequested;
            insert_legacy_event(&conn, b.id, 1, &ev);
            expected_b.push((1, ev));
            expected_global.push((b.id, 1));

            let ev = Event::worker_progress("r", "go");
            insert_legacy_event(&conn, a.id, 2, &ev);
            expected_a.push((2, ev));
            expected_global.push((a.id, 2));
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let since = store.events_since(0, 100).unwrap();
        assert_eq!(since.len(), 5);
        let got_order: Vec<(TaskId, u64)> = since.iter().map(|r| (r.task_id, r.seq)).collect();
        assert_eq!(
            got_order, expected_global,
            "events_since id order must match original rowid order"
        );
        assert!(since.windows(2).all(|w| w[0].id < w[1].id));

        assert_eq!(store.events_for(a.id).unwrap(), expected_a);
        assert_eq!(store.events_for(b.id).unwrap(), expected_b);

        let (title_a, updated_at_a, objective_a): (String, String, String) = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT title, updated_at, objective FROM tasks WHERE id = ?1",
                params![a.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
        };
        assert_eq!(title_a, a.title);
        // ADR-0014 D2: マイグレーション 0004 が既存行の objective を json から埋める。
        assert_eq!(objective_a, a.objective);
        assert_eq!(updated_at_a, format_rfc3339(a.updated_at).unwrap());

        let applied_before: Vec<(i64, String)> = {
            let conn = store.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        drop(store);

        let store2 = SqliteStore::open(&path).unwrap();
        assert_eq!(store2.schema_version().unwrap(), SCHEMA_VERSION);
        let applied_after: Vec<(i64, String)> = {
            let conn = store2.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            applied_before, applied_after,
            "second open must not re-apply migrations"
        );
    }

    /// ADR-0027 D1: 版数 4 の DB（`genre` 列が無い）を `open` すると版数 5 に上がり、
    /// 既存行は `genre` 列が `NULL`（= `Task::genre == None`）のまま読める。0003/0004 の
    /// マイグレーションテストと同じ形（生の SQL で旧スキーマを作ってから `open` する）。
    #[test]
    fn open_migrates_schema_4_db_and_old_rows_read_back_with_genre_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema4.sqlite3");
        let task = sample_task(Status::Draft);
        assert!(task.genre.is_none());

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATION_0001).unwrap();
            conn.execute_batch(MIGRATION_0002).unwrap();
            conn.execute_batch(MIGRATION_0003).unwrap();
            conn.execute_batch(MIGRATION_0004).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                 INSERT INTO schema_migrations (version, applied_at) VALUES \
                 (1, '2020-01-01T00:00:00Z'), (2, '2020-01-01T00:00:00Z'), \
                 (3, '2020-01-01T00:00:00Z'), (4, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
            // 版数 4 の列（genre はまだ無い）で直接挿入する。
            let json = serde_json::to_string(&task).unwrap();
            let created_at = format_rfc3339(task.created_at).unwrap();
            conn.execute(
                "INSERT INTO tasks (id, status, kind, parent_id, priority, created_at, \
                 lease_worker_run_id, lease_expires_at, json, title, updated_at, objective) \
                 VALUES (?1,?2,?3,?4,?5,?6,NULL,NULL,?7,?8,?9,?10)",
                params![
                    task.id.to_string(),
                    status_str(task.status),
                    kind_str(task.kind),
                    task.parent_id.map(|p| p.to_string()),
                    task.priority,
                    created_at,
                    json,
                    task.title,
                    created_at,
                    task.objective,
                ],
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let got = store
            .get(task.id)
            .unwrap()
            .expect("task still readable after migration");
        assert_eq!(got.genre, None);
        assert_eq!(got.objective, task.objective);

        let genre_col: Option<String> = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT genre FROM tasks WHERE id = ?1",
                params![task.id.to_string()],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            genre_col, None,
            "migration must not invent a genre for pre-existing rows"
        );
    }

    /// ADR-0013 D5: `schema_migrations` の最大版数がこのバイナリの `SCHEMA_VERSION` より大きい DB は
    /// `StoreError::SchemaTooNew` で開けない。
    #[test]
    fn open_rejects_db_with_schema_version_newer_than_supported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toonew.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                 INSERT INTO schema_migrations (version, applied_at) VALUES (99, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        let result = SqliteStore::open(&path);
        assert!(matches!(
            result,
            Err(StoreError::SchemaTooNew { found: 99, supported }) if supported == SCHEMA_VERSION
        ));
    }

    /// ADR-0013 D5: ファイル DB では `PRAGMA journal_mode` が `wal` になる。
    #[test]
    fn open_sets_wal_journal_mode_for_file_backed_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.sqlite3");
        let store = SqliteStore::open(&path).unwrap();
        let mode: String = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(mode.to_lowercase(), "wal");
    }

    /// Phase 9 監査（受け入れ 2）: 2 つの接続（ディスパッチャと celerisctl / API 相当）が、読んでから書くトランザクションを
    /// 同じファイルに並走させても `database is locked` にならない。DEFERRED だと読み取り後の書き込みへの格上げが
    /// busy_timeout を待たずに SQLITE_BUSY で失敗するため、書き込みトランザクションは IMMEDIATE で始める。
    #[test]
    fn concurrent_read_then_write_transactions_on_two_connections_wait_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writers.sqlite3");
        drop(SqliteStore::open(&path).unwrap());

        let writer = |store: Arc<SqliteStore>| {
            std::thread::spawn(move || -> Result<(), StoreError> {
                for _ in 0..200 {
                    let t = sample_task(Status::Draft);
                    store.create_task(&t, vec![])?;
                    store.apply_transition(t.id, crate::Trigger::Accept, None)?;
                    store.append_event(t.id, &Event::ApprovalRequested)?;
                }
                Ok(())
            })
        };
        let a = Arc::new(SqliteStore::open(&path).unwrap());
        let b = Arc::new(SqliteStore::open(&path).unwrap());
        let (ha, hb) = (writer(Arc::clone(&a)), writer(Arc::clone(&b)));
        ha.join()
            .unwrap()
            .expect("writer a never sees database is locked");
        hb.join()
            .unwrap()
            .expect("writer b never sees database is locked");
        assert_eq!(a.list(Some(Status::Ready)).unwrap().len(), 400);
        assert_eq!(b.events_since(0, 10_000).unwrap().len(), 400 * 3);
    }

    /// ADR-0013 D5: 同じファイルを開いた 2 つの `SqliteStore` が、一方の書き込み中でももう一方から
    /// 読める（WAL + busy_timeout）。
    #[test]
    fn two_connections_read_and_write_the_same_file_concurrently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("concurrent.sqlite3");
        // 先に一度開いてファイルとスキーマを作っておく。
        drop(SqliteStore::open(&path).unwrap());

        let store_a = Arc::new(SqliteStore::open(&path).unwrap());
        let store_b = Arc::new(SqliteStore::open(&path).unwrap());

        let writer = {
            let store_a = Arc::clone(&store_a);
            std::thread::spawn(move || {
                for _ in 0..20 {
                    let t = sample_task(Status::Draft);
                    store_a.insert(&t).unwrap();
                }
            })
        };
        let reader = {
            let store_b = Arc::clone(&store_b);
            std::thread::spawn(move || {
                for _ in 0..20 {
                    store_b.list(None).unwrap();
                }
            })
        };
        writer.join().unwrap();
        reader.join().unwrap();

        assert_eq!(store_a.list(None).unwrap().len(), 20);
    }

    /// ADR-0013 D6: `events_since` は全タスクを跨いで id 昇順、`limit`、`after_id` を尊重し、
    /// `latest_event_id` は現在の最大 id（無ければ 0）を返す。
    #[test]
    fn event_rows_for_returns_one_tasks_rows_with_ids_after_seq() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = sample_task(Status::Draft);
        let b = sample_task(Status::Draft);
        store.insert(&a).unwrap();
        store.insert(&b).unwrap();
        store.append_event(a.id, &Event::ApprovalRequested).unwrap();
        store.append_event(b.id, &Event::ApprovalRequested).unwrap();
        store.append_event(a.id, &Event::ApprovalRequested).unwrap();
        let rows = store.event_rows_for(a.id, None, 10).unwrap();
        assert_eq!(rows.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![0, 1]);
        assert!(rows[0].id < rows[1].id);
        assert!(rows.iter().all(|r| r.task_id == a.id && !r.ts.is_empty()));
        assert_eq!(store.event_rows_for(a.id, Some(0), 10).unwrap().len(), 1);
        assert_eq!(store.event_rows_for(a.id, None, 1).unwrap().len(), 1);
        assert!(
            store
                .event_rows_for(TaskId::new(), None, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn events_since_orders_globally_and_respects_after_id_and_limit() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = sample_task(Status::Draft);
        let b = sample_task(Status::Draft);
        store.insert(&a).unwrap();
        store.insert(&b).unwrap();
        assert_eq!(store.latest_event_id().unwrap(), 0);

        store.append_event(a.id, &Event::ApprovalRequested).unwrap();
        store.append_event(b.id, &Event::ApprovalRequested).unwrap();
        store
            .append_event(a.id, &Event::worker_progress("r", "x"))
            .unwrap();

        let all = store.events_since(0, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(all[0].task_id, a.id);
        assert_eq!(all[1].task_id, b.id);
        assert_eq!(all[2].task_id, a.id);

        assert_eq!(store.latest_event_id().unwrap(), 3);

        let limited = store.events_since(0, 2).unwrap();
        assert_eq!(limited.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2]);

        let after = store.events_since(1, 100).unwrap();
        assert_eq!(after.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 3]);

        let none = store.events_since(3, 100).unwrap();
        assert!(none.is_empty());
    }

    fn task_with(
        title: &str,
        status: Status,
        kind: TaskKind,
        priority: i32,
        parent: Option<TaskId>,
    ) -> Task {
        let mut t = sample_task(status);
        t.title = title.to_string();
        t.kind = kind;
        t.priority = priority;
        t.parent_id = parent;
        t
    }

    /// ADR-0013 D10: `ListFilter` の各条件（複数 status、kind、parent、root_only、text_contains。
    /// `%` を含む検索語のエスケープ込み）。
    #[test]
    fn list_page_filters_by_status_kind_parent_root_only_and_title() {
        let store = SqliteStore::open_in_memory().unwrap();
        let root = task_with("root", Status::Ready, TaskKind::Plan, 0, None);
        store.insert(&root).unwrap();
        let child_exec_ready = task_with(
            "child a",
            Status::Ready,
            TaskKind::Execute,
            0,
            Some(root.id),
        );
        store.insert(&child_exec_ready).unwrap();
        let child_exec_done =
            task_with("child b", Status::Done, TaskKind::Execute, 0, Some(root.id));
        store.insert(&child_exec_done).unwrap();
        let child_approval = task_with(
            "approve 100%",
            Status::Ready,
            TaskKind::Approval,
            0,
            Some(root.id),
        );
        store.insert(&child_approval).unwrap();
        let other_root = task_with("other_root", Status::Draft, TaskKind::Execute, 0, None);
        store.insert(&other_root).unwrap();

        // statuses: 複数指定は OR。
        let f = ListFilter {
            statuses: vec![Status::Ready, Status::Draft],
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            [
                root.id,
                child_exec_ready.id,
                child_approval.id,
                other_root.id
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(page.total, 4);

        // kind
        let f = ListFilter {
            kinds: vec![TaskKind::Approval],
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        assert_eq!(
            page.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![child_approval.id]
        );
        assert_eq!(page.total, 1);

        // parent
        let f = ListFilter {
            parent_id: Some(root.id),
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            [child_exec_ready.id, child_exec_done.id, child_approval.id]
                .into_iter()
                .collect()
        );
        assert_eq!(page.total, 3);

        // root_only
        let f = ListFilter {
            root_only: true,
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(ids, [root.id, other_root.id].into_iter().collect());
        assert_eq!(page.total, 2);

        // text_contains: リテラルな '%' を含む検索語（エスケープが効いているか）。
        let percent_task = task_with("100% done", Status::Ready, TaskKind::Execute, 0, None);
        store.insert(&percent_task).unwrap();
        let no_percent_task = task_with("100 done", Status::Ready, TaskKind::Execute, 0, None);
        store.insert(&no_percent_task).unwrap();
        let f = ListFilter {
            text_contains: Some("100%".to_string()),
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            [child_approval.id, percent_task.id].into_iter().collect()
        );
        assert!(!ids.contains(&no_percent_task.id));

        // ADR-0014 D2: text_contains は objective も対象にする（ASCII の大文字小文字は区別しない）。
        let mut by_objective = task_with("plain title", Status::Ready, TaskKind::Execute, 0, None);
        by_objective.objective = "migrate the billing service".to_string();
        store.insert(&by_objective).unwrap();
        let f = ListFilter {
            text_contains: Some("billing".to_string()),
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        assert_eq!(
            page.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![by_objective.id]
        );
        let f = ListFilter {
            text_contains: Some("BILLING".to_string()),
            ..Default::default()
        };
        assert_eq!(
            store
                .list_page(&f, ListOrder::CreatedDesc, None, 10)
                .unwrap()
                .total,
            1
        );
    }

    /// ADR-0027 D1: `genre` は `kind` と同じ形（完全一致、複数は OR）で絞り込める。
    #[test]
    fn list_page_filters_by_genre() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut coding = task_with("write code", Status::Ready, TaskKind::Execute, 0, None);
        coding.genre = Some("coding".to_string());
        store.insert(&coding).unwrap();
        let mut literature = task_with("survey papers", Status::Ready, TaskKind::Execute, 0, None);
        literature.genre = Some("literature".to_string());
        store.insert(&literature).unwrap();
        let no_genre = task_with("no genre", Status::Ready, TaskKind::Execute, 0, None);
        store.insert(&no_genre).unwrap();

        let f = ListFilter {
            genres: vec!["coding".to_string()],
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        assert_eq!(
            page.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![coding.id]
        );
        assert_eq!(page.total, 1);

        let f = ListFilter {
            genres: vec!["coding".to_string(), "literature".to_string()],
            ..Default::default()
        };
        let page = store
            .list_page(&f, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(ids, [coding.id, literature.id].into_iter().collect());
        assert_eq!(page.total, 2);
    }

    /// ADR-0013 D10: 3 つの並び順。`updated_at` は遷移後に変わるので `UpdatedDesc` の順序も変わる。
    #[test]
    fn list_page_orders_dispatch_updated_desc_created_desc_and_reacts_to_transitions() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut ids = Vec::new();
        for i in 0..5i32 {
            let mut t = sample_task(Status::Ready);
            t.priority = i;
            t.title = format!("t{i}");
            store.insert(&t).unwrap();
            ids.push(t.id);
            std::thread::sleep(StdDuration::from_millis(2));
        }
        let expected_desc: Vec<TaskId> = ids.iter().rev().copied().collect();

        let page = store
            .list_page(&ListFilter::default(), ListOrder::Dispatch, None, 10)
            .unwrap();
        assert_eq!(
            page.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            expected_desc
        );

        let page = store
            .list_page(&ListFilter::default(), ListOrder::CreatedDesc, None, 10)
            .unwrap();
        assert_eq!(
            page.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            expected_desc
        );

        let page = store
            .list_page(&ListFilter::default(), ListOrder::UpdatedDesc, None, 10)
            .unwrap();
        assert_eq!(
            page.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            expected_desc
        );

        // ids[0] は priority 最小・最も古い。cancel して updated_at を更新すると UpdatedDesc の先頭になる。
        store
            .apply_transition(ids[0], Trigger::Cancel, None)
            .unwrap();
        let page = store
            .list_page(&ListFilter::default(), ListOrder::UpdatedDesc, None, 10)
            .unwrap();
        assert_eq!(page.items[0].id, ids[0]);
        // Dispatch 順は status を見ないので変わらない（cancelled でも一覧には出る）。
        let page = store
            .list_page(&ListFilter::default(), ListOrder::Dispatch, None, 10)
            .unwrap();
        assert_eq!(
            page.items.iter().map(|t| t.id).collect::<Vec<_>>(),
            expected_desc
        );
    }

    /// ADR-0013 D10: `limit` より多い件数を cursor で辿ると、重複・欠落なく全件を1回ずつ得られる。
    #[test]
    fn list_page_cursor_chains_without_duplicates_or_gaps() {
        let store = SqliteStore::open_in_memory().unwrap();
        for i in 0..7i32 {
            let mut t = sample_task(Status::Ready);
            t.priority = i % 3;
            store.insert(&t).unwrap();
        }

        let full = store
            .list_page(&ListFilter::default(), ListOrder::Dispatch, None, 100)
            .unwrap();
        assert_eq!(full.total, 7);

        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store
                .list_page(
                    &ListFilter::default(),
                    ListOrder::Dispatch,
                    cursor.as_deref(),
                    3,
                )
                .unwrap();
            assert_eq!(page.total, 7);
            seen.extend(page.items.iter().map(|t| t.id));
            if page.next_cursor.is_none() {
                break;
            }
            cursor = page.next_cursor;
        }
        assert_eq!(seen, full.items.iter().map(|t| t.id).collect::<Vec<_>>());
        let unique: HashSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), 7);
    }

    /// ADR-0013 D10: 不正な cursor は `StoreError::Invalid`。
    #[test]
    fn list_page_rejects_invalid_cursor() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert!(matches!(
            store.list_page(
                &ListFilter::default(),
                ListOrder::Dispatch,
                Some("not-a-cursor"),
                10
            ),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.list_page(&ListFilter::default(), ListOrder::Dispatch, Some("abc"), 10),
            Err(StoreError::Invalid(_))
        ));
        // 偶数長・16進として妥当だが JSON として不正。
        assert!(matches!(
            store.list_page(&ListFilter::default(), ListOrder::Dispatch, Some("00"), 10),
            Err(StoreError::Invalid(_))
        ));
    }

    /// ADR-0013 D10: status ごとの件数集計。0 件の status は含まない。
    #[test]
    fn count_by_status_aggregates_present_statuses_only() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.insert(&sample_task(Status::Ready)).unwrap();
        store.insert(&sample_task(Status::Ready)).unwrap();
        store.insert(&sample_task(Status::Draft)).unwrap();

        let counts = store.count_by_status().unwrap();
        let map: HashMap<Status, u64> = counts.into_iter().collect();
        assert_eq!(map.get(&Status::Ready), Some(&2));
        assert_eq!(map.get(&Status::Draft), Some(&1));
        assert_eq!(map.get(&Status::Done), None);
    }

    /// ADR-0013 D9: `ProviderThrottled.reason` は往復し、`reason` の無い旧 JSON も読める。
    #[test]
    fn provider_throttled_reason_roundtrips_and_reads_legacy_json() {
        let old =
            r#"{"type":"provider_throttled","provider":"acct-a","until":"2024-01-01T00:00:00Z"}"#;
        let ev: Event = serde_json::from_str(old).unwrap();
        assert_eq!(
            ev,
            Event::ProviderThrottled {
                provider: "acct-a".into(),
                until: OffsetDateTime::parse("2024-01-01T00:00:00Z", &Rfc3339).unwrap(),
                reason: None,
            }
        );
        assert_eq!(serde_json::to_string(&ev).unwrap(), old);

        let with_reason = Event::ProviderThrottled {
            provider: "acct-a".into(),
            until: OffsetDateTime::parse("2024-01-01T00:00:00Z", &Rfc3339).unwrap(),
            reason: Some("throttled".into()),
        };
        let json = serde_json::to_string(&with_reason).unwrap();
        assert!(json.contains(r#""reason":"throttled""#));
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(back, with_reason);
    }

    /// ADR-0013 D8: 生成した `EventRow` の JSON Schema とコミット済みファイルの一致。
    /// `UPDATE_SCHEMA=1` で再生成。
    #[test]
    fn event_row_schema_matches_committed() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/api/v1/event.schema.json"
        );
        let generated = serde_json::to_string_pretty(&event_row_schema_value()).unwrap() + "\n";
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(
            committed, generated,
            "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-core`"
        );
    }

    // ---- ADR-0033 D1/D2（Phase 23）: 組織・案件・途中目標 ----

    fn org_node(id: &str, parent: Option<&str>, kind: OrgKind) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            profile: Default::default(),
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            name: format!("{id} の人"),
            kind,
            genre: None,
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn seed_secretary(store: &SqliteStore) {
        store
            .org_upsert(&org_node("secretary", None, OrgKind::Secretary))
            .expect("secretary");
    }

    /// 版数 5 の DB を開くと以後の migration（6, 7）が適用され、もう一度開いても何も起きない（冪等）。
    /// 既存行は壊れない。
    #[test]
    fn open_migrates_schema_5_db_to_the_current_version_and_reapplying_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema5.sqlite3");
        let task = sample_task(Status::Draft);
        {
            let conn = Connection::open(&path).unwrap();
            for sql in [
                MIGRATION_0001,
                MIGRATION_0002,
                MIGRATION_0003,
                MIGRATION_0004,
                MIGRATION_0005,
            ] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                 INSERT INTO schema_migrations (version, applied_at) VALUES \
                 (1, '2020-01-01T00:00:00Z'), (2, '2020-01-01T00:00:00Z'), (3, '2020-01-01T00:00:00Z'), \
                 (4, '2020-01-01T00:00:00Z'), (5, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
            let json = serde_json::to_string(&task).unwrap();
            let created_at = format_rfc3339(task.created_at).unwrap();
            conn.execute(
                "INSERT INTO tasks (id, status, kind, parent_id, priority, created_at, \
                 lease_worker_run_id, lease_expires_at, json, title, updated_at, objective, genre) \
                 VALUES (?1,?2,?3,?4,?5,?6,NULL,NULL,?7,?8,?9,?10,NULL)",
                params![
                    task.id.to_string(),
                    status_str(task.status),
                    kind_str(task.kind),
                    task.parent_id.map(|p| p.to_string()),
                    task.priority,
                    created_at,
                    json,
                    task.title,
                    created_at,
                    task.objective,
                ],
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let got = store.get(task.id).unwrap().expect("old row still readable");
        assert_eq!(got.project_id, None);
        assert_eq!(got.milestone_id, None);
        assert_eq!(got.assignee, None);
        assert!(store.org_list().unwrap().is_empty());
        seed_secretary(&store);
        drop(store);

        // 2 回目に開いても 0006 / 0007 は再適用されず（適用済み）、中身も残る。
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.org_list().unwrap().len(), 1);
        let applied = |version: u32| -> i64 {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
                params![version],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(applied(6), 1, "migration 6 must be recorded exactly once");
        assert_eq!(applied(7), 1, "migration 7 must be recorded exactly once");
    }

    /// Phase 27 / migration 0007: 版数 6 の DB（`reports.project_id` が NOT NULL、案件なしは空文字列。
    /// ADR-0034 D1）を開くと、空文字列の行が NULL になって `None` として読め、`messages.task_id` が増える。
    #[test]
    fn migration_0007_turns_the_empty_project_sentinel_into_null_and_adds_message_task_id() {
        use crate::report::{ReportKind, ReportStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema6.sqlite3");
        let report_id = crate::report::ReportId::new();
        let kept_id = crate::report::ReportId::new();
        let project = sample_project();
        {
            let conn = Connection::open(&path).unwrap();
            for sql in [
                MIGRATION_0001,
                MIGRATION_0002,
                MIGRATION_0003,
                MIGRATION_0004,
                MIGRATION_0005,
                MIGRATION_0006,
            ] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                 INSERT INTO schema_migrations (version, applied_at) VALUES \
                 (1, '2020-01-01T00:00:00Z'), (2, '2020-01-01T00:00:00Z'), (3, '2020-01-01T00:00:00Z'), \
                 (4, '2020-01-01T00:00:00Z'), (5, '2020-01-01T00:00:00Z'), (6, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
            for (id, project_id) in [
                (report_id, String::new()),
                (kept_id, project.id.to_string()),
            ] {
                conn.execute(
                    "INSERT INTO reports (id, project_id, node_id, task_id, kind, level, headline, body, \
                     sources, read_at, created_at) VALUES (?1, ?2, 'infra', NULL, 'bad_news', 1, 'h', 'b', \
                     '[]', NULL, '2026-09-17T00:00:00Z')",
                    params![id.to_string(), project_id],
                )
                .unwrap();
            }
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let migrated = store
            .report_get(report_id)
            .unwrap()
            .expect("old row still readable");
        assert_eq!(
            migrated.project_id, None,
            "空文字列のセンチネルは NULL になる"
        );
        assert_eq!(migrated.kind, ReportKind::BadNews);
        assert_eq!(
            store.report_get(kept_id).unwrap().map(|r| r.project_id),
            Some(Some(project.id)),
            "案件付きの行はそのまま"
        );
        // `messages.task_id` が増えているので、書いて読み戻せる。
        let task_id = TaskId::new();
        let message = Message {
            id: MessageId::new(),
            node_id: "secretary".into(),
            project_id: None,
            role: MessageRole::User,
            text: "こんにちは".into(),
            run_id: None,
            task_id: Some(task_id),
            created_at: OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap(),
        };
        store.message_append(&message).unwrap();
        assert_eq!(
            store.message_list("secretary", None, 10).unwrap()[0].task_id,
            Some(task_id)
        );
    }

    /// Phase 39 / migration 0008（ADR-0037）: 版数 7 の DB を開くと版数 8 になり、`notifications` が
    /// 使えるようになる（既存の行はそのまま）。2 回目に開いても 0008 は再適用されない。
    #[test]
    fn migration_0008_adds_the_notifications_table_to_a_schema_7_db() {
        use crate::notify::{NotificationKind, NotificationStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema7.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            for sql in [
                MIGRATION_0001,
                MIGRATION_0002,
                MIGRATION_0003,
                MIGRATION_0004,
                MIGRATION_0005,
                MIGRATION_0006,
                MIGRATION_0007,
            ] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                 INSERT INTO schema_migrations (version, applied_at) VALUES \
                 (1, '2020-01-01T00:00:00Z'), (2, '2020-01-01T00:00:00Z'), (3, '2020-01-01T00:00:00Z'), \
                 (4, '2020-01-01T00:00:00Z'), (5, '2020-01-01T00:00:00Z'), (6, '2020-01-01T00:00:00Z'), \
                 (7, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 16);
        let now = OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();
        assert!(
            store
                .notification_upsert_pending(
                    NotificationKind::BadNews,
                    "r1",
                    "悪い知らせ",
                    None,
                    now
                )
                .unwrap()
                .is_some()
        );
        assert_eq!(store.notification_pending().unwrap().len(), 1);

        // migration 9（ADR-0037 D6 / GUI 依頼 G13i-P1）: `project_id` が新しい DB でも往復する。
        let project_id = ProjectId::new();
        let with_project = store
            .notification_upsert_pending(
                NotificationKind::MilestoneReady,
                "m1:2",
                "b",
                Some(project_id),
                now,
            )
            .unwrap()
            .unwrap();
        assert_eq!(with_project.project_id, Some(project_id));
        let recent = store.notification_recent(10).unwrap();
        let found = recent.iter().find(|n| n.id == with_project.id).unwrap();
        assert_eq!(found.project_id, Some(project_id));
        drop(store);

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(
            store.notification_pending().unwrap().len(),
            2,
            "既存の行は残る"
        );
        let found = store
            .notification_recent(10)
            .unwrap()
            .into_iter()
            .find(|n| n.id == with_project.id)
            .unwrap();
        assert_eq!(
            found.project_id,
            Some(project_id),
            "project_id も再オープン後に残る"
        );
        let applied: i64 = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 8",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(applied, 1, "migration 8 must be recorded exactly once");
        let applied_9: i64 = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 9",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(applied_9, 1, "migration 9 must be recorded exactly once");
    }

    #[test]
    fn org_nodes_round_trip_and_upsert_keeps_created_at() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_secretary(&store);
        let mut coding = org_node("coding", Some("secretary"), OrgKind::Department);
        coding.position = 2;
        coding.brief = "コードを書く".into();
        let stored = store.org_upsert(&coding).unwrap();
        assert_eq!(store.org_get("coding").unwrap().as_ref(), Some(&stored));

        let mut renamed = stored.clone();
        renamed.name = "コーディング部".into();
        renamed.genre = Some("coding".into());
        renamed.created_at = OffsetDateTime::now_utc() + time::Duration::days(1);
        let updated = store.org_upsert(&renamed).unwrap();
        assert_eq!(
            updated.created_at, stored.created_at,
            "created_at is kept on update"
        );
        assert_eq!(updated.name, "コーディング部");
        assert_eq!(updated.genre.as_deref(), Some("coding"));

        // 並び順は position（同値なら id）の昇順。
        let mut infra = org_node("infra", Some("secretary"), OrgKind::Department);
        infra.position = 1;
        store.org_upsert(&infra).unwrap();
        let ids: Vec<String> = store
            .org_list()
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(
            ids,
            vec!["secretary".to_string(), "infra".into(), "coding".into()]
        );
    }

    /// 監査 D-4: `org_seed` は 1 トランザクション。途中の 1 件が不正（親が居ない）なら、それより前の
    /// 行も含めて何も書かれない（部分的に蒔かれた組織が残らない）。全件が正しければ渡した順に入る。
    #[test]
    fn org_seed_writes_nothing_when_one_node_is_invalid() {
        let store = SqliteStore::open_in_memory().unwrap();
        let nodes = vec![
            org_node("secretary", None, OrgKind::Secretary),
            org_node("coding", Some("secretary"), OrgKind::Department),
            // 親が存在しない: この 1 件が不正。
            org_node("orphan", Some("ghost"), OrgKind::Section),
        ];
        let err = store.org_seed(&nodes).unwrap_err();
        assert!(
            matches!(err, StoreError::Org(OrgError::UnknownParent { .. })),
            "{err}"
        );
        assert!(
            store.org_list().unwrap().is_empty(),
            "nothing is written on failure"
        );

        let ok_nodes = vec![
            org_node("secretary", None, OrgKind::Secretary),
            org_node("coding", Some("secretary"), OrgKind::Department),
            org_node("poc", Some("coding"), OrgKind::Section),
        ];
        store.org_seed(&ok_nodes).unwrap();
        // `org_list` は position（同値なら id）の昇順で返す。全ノードが position = 0 なので id 順になる。
        let ids: Vec<String> = store
            .org_list()
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(
            ids,
            vec!["coding".to_string(), "poc".into(), "secretary".into()]
        );
    }

    #[test]
    fn org_upsert_rejects_a_second_secretary_and_cycles() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_secretary(&store);
        store
            .org_upsert(&org_node("coding", Some("secretary"), OrgKind::Department))
            .unwrap();
        store
            .org_upsert(&org_node("poc", Some("coding"), OrgKind::Section))
            .unwrap();

        let err = store
            .org_upsert(&org_node("boss", None, OrgKind::Secretary))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Org(OrgError::DuplicateSecretary { .. })),
            "{err}"
        );
        let err = store
            .org_upsert(&org_node("coding", Some("coding"), OrgKind::Department))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Org(OrgError::Cycle { .. })),
            "{err}"
        );
        let err = store
            .org_upsert(&org_node("x", Some("ghost"), OrgKind::Section))
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Org(OrgError::UnknownParent { .. })),
            "{err}"
        );
        assert_eq!(
            store.org_list().unwrap().len(),
            3,
            "nothing was written by the failed upserts"
        );
    }

    #[test]
    fn org_delete_refuses_while_a_task_is_open_or_children_remain() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_secretary(&store);
        store
            .org_upsert(&org_node("coding", Some("secretary"), OrgKind::Department))
            .unwrap();
        store
            .org_upsert(&org_node("poc", Some("coding"), OrgKind::Section))
            .unwrap();

        let mut task = sample_task(Status::Ready);
        task.assignee = Some("poc".into());
        store.insert(&task).unwrap();

        let err = store.org_delete("poc").unwrap_err();
        assert!(matches!(err, StoreError::InUse { .. }), "{err}");
        // 子を抱えた部も消せない。
        let err = store.org_delete("coding").unwrap_err();
        assert!(matches!(err, StoreError::InUse { .. }), "{err}");
        // タスクが終端になれば消せる。
        store
            .apply_transition(task.id, Trigger::Cancel, None)
            .unwrap();
        assert!(store.org_delete("poc").unwrap());
        assert!(store.org_get("poc").unwrap().is_none());
        assert!(
            !store.org_delete("poc").unwrap(),
            "deleting a missing node is Ok(false)"
        );
    }

    fn sample_project() -> Project {
        let now = OffsetDateTime::now_utc();
        Project {
            archived_at: None,
            paused_from: None,
            id: ProjectId::new(),
            title: "Pluvio の新テーマ".into(),
            request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証".into(),
            status: ProjectStatus::Proposed,
            secretary_summary: None,
            workspace: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// ADR-0033 D4（Phase 24）: 対話の追記と一覧（案件ごと・ノードごと・件数上限・古い順）。
    #[test]
    fn messages_are_appended_and_listed_oldest_first_per_node_and_project() {
        let store = SqliteStore::open_in_memory().unwrap();
        let project = sample_project();
        store.project_create(&project).unwrap();
        let other_project = sample_project();
        store.project_create(&other_project).unwrap();
        let base = OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();

        let task_id = TaskId::new();
        let append = |node: &str, project_id: Option<ProjectId>, role, text: &str, n: i64| {
            let m = Message {
                id: MessageId::new(),
                node_id: node.into(),
                project_id,
                role,
                text: text.into(),
                run_id: if role == MessageRole::Node {
                    Some(format!("run-{n}"))
                } else {
                    None
                },
                task_id: Some(task_id),
                created_at: base + std::time::Duration::from_secs(n as u64),
            };
            store.message_append(&m).unwrap();
            m
        };

        let q = append(
            "secretary",
            Some(project.id),
            MessageRole::User,
            "この案件をお願い",
            1,
        );
        let a = append(
            "secretary",
            Some(project.id),
            MessageRole::Node,
            "承知しました",
            2,
        );
        append(
            "secretary",
            Some(other_project.id),
            MessageRole::User,
            "別の案件",
            3,
        );
        append(
            "research-survey",
            Some(project.id),
            MessageRole::User,
            "別の人",
            4,
        );
        append(
            "secretary",
            None,
            MessageRole::User,
            "案件に紐づかない雑談",
            5,
        );

        // 案件ごと・ノードごとに分かれ、古い順に並ぶ。
        let thread = store
            .message_list("secretary", Some(project.id), 20)
            .unwrap();
        assert_eq!(
            thread.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![q.id, a.id]
        );
        assert_eq!(thread[0].role, MessageRole::User);
        assert_eq!(thread[1].run_id.as_deref(), Some("run-2"));
        assert_eq!(thread[1].project_id, Some(project.id));
        // R4（migration 0007）: 1 往復の両方の行に、それを起こした対話用タスクの id が入る。
        assert_eq!(thread[0].task_id, Some(task_id));
        assert_eq!(thread[1].task_id, Some(task_id));
        assert_eq!(
            store
                .message_list("secretary", Some(other_project.id), 20)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .message_list("research-survey", Some(project.id), 20)
                .unwrap()
                .len(),
            1
        );
        // `project_id = None` は案件に紐づかない行だけ（案件の行は混ざらない）。
        let chat = store.message_list("secretary", None, 20).unwrap();
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].text, "案件に紐づかない雑談");
        assert!(
            store
                .message_list("ghost", Some(project.id), 20)
                .unwrap()
                .is_empty()
        );

        // 件数上限は「新しい方を残して古い順に返す」。
        for n in 10..20 {
            append(
                "secretary",
                Some(project.id),
                MessageRole::User,
                &format!("m{n}"),
                n,
            );
        }
        let last3 = store
            .message_list("secretary", Some(project.id), 3)
            .unwrap();
        assert_eq!(
            last3.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            vec!["m17", "m18", "m19"]
        );
        assert!(
            store
                .message_list("secretary", Some(project.id), 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn projects_and_milestones_round_trip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let project = sample_project();
        store.project_create(&project).unwrap();
        assert_eq!(
            store.project_get(project.id).unwrap().as_ref(),
            Some(&project)
        );
        assert_eq!(store.project_list().unwrap().len(), 1);

        assert!(
            store
                .project_set_status(project.id, ProjectStatus::Active)
                .unwrap()
        );
        let got = store.project_get(project.id).unwrap().unwrap();
        assert_eq!(got.status, ProjectStatus::Active);
        assert!(
            !store
                .project_set_status(ProjectId::new(), ProjectStatus::Done)
                .unwrap()
        );

        let first = store
            .milestone_create(
                project.id,
                "関連研究を棚卸しする",
                "候補を 3 本",
                MilestoneStatus::Proposed,
            )
            .unwrap();
        let second = store
            .milestone_create(
                project.id,
                "小さな検証を回す",
                "",
                MilestoneStatus::Proposed,
            )
            .unwrap();
        assert_eq!(
            (first.seq, second.seq),
            (1, 2),
            "seq is numbered per project"
        );
        assert_eq!(
            store
                .milestone_list(project.id)
                .unwrap()
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
        assert!(
            store
                .milestone_set_status(second.id, MilestoneStatus::Approved)
                .unwrap()
        );
        assert_eq!(
            store.milestone_list(project.id).unwrap()[1].status,
            MilestoneStatus::Approved
        );
        assert!(
            !store
                .milestone_set_status(MilestoneId::new(), MilestoneStatus::Reached)
                .unwrap()
        );

        let err = store
            .milestone_create(ProjectId::new(), "無い案件", "", MilestoneStatus::Proposed)
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{err}");
    }

    /// ADR-0039 D1（migration 0010）: 案件の作業場所が None / Local / Remote で往復し、後から付け外しできる。
    #[test]
    fn project_workspace_round_trips_and_can_be_set_and_cleared() {
        let store = SqliteStore::open_in_memory().unwrap();
        let none = sample_project();
        store.project_create(&none).unwrap();
        assert_eq!(
            store
                .project_get(none.id)
                .unwrap()
                .and_then(|p| p.workspace),
            None
        );

        let local_spec = WorkspaceSpec::Local {
            path: std::path::PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
            mode: None,
        };
        let mut local = sample_project();
        local.workspace = Some(local_spec.clone());
        store.project_create(&local).unwrap();
        assert_eq!(
            store.project_get(local.id).unwrap().unwrap().workspace,
            Some(local_spec)
        );

        let remote_spec = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: std::path::PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
        };
        let mut remote = sample_project();
        remote.workspace = Some(remote_spec.clone());
        store.project_create(&remote).unwrap();
        assert_eq!(
            store.project_get(remote.id).unwrap().unwrap().workspace,
            Some(remote_spec.clone())
        );
        // 一覧にも載る。
        let listed = store.project_list().unwrap();
        assert_eq!(listed.iter().filter(|p| p.workspace.is_some()).count(), 2);

        // 後から付ける / 消す。
        assert!(
            store
                .project_set_workspace(none.id, Some(&remote_spec))
                .unwrap()
        );
        assert_eq!(
            store.project_get(none.id).unwrap().unwrap().workspace,
            Some(remote_spec)
        );
        assert!(store.project_set_workspace(none.id, None).unwrap());
        assert_eq!(store.project_get(none.id).unwrap().unwrap().workspace, None);
        assert!(!store.project_set_workspace(ProjectId::new(), None).unwrap());
    }

    /// ADR-0039 D1: 版数 9 の DB に migration 0010 が当たり、既存の案件は `workspace = NULL` のまま読める。
    #[test]
    fn migration_0010_adds_the_projects_workspace_column_to_a_schema_9_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema9.sqlite3");
        let legacy = ProjectId::new();
        {
            let conn = Connection::open(&path).unwrap();
            for sql in [
                MIGRATION_0001,
                MIGRATION_0002,
                MIGRATION_0003,
                MIGRATION_0004,
                MIGRATION_0005,
                MIGRATION_0006,
                MIGRATION_0007,
                MIGRATION_0008,
                MIGRATION_0009,
            ] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                 INSERT INTO schema_migrations (version, applied_at) VALUES \
                 (1, '2020-01-01T00:00:00Z'), (2, '2020-01-01T00:00:00Z'), (3, '2020-01-01T00:00:00Z'), \
                 (4, '2020-01-01T00:00:00Z'), (5, '2020-01-01T00:00:00Z'), (6, '2020-01-01T00:00:00Z'), \
                 (7, '2020-01-01T00:00:00Z'), (8, '2020-01-01T00:00:00Z'), (9, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO projects (id, title, request, status, created_at, updated_at) \
                 VALUES (?1, '古い案件', '依頼', 'active', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
                params![legacy.to_string()],
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 16);
        // 導入前の案件は「作業場所なし」= 従来どおり。
        assert_eq!(store.project_get(legacy).unwrap().unwrap().workspace, None);
        let spec = WorkspaceSpec::Local {
            path: std::path::PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
            mode: None,
        };
        assert!(store.project_set_workspace(legacy, Some(&spec)).unwrap());
        assert_eq!(
            store.project_get(legacy).unwrap().unwrap().workspace,
            Some(spec)
        );
    }

    /// ADR-0043 D1（Phase 52）: 版数 11 の DB に migration 0012 が当たり、既存の `projects.workspace` が
    /// `is_primary = 1` のリポジトリ 1 件に写る。`kind` は「パスが git なら git、でなければ dir」で、
    /// これは Rust の backfill（`backfill_project_repos`）が決める。
    #[test]
    fn migration_0012_copies_each_project_workspace_into_a_primary_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("benchfs");
        std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
        let plain_dir = dir.path().join("data set");
        std::fs::create_dir_all(&plain_dir).unwrap();

        let path = dir.path().join("schema11.sqlite3");
        let (git_project, plain_project, none_project, remote_project) = (
            ProjectId::new(),
            ProjectId::new(),
            ProjectId::new(),
            ProjectId::new(),
        );
        {
            let conn = Connection::open(&path).unwrap();
            for sql in [
                MIGRATION_0001,
                MIGRATION_0002,
                MIGRATION_0003,
                MIGRATION_0004,
                MIGRATION_0005,
                MIGRATION_0006,
                MIGRATION_0007,
                MIGRATION_0008,
                MIGRATION_0009,
                MIGRATION_0010,
                MIGRATION_0011,
            ] {
                conn.execute_batch(sql).unwrap();
            }
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\
                 INSERT INTO schema_migrations (version, applied_at) VALUES \
                 (1, '2020-01-01T00:00:00Z'), (2, '2020-01-01T00:00:00Z'), (3, '2020-01-01T00:00:00Z'), \
                 (4, '2020-01-01T00:00:00Z'), (5, '2020-01-01T00:00:00Z'), (6, '2020-01-01T00:00:00Z'), \
                 (7, '2020-01-01T00:00:00Z'), (8, '2020-01-01T00:00:00Z'), (9, '2020-01-01T00:00:00Z'), \
                 (10, '2020-01-01T00:00:00Z'), (11, '2020-01-01T00:00:00Z');",
            )
            .unwrap();
            let rows: [(ProjectId, Option<String>); 4] = [
                (
                    git_project,
                    Some(format!(
                        r#"{{"kind":"local","path":"{}"}}"#,
                        repo_dir.display()
                    )),
                ),
                (
                    plain_project,
                    Some(format!(
                        r#"{{"kind":"local","path":"{}"}}"#,
                        plain_dir.display()
                    )),
                ),
                (none_project, None),
                (
                    remote_project,
                    Some(
                        r#"{"kind":"remote","cluster":"pegasus","path":"/work/NBB/x/benchfs"}"#
                            .to_string(),
                    ),
                ),
            ];
            for (id, workspace) in rows {
                conn.execute(
                    "INSERT INTO projects (id, title, request, status, created_at, updated_at, workspace) \
                     VALUES (?1, '案件', '依頼', 'active', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', ?2)",
                    params![id.to_string(), workspace],
                )
                .unwrap();
            }
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        // git のリポジトリ（`.git` がある）→ `kind = git`、名前はディレクトリ名、primary。
        let repos = store.repo_list(git_project).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "benchfs");
        assert_eq!(repos[0].kind, RepoKind::Git);
        assert!(repos[0].is_primary);
        assert_eq!(repos[0].location, WorkspaceSpec::local(repo_dir.clone()));
        assert_eq!(repos[0].run, RepoRun::Auto);

        // git でないディレクトリ → `kind = dir`。名前は slug（空白は `-`）。
        let plain = store.repo_list(plain_project).unwrap();
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].kind, RepoKind::Dir);
        assert_eq!(plain[0].name, "data-set");

        // 作業場所を決めていない案件にはリポジトリを作らない。
        assert!(store.repo_list(none_project).unwrap().is_empty());

        // リモートは `git`（クラスタ側は見られないので ADR-0018 / 0019 の前提に倒す）。
        let remote = store.repo_list(remote_project).unwrap();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].kind, RepoKind::Git);

        // `Project.workspace` は primary の写し（GUI の後方互換）。
        assert_eq!(
            store.project_get(git_project).unwrap().unwrap().workspace,
            Some(WorkspaceSpec::local(repo_dir))
        );
        assert_eq!(
            store.project_get(none_project).unwrap().unwrap().workspace,
            None
        );
    }

    /// ADR-0043 D1: リポジトリの CRUD と primary の不変条件（1 案件に 1 つ）。
    #[test]
    fn project_repos_are_created_updated_and_have_exactly_one_primary() {
        let store = SqliteStore::open_in_memory().unwrap();
        let project = sample_project();
        store.project_create(&project).unwrap();

        let code = ProjectRepo {
            id: RepoId::new(),
            project_id: project.id,
            name: "benchfs".into(),
            kind: RepoKind::Git,
            location: WorkspaceSpec::local("/srv/benchfs"),
            default_branch: None,
            sync: None,
            run: RepoRun::Auto,
            is_primary: false,
            created_at: OffsetDateTime::now_utc(),
        };
        store.repo_create(&code).unwrap();
        // 最初の 1 件は自動的に primary。
        assert!(store.repo_get(code.id).unwrap().unwrap().is_primary);
        assert_eq!(
            store.project_get(project.id).unwrap().unwrap().workspace,
            Some(WorkspaceSpec::local("/srv/benchfs")),
            "Project.workspace は primary の写し"
        );

        let paper = ProjectRepo {
            id: RepoId::new(),
            name: "benchfs-paper".into(),
            location: WorkspaceSpec::local("/srv/benchfs-paper"),
            ..code.clone()
        };
        store.repo_create(&paper).unwrap();
        let repos = store.repo_list(project.id).unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name, "benchfs", "primary が先頭");
        assert!(!repos[1].is_primary);

        // 名前が重複したら 422（`StoreError::Repo`）。
        let dup = ProjectRepo {
            id: RepoId::new(),
            ..paper.clone()
        };
        assert!(matches!(
            store.repo_create(&dup),
            Err(StoreError::Repo(RepoError::DuplicateName(_)))
        ));

        // primary を移すと写しも移る。
        assert!(store.repo_set_primary(paper.id).unwrap());
        assert!(!store.repo_get(code.id).unwrap().unwrap().is_primary);
        assert_eq!(
            store.project_get(project.id).unwrap().unwrap().workspace,
            Some(WorkspaceSpec::local("/srv/benchfs-paper"))
        );

        // 更新（名前と run）。`project_id` / `created_at` は動かない。
        let renamed = ProjectRepo {
            name: "paper".into(),
            run: RepoRun::Host,
            project_id: ProjectId::new(),
            ..store.repo_get(paper.id).unwrap().unwrap()
        };
        assert!(store.repo_update(&renamed).unwrap());
        let back = store.repo_get(paper.id).unwrap().unwrap();
        assert_eq!(back.name, "paper");
        assert_eq!(back.run, RepoRun::Host);
        assert_eq!(back.project_id, project.id, "案件の付け替えはしない");

        // 消すと、残りのうち一番古いものが primary になる。
        assert!(store.repo_delete(paper.id).unwrap());
        assert!(store.repo_get(code.id).unwrap().unwrap().is_primary);
        assert_eq!(
            store.project_get(project.id).unwrap().unwrap().workspace,
            Some(WorkspaceSpec::local("/srv/benchfs"))
        );
        assert!(!store.repo_delete(paper.id).unwrap(), "無い id は false");
    }

    /// ADR-0043 D1: 未終端のタスクが参照しているリポジトリは消せない（API は 409）。
    #[test]
    fn a_repo_used_by_an_unfinished_task_cannot_be_deleted() {
        let store = SqliteStore::open_in_memory().unwrap();
        let project = sample_project();
        store.project_create(&project).unwrap();
        let repo = ProjectRepo {
            id: RepoId::new(),
            project_id: project.id,
            name: "benchfs".into(),
            kind: RepoKind::Git,
            location: WorkspaceSpec::local("/srv/benchfs"),
            default_branch: None,
            sync: None,
            run: RepoRun::Auto,
            is_primary: true,
            created_at: OffsetDateTime::now_utc(),
        };
        store.repo_create(&repo).unwrap();

        let mut task = sample_task(Status::Ready);
        task.project_id = Some(project.id);
        task.repos = vec![crate::repos::RepoRef::of(&repo)];
        store.insert(&task).unwrap();

        assert_eq!(store.repo_active_tasks(repo.id).unwrap(), vec![task.id]);
        assert!(matches!(
            store.repo_delete(repo.id),
            Err(StoreError::InUse {
                kind: "project repo",
                ..
            })
        ));

        // 終端になれば消せる。
        store
            .apply_transition(task.id, Trigger::Cancel, None)
            .unwrap();
        assert!(store.repo_active_tasks(repo.id).unwrap().is_empty());
        assert!(store.repo_delete(repo.id).unwrap());
        // primary が消えたので案件は「作業場所なし」に戻る。
        assert_eq!(
            store.project_get(project.id).unwrap().unwrap().workspace,
            None
        );
    }

    /// ADR-0043 D5（Phase 54）: 取り込みの記録を書く → 最新を引く → 案件の一覧は
    /// 「タスク × リポジトリごとに最新の 1 件」。
    #[test]
    fn integrations_are_recorded_and_the_latest_one_per_task_and_repo_is_listed() {
        let store = SqliteStore::open_in_memory().unwrap();
        let project = sample_project();
        store.project_create(&project).unwrap();
        let mut task = sample_task(Status::Done);
        task.project_id = Some(project.id);
        store.insert(&task).unwrap();
        // 案件に属さないタスクの記録は案件の一覧に出ない。
        let other = sample_task(Status::Done);
        store.insert(&other).unwrap();

        let t0 = OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();
        let mut first = TaskIntegration::new(
            task.id,
            None,
            "code",
            IntegrationMethod::Pr,
            IntegrationState::Open,
            t0,
        );
        first.pr_number = Some(7);
        first.pr_url = Some("https://example.invalid/pull/7".into());
        store.integration_put(&first).unwrap();
        let paper = TaskIntegration::new(
            task.id,
            None,
            "paper",
            IntegrationMethod::Merge,
            IntegrationState::Done,
            t0 + time::Duration::seconds(1),
        );
        store.integration_put(&paper).unwrap();
        let elsewhere = TaskIntegration::new(
            other.id,
            None,
            "code",
            IntegrationMethod::Discard,
            IntegrationState::Done,
            t0 + time::Duration::seconds(2),
        );
        store.integration_put(&elsewhere).unwrap();

        assert_eq!(
            store.integration_get(first.id).unwrap().as_ref(),
            Some(&first)
        );
        assert_eq!(
            store.integration_latest(task.id, "code").unwrap().as_ref(),
            Some(&first)
        );
        assert_eq!(store.integration_latest(task.id, "nope").unwrap(), None);
        // 新しい順（ADR-0044 B1 の timeline はこれを読む）。
        assert_eq!(
            store
                .integration_list_for_task(task.id)
                .unwrap()
                .iter()
                .map(|i| i.repo.as_str())
                .collect::<Vec<_>>(),
            vec!["paper", "code"]
        );

        // 同じタスク・同じリポジトリをもう一度取り込むと、一覧には新しい方だけ出る。
        let mut second = first.clone();
        second.id = crate::integrations::IntegrationId::new();
        second.state = IntegrationState::Merged;
        second.created_at = t0 + time::Duration::seconds(10);
        second.updated_at = second.created_at;
        store.integration_put(&second).unwrap();
        assert_eq!(
            store.integration_latest(task.id, "code").unwrap().as_ref(),
            Some(&second)
        );

        let listed = store.integration_list_for_project(project.id, 100).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|i| (i.repo.as_str(), i.state))
                .collect::<Vec<_>>(),
            vec![
                ("code", IntegrationState::Merged),
                ("paper", IntegrationState::Done)
            ],
            "タスク × リポジトリごとに最新の 1 件、新しい順"
        );
        assert_eq!(
            store
                .integration_list_for_project(project.id, 1)
                .unwrap()
                .len(),
            1,
            "limit が効く"
        );
    }

    /// ADR-0043 D1: `PATCH /projects {workspace}`（従来のフォーム）は primary のリポジトリを書き換える。
    #[test]
    fn setting_the_project_workspace_keeps_the_primary_repo_in_step() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut project = sample_project();
        project.workspace = None;
        store.project_create(&project).unwrap();
        assert!(store.repo_list(project.id).unwrap().is_empty());

        // 作業場所を付けると primary のリポジトリが 1 件できる。
        let first = WorkspaceSpec::local("/srv/benchfs");
        assert!(
            store
                .project_set_workspace(project.id, Some(&first))
                .unwrap()
        );
        let repos = store.repo_list(project.id).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "benchfs");
        assert!(repos[0].is_primary);

        // 差し替えは場所だけを直す（名前は人の設定を残す）。
        let moved = WorkspaceSpec::local("/srv/moved");
        assert!(
            store
                .project_set_workspace(project.id, Some(&moved))
                .unwrap()
        );
        let repos = store.repo_list(project.id).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "benchfs");
        assert_eq!(repos[0].location, moved);
        assert_eq!(
            store.project_get(project.id).unwrap().unwrap().workspace,
            Some(moved)
        );

        // `null` は primary を消す（= 案件を「作業場所なし」に戻す）。
        assert!(store.project_set_workspace(project.id, None).unwrap());
        assert!(store.repo_list(project.id).unwrap().is_empty());
        assert_eq!(
            store.project_get(project.id).unwrap().unwrap().workspace,
            None
        );
    }

    #[test]
    fn tasks_can_be_listed_by_project() {
        let store = SqliteStore::open_in_memory().unwrap();
        let project = sample_project();
        store.project_create(&project).unwrap();
        let milestone = store
            .milestone_create(project.id, "最初の途中目標", "", MilestoneStatus::Approved)
            .unwrap();

        let mut mine = sample_task(Status::Ready);
        mine.project_id = Some(project.id);
        mine.milestone_id = Some(milestone.id);
        mine.assignee = Some("poc".into());
        store.insert(&mine).unwrap();
        store.insert(&sample_task(Status::Ready)).unwrap();

        let filter = ListFilter {
            project_id: Some(project.id),
            ..ListFilter::default()
        };
        let page = store
            .list_page(&filter, ListOrder::CreatedDesc, None, 10)
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, mine.id);
        let back = store.get(mine.id).unwrap().unwrap();
        assert_eq!(back.project_id, Some(project.id));
        assert_eq!(back.milestone_id, Some(milestone.id));
        assert_eq!(back.assignee.as_deref(), Some("poc"));
    }

    /// ADR-0033 D2: 既存の JSON（3 つの列を持たない）もそのまま読める。
    #[test]
    fn tasks_without_the_new_fields_still_deserialize() {
        let task = sample_task(Status::Draft);
        let mut json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&task).unwrap()).unwrap();
        let obj = json.as_object_mut().unwrap();
        assert!(
            !obj.contains_key("project_id"),
            "None is skipped on serialization"
        );
        obj.remove("genre");
        let back: Task = serde_json::from_value(json).unwrap();
        assert_eq!(back.project_id, None);
        assert_eq!(back.assignee, None);
        // ADR-0044 D3（Phase 53）: 導入前のタスクはラベル無し・種類 other として読める。
        assert!(back.labels.is_empty());
        assert_eq!(back.category, crate::model::TaskCategory::Other);
    }

    // ---- ADR-0044 D6（Phase 55）: 中止・一時停止・アーカイブ ----

    /// migration 0015 が schema 14 の DB に `projects.archived_at` / `projects.paused_from` /
    /// `milestones.paused_from` を足す。既存の行はどれも NULL（＝止まっていない・アーカイブされていない）。
    #[test]
    fn migration_0015_adds_the_lifecycle_columns_to_a_schema_14_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy.sqlite3");
        let project_id = ProjectId::new();
        let milestone_id = MilestoneId::new();
        let now = format_rfc3339(OffsetDateTime::now_utc()).unwrap();
        {
            let mut conn = Connection::open(&path).unwrap();
            SqliteStore::configure_pragmas(&conn, &StoreOptions::default()).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
            )
            .unwrap();
            for version in 1..=14 {
                SqliteStore::apply_migration_version(&mut conn, version).unwrap();
            }
            // 14 版の列だけで案件と途中目標を 1 件ずつ書く（`archived_at` / `paused_from` はまだ無い）。
            conn.execute(
                "INSERT INTO projects (id, title, request, status, created_at, updated_at) \
                 VALUES (?1, '昔の案件', 'やって', 'active', ?2, ?2)",
                params![project_id.to_string(), now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO milestones (id, project_id, seq, title, description, status, created_at, updated_at) \
                 VALUES (?1, ?2, 1, '昔の途中目標', '', 'in_progress', ?3, ?3)",
                params![milestone_id.to_string(), project_id.to_string(), now],
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 16);

        let project = store.project_get(project_id).unwrap().expect("project");
        assert_eq!(project.status, ProjectStatus::Active);
        assert_eq!(project.archived_at, None);
        assert_eq!(project.paused_from, None);
        let milestone = store
            .milestone_get(milestone_id)
            .unwrap()
            .expect("milestone");
        assert_eq!(milestone.status, MilestoneStatus::InProgress);
        assert_eq!(milestone.paused_from, None);

        // 新しい値も往復する。
        store
            .project_set_lifecycle(
                project_id,
                ProjectStatus::Paused,
                Some(Some(ProjectStatus::Active)),
            )
            .unwrap();
        store.project_set_archived_at(project_id, None).unwrap();
        let project = store.project_get(project_id).unwrap().expect("project");
        assert_eq!(project.status, ProjectStatus::Paused);
        assert_eq!(project.paused_from, Some(ProjectStatus::Active));

        store
            .milestone_set_lifecycle(milestone_id, MilestoneStatus::Cancelled, Some(None))
            .unwrap();
        let milestone = store
            .milestone_get(milestone_id)
            .unwrap()
            .expect("milestone");
        assert_eq!(milestone.status, MilestoneStatus::Cancelled);
        assert_eq!(milestone.paused_from, None);
    }

    // ---- ADR-0044 D2/D3/D4（Phase 53）: コメント・ラベル・種類・ボードのフィルタ ----

    /// migration 0013 が schema 11 の DB に `task_comments` と `tasks.labels_json` / `tasks.category` を足す。
    /// 既存の行は「ラベル無し・種類 other」になる。
    #[test]
    fn migration_0013_adds_task_comments_and_the_label_columns_to_a_schema_11_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy.sqlite3");
        let legacy = sample_task(Status::Ready);
        {
            let mut conn = Connection::open(&path).unwrap();
            SqliteStore::configure_pragmas(&conn, &StoreOptions::default()).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
            )
            .unwrap();
            for version in 1..=11 {
                SqliteStore::apply_migration_version(&mut conn, version).unwrap();
            }
            // 11 版の列だけで 1 行書く（`labels_json` / `category` はまだ無い）。
            conn.execute(
                "INSERT INTO tasks (id, status, kind, parent_id, priority, created_at, json, title, updated_at, \
                 objective) VALUES (?1, 'ready', 'execute', NULL, 0, ?2, ?3, ?4, ?2, ?5)",
                params![
                    legacy.id.to_string(),
                    format_rfc3339(legacy.created_at).unwrap(),
                    serde_json::to_string(&legacy).unwrap(),
                    legacy.title,
                    legacy.objective,
                ],
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 16);
        {
            let conn = store.lock().unwrap();
            let (labels, category): (String, String) = conn
                .query_row(
                    "SELECT labels_json, category FROM tasks WHERE id = ?1",
                    params![legacy.id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(labels, "[]");
            assert_eq!(category, "other");
        }
        // コメントを 1 件書ける（表がある）。
        let comment = TaskComment::new(
            legacy.id,
            CommentAuthorKind::Human,
            None,
            "移行後でも書ける".into(),
            None,
            OffsetDateTime::now_utc(),
        );
        assert!(store.comment_add(&comment, None).unwrap().is_none());
        assert_eq!(store.comments_for(legacy.id).unwrap().len(), 1);
    }

    /// コメントは古い順に読め、遷移と同じトランザクションで書ける（割り込み）。
    /// 知らないタスクへのコメントは書けない。
    #[test]
    fn comments_round_trip_and_can_carry_a_transition_in_one_transaction() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task(Status::Running);
        store.insert(&task).unwrap();

        let base = OffsetDateTime::now_utc();
        for (i, body) in ["ひとつめ", "ふたつめ"].iter().enumerate() {
            let c = TaskComment::new(
                task.id,
                CommentAuthorKind::Node,
                Some("impl".into()),
                (*body).to_string(),
                Some(format!("run-{i}")),
                base + time::Duration::seconds(i as i64),
            );
            store.comment_add(&c, None).unwrap();
        }
        let comments = store.comments_for(task.id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].body, "ひとつめ");
        assert_eq!(comments[1].run_id.as_deref(), Some("run-1"));
        assert_eq!(comments[0].author.as_deref(), Some("impl"));

        // 割り込み: コメント + `Interrupt` + `WorkerFinished` が 1 トランザクション。
        let human = TaskComment::new(
            task.id,
            CommentAuthorKind::Human,
            None,
            "止めて".into(),
            None,
            base + time::Duration::seconds(5),
        );
        let finished = Event::WorkerFinished {
            run_id: "run-1".into(),
            outcome: "interrupted: comment".into(),
            usage: None,
            role: None,
        };
        let outcome = store
            .comment_add(&human, Some((Trigger::Interrupt, vec![finished])))
            .unwrap()
            .expect("outcome");
        assert_eq!(outcome.next, Status::Ready);
        assert_eq!(outcome.reason, "comment");
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Ready);
        assert_eq!(store.comments_for(task.id).unwrap().len(), 3);

        // 知らないタスクには書けない（表に行が残らない）。
        let orphan = TaskComment::new(
            TaskId::new(),
            CommentAuthorKind::Human,
            None,
            "x".into(),
            None,
            base,
        );
        assert!(matches!(
            store.comment_add(&orphan, None),
            Err(StoreError::Invalid(_))
        ));
    }

    /// ADR-0044 D1: `update_task` は `json` と絞り込みの列を書き直し、`Event::Edited` を積む。
    /// 状態機械は通らない（`status` は変わらない）。
    #[test]
    fn update_task_rewrites_the_denormalized_columns_and_appends_edited() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task(Status::Running);
        store.insert(&task).unwrap();

        task.title = "新しい題名".into();
        task.objective = "新しい目的".into();
        task.priority = 30;
        task.labels = vec!["infra".into()];
        task.category = crate::model::TaskCategory::Ops;
        task.assignee = Some("infra-section".into());
        store
            .update_task(
                &task,
                Event::Edited {
                    fields: vec!["title".into(), "labels".into()],
                    by: "human".into(),
                },
            )
            .unwrap();

        let after = store.get(task.id).unwrap().unwrap();
        assert_eq!(after.title, "新しい題名");
        assert_eq!(after.status, Status::Running, "状態機械は通らない");
        {
            let conn = store.lock().unwrap();
            let (title, objective, priority, labels, category, assignee): (
                String,
                String,
                i64,
                String,
                String,
                Option<String>,
            ) = conn
                .query_row(
                    "SELECT title, objective, priority, labels_json, category, assignee FROM tasks WHERE id = ?1",
                    params![task.id.to_string()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(title, "新しい題名");
            assert_eq!(objective, "新しい目的");
            assert_eq!(priority, 30);
            assert_eq!(labels, r#"["infra"]"#);
            assert_eq!(category, "ops");
            assert_eq!(assignee.as_deref(), Some("infra-section"));
        }
        assert!(
            store
                .events_for(task.id)
                .unwrap()
                .iter()
                .any(|(_, e)| matches!(e, Event::Edited { by, .. } if by == "human"))
        );

        // 無いタスクは書けない。
        let mut ghost = sample_task(Status::Ready);
        ghost.title = "いない".into();
        assert!(matches!(
            store.update_task(
                &ghost,
                Event::Edited {
                    fields: vec![],
                    by: "human".into()
                }
            ),
            Err(StoreError::Invalid(_))
        ));
    }

    /// ADR-0044 D4: label（AND）/ category / milestone / tier / priority / `q`（コメント本文も）で絞れる。
    #[test]
    fn list_page_filters_by_label_category_tier_priority_and_comment_text() {
        let store = SqliteStore::open_in_memory().unwrap();

        let mut infra = task_with("infra work", Status::Ready, TaskKind::Execute, 30, None);
        infra.labels = vec!["infra".into(), "urgent".into()];
        infra.category = crate::model::TaskCategory::Ops;
        infra.worker_hint.tier = crate::model::Tier::Frontier;
        store.insert(&infra).unwrap();

        let mut docs = task_with("write docs", Status::Ready, TaskKind::Execute, 0, None);
        docs.labels = vec!["infra".into()];
        docs.category = crate::model::TaskCategory::Docs;
        docs.worker_hint.tier = crate::model::Tier::Cheap;
        store.insert(&docs).unwrap();

        let page = |filter: ListFilter| {
            store
                .list_page(&filter, ListOrder::CreatedDesc, None, 50)
                .unwrap()
                .items
                .iter()
                .map(|t| t.title.clone())
                .collect::<Vec<_>>()
        };

        // ラベルは AND。
        assert_eq!(
            page(ListFilter {
                labels: vec!["infra".into()],
                ..ListFilter::default()
            })
            .len(),
            2
        );
        assert_eq!(
            page(ListFilter {
                labels: vec!["infra".into(), "urgent".into()],
                ..ListFilter::default()
            }),
            vec!["infra work".to_string()]
        );
        // 種類・tier・優先度。
        assert_eq!(
            page(ListFilter {
                categories: vec![crate::model::TaskCategory::Docs],
                ..ListFilter::default()
            }),
            vec!["write docs".to_string()]
        );
        assert_eq!(
            page(ListFilter {
                tiers: vec![crate::model::Tier::Frontier],
                ..ListFilter::default()
            }),
            vec!["infra work".to_string()]
        );
        assert_eq!(
            page(ListFilter {
                priorities: vec![30],
                ..ListFilter::default()
            }),
            vec!["infra work".to_string()]
        );
        // 複数のフィルタは AND（種類が合わないので 0 件）。
        assert!(
            page(ListFilter {
                labels: vec!["infra".into()],
                categories: vec![crate::model::TaskCategory::Feature],
                ..ListFilter::default()
            })
            .is_empty()
        );

        // `q` はコメント本文も見る（`text_includes_comments = true` のときだけ）。
        let comment = TaskComment::new(
            docs.id,
            CommentAuthorKind::Human,
            None,
            "ここに zebra と書いてある".into(),
            None,
            OffsetDateTime::now_utc(),
        );
        store.comment_add(&comment, None).unwrap();
        assert_eq!(
            page(ListFilter {
                text_contains: Some("zebra".into()),
                text_includes_comments: true,
                ..ListFilter::default()
            }),
            vec!["write docs".to_string()]
        );
        assert!(
            page(ListFilter {
                text_contains: Some("zebra".into()),
                ..ListFilter::default()
            })
            .is_empty(),
            "コメントを含めない従来の検索では当たらない"
        );
    }

    /// ADR-0044 D4 の検討の記録: rusqlite の bundled には **FTS5 がある**（この検査で実測している）。
    /// それでも Phase 53 が `LIKE` を選んだのは、FTS5 の既定のトークナイザ（unicode61）が**日本語を
    /// 語に切らない**ため、「調査」のような部分一致がこの検査のとおり 0 件になるから。
    /// ADR-0044 D4 は「FTS5。無ければ `LIKE`」と書いているが、日本語の検索としては `LIKE` が正しい。
    #[test]
    fn fts5_availability_of_the_bundled_sqlite_is_recorded() {
        let store = SqliteStore::open_in_memory().unwrap();
        let conn = store.lock().unwrap();
        let available = conn
            .execute_batch("CREATE VIRTUAL TABLE temp.fts5_probe USING fts5(body)")
            .is_ok();
        assert!(
            available,
            "rusqlite の bundled には FTS5 がある（ADR-0044 D4 の前提）"
        );
        // FTS5 はある。だが `unicode61` は日本語を 1 つの token にしてしまうので、
        // 「途中の語」では引けない（`LIKE` を選んだ理由）。
        conn.execute_batch("INSERT INTO temp.fts5_probe(body) VALUES ('関連研究の調査をする')")
            .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM temp.fts5_probe WHERE temp.fts5_probe MATCH '調査'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        assert_eq!(
            hits, 0,
            "FTS5 の既定のトークナイザでは日本語の部分一致にならない"
        );
    }
}
