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

use crate::model::{Event, Status, Task, TaskId, TaskKind};
use crate::org::{
    Milestone, MilestoneId, MilestoneStatus, OrgError, OrgKind, OrgNode, Project, ProjectId, ProjectStatus,
};
use crate::transition::{InvalidTransition, Outcome, StateView, Trigger, transition};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_events_global_id.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_tasks_list_columns.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_tasks_objective_column.sql");
const MIGRATION_0005: &str = include_str!("../migrations/0005_tasks_genre_column.sql");
const MIGRATION_0006: &str = include_str!("../migrations/0006_organization.sql");

/// このバイナリが知っている最新のスキーマ版数（ADR-0013 D5）。DB の版数がこれより大きければ
/// `SqliteStore::open`/`open_with` は `StoreError::SchemaTooNew` で失敗する。
pub const SCHEMA_VERSION: u32 = 6;

/// `SqliteStore::open_with` に渡す接続オプション（ADR-0013 D5）。
#[derive(Debug, Clone, Copy)]
pub struct StoreOptions {
    /// `PRAGMA busy_timeout`。複数接続（ディスパッチャ・API・taskctl）が同じファイルを
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
    if cursor.is_empty() || !cursor.len().is_multiple_of(2) || !cursor.chars().all(|c| c.is_ascii_hexdigit()) {
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
    if let Some(needle) = &filter.text_contains {
        clauses.push("(title LIKE ? ESCAPE '\\' OR objective LIKE ? ESCAPE '\\')".to_string());
        let pattern = format!("%{}%", escape_like(needle));
        params.push(SqlValue::Text(pattern.clone()));
        params.push(SqlValue::Text(pattern));
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
        other => Err(StoreError::Invalid(format!("invalid status in tasks table: {other}"))),
    }
}

/// ADR-0033 D3: 報告（`reports`）の読み書きは `crate::report::ReportStore` にあり、`TaskStore` はそれを
/// supertrait として要求する（ディスパッチャの `Arc<dyn TaskStore>` から報告を追記できるようにするため。
/// 実装は `report.rs` にあり、この表の SQL はここには無い）。
pub trait TaskStore: Send + Sync + crate::report::ReportStore {
    fn insert(&self, task: &Task) -> Result<(), StoreError>;
    fn get(&self, id: TaskId) -> Result<Option<Task>, StoreError>;
    fn list(&self, filter: Option<Status>) -> Result<Vec<Task>, StoreError>;
    /// イベントを追記し、割り当てられた `seq`（0始まり、task_id 内で単調増加）を返す。
    fn append_event(&self, task_id: TaskId, event: &Event) -> Result<u64, StoreError>;
    /// `task_id` に紐づく全イベントを `seq` 昇順で返す。
    fn events_for(&self, task_id: TaskId) -> Result<Vec<(u64, Event)>, StoreError>;
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
    fn delegate_children(&self, parent_id: TaskId, run_id: &str, children: Vec<Task>) -> Result<Vec<TaskId>, StoreError>;

    /// `parent_id` を親に持つタスク（終端を含む）を `created_at` 昇順（同時刻は挿入順）で返す（ADR-0016 M5 / M6）。
    fn children(&self, parent_id: TaskId) -> Result<Vec<Task>, StoreError>;

    /// ADR-0010 D2 / D7（P-7）: `status = running` かつリースの run_id が一致するときだけ `expires_at = now + ttl` に
    /// 延長して true を返す。状態遷移ではないのでイベントは追記しない。
    fn renew_lease(&self, task_id: TaskId, worker_run_id: &str, ttl: StdDuration) -> Result<bool, StoreError>;

    /// ADR-0013 D6: `events` を `id` 昇順で `after_id` より後、最大 `limit` 件返す。
    fn events_since(&self, after_id: u64, limit: usize) -> Result<Vec<EventRow>, StoreError>;
    /// ADR-0013 D6: `events` の現在の最大 `id`。行が無ければ 0。
    fn latest_event_id(&self) -> Result<u64, StoreError>;
    /// ADR-0013（Phase 9b）: 1 タスクのイベントを `seq` 昇順で、`after_seq` より後（`None` なら最初から）最大 `limit` 件、
    /// グローバル `id` と `ts` 付きで返す（API の `GET /tasks/{id}/events` と run の要約が使う）。
    fn event_rows_for(&self, task_id: TaskId, after_seq: Option<u64>, limit: usize) -> Result<Vec<EventRow>, StoreError>;

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
    /// 状態だけを変える。無い途中目標は `Ok(false)`。
    fn milestone_set_status(&self, id: MilestoneId, status: MilestoneStatus) -> Result<bool, StoreError>;
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
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
        let _journal_mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
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
            other => Err(StoreError::Invalid(format!("unknown migration version: {other}"))),
        }
    }

    fn apply_migration_version(conn: &mut Connection, version: u32) -> Result<(), StoreError> {
        let sql = Self::migration_sql(version)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
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
        let Some(kind) = OrgKind::parse(&kind_col) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid org node kind in org_nodes: {kind_col}"
            ))));
        };
        Ok((|| {
            Ok(OrgNode {
                id,
                parent_id: row.get(1)?,
                name: row.get(2)?,
                kind,
                genre: row.get(4)?,
                brief: row.get(5)?,
                position: row.get(6)?,
                created_at: parse_rfc3339(&created_at)?,
                updated_at: parse_rfc3339(&updated_at)?,
            })
        })())
    }

    fn org_list_tx(conn: &Connection) -> Result<Vec<OrgNode>, StoreError> {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id, name, kind, genre, brief, position, created_at, updated_at \
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
        let (Ok(id), Some(status)) = (id.parse::<ProjectId>(), ProjectStatus::parse(&status_col)) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid project row: id={id} status={status_col}"
            ))));
        };
        Ok((|| {
            Ok(Project {
                id,
                title: row.get(1)?,
                request: row.get(2)?,
                status,
                secretary_summary: row.get(4)?,
                created_at: parse_rfc3339(&created_at)?,
                updated_at: parse_rfc3339(&updated_at)?,
            })
        })())
    }

    fn milestone_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Milestone, StoreError>> {
        let id: String = row.get(0)?;
        let project_id: String = row.get(1)?;
        let status_col: String = row.get(5)?;
        let created_at: String = row.get(6)?;
        let updated_at: String = row.get(7)?;
        let (Ok(id), Ok(project_id), Some(status)) = (
            id.parse::<MilestoneId>(),
            project_id.parse::<ProjectId>(),
            MilestoneStatus::parse(&status_col),
        ) else {
            return Ok(Err(StoreError::Invalid(format!(
                "invalid milestone row: id={id} project_id={project_id} status={status_col}"
            ))));
        };
        Ok((|| {
            Ok(Milestone {
                id,
                project_id,
                seq: row.get(2)?,
                title: row.get(3)?,
                description: row.get(4)?,
                status,
                created_at: parse_rfc3339(&created_at)?,
                updated_at: parse_rfc3339(&updated_at)?,
            })
        })())
    }

    /// ADR-0033 D3: `report.rs`（`reports` 表の SQL）も同じ接続を使うので crate 内に公開する。
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
        conn.execute(
            "INSERT INTO tasks (id, status, kind, parent_id, priority, created_at, \
             lease_worker_run_id, lease_expires_at, json, title, updated_at, objective, genre, \
             project_id, milestone_id, assignee) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                params![status_str(task.status), new_json, task.title, updated_at_str, task_id.to_string()],
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
    fn cascade_after_transition_tx(tx: &Connection, task: &Task, from: Status, to: Status) -> Result<(), StoreError> {
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
                Self::transition_if_non_terminal_tx(tx, dependent, Trigger::DependencyFailed)?;
            }
        }
        Ok(())
    }

    /// 伝播の途中で既に終端になったタスク（例: 子でもあり後続でもある）は飛ばす。
    fn transition_if_non_terminal_tx(tx: &Connection, id: TaskId, trigger: Trigger) -> Result<(), StoreError> {
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
    fn non_terminal_dependents_tx(tx: &Connection, dep_id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        let sql = format!("SELECT json FROM tasks WHERE {} AND json LIKE ?1", Self::NON_TERMINAL_SQL);
        let mut stmt = tx.prepare(&sql)?;
        let rows = stmt.query_map(params![format!("%{dep_id}%")], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            let t = Self::row_to_task(row?)?;
            if t.id != dep_id && t.depends_on.contains(&dep_id) {
                out.push(t.id);
            }
        }
        Ok(out)
    }

    fn append_event_tx(conn: &Connection, task_id: TaskId, event: &Event) -> Result<u64, StoreError> {
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
                let rows = stmt.query_map(params![status_str(status)], |row| {
                    row.get::<_, String>(0)
                })?;
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
        // seq の採番（SELECT）と INSERT を 1 つの IMMEDIATE トランザクションにする。別接続（taskctl / API）が同じタスクに
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
        // 読んだ json を書き戻すので、間に別接続（taskctl / API）の書き込みが挟まらないよう IMMEDIATE で囲む（Phase 9 監査）。
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

            let mut deps_done = true;
            for dep_id in &task.depends_on {
                match Self::get_locked(&conn, *dep_id)? {
                    Some(dep) if dep.status == Status::Done => {}
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

    fn delegate_children(&self, parent_id: TaskId, run_id: &str, children: Vec<Task>) -> Result<Vec<TaskId>, StoreError> {
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

    fn children(&self, parent_id: TaskId) -> Result<Vec<Task>, StoreError> {
        let conn = self.lock()?;
        // 同じトランザクションで挿入した子（created_at が同じ）は挿入順（rowid）で返す。
        let mut stmt = conn.prepare("SELECT json FROM tasks WHERE parent_id = ?1 ORDER BY created_at ASC, rowid ASC")?;
        let rows = stmt.query_map(params![parent_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(Self::row_to_task(row?)?);
        }
        Ok(out)
    }

    fn renew_lease(&self, task_id: TaskId, worker_run_id: &str, ttl: StdDuration) -> Result<bool, StoreError> {
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
        let expires_at = OffsetDateTime::now_utc() + time::Duration::new(ttl.as_secs() as i64, ttl.subsec_nanos() as i32);
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
        let id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| row.get(0))?;
        Ok(id as u64)
    }

    fn event_rows_for(&self, task_id: TaskId, after_seq: Option<u64>, limit: usize) -> Result<Vec<EventRow>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, task_id, seq, ts, json FROM events WHERE task_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
        )?;
        let after: i64 = after_seq.map(u64_to_i64).unwrap_or(-1);
        let rows = stmt.query_map(params![task_id.to_string(), after, usize_to_i64(limit)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
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
            conn.query_row(&sql, params_from_iter(filter_params.iter()), |row| row.get(0))?
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
        let rows = stmt.query_map(params_from_iter(query_params.iter()), |row| row.get::<_, String>(0))?;
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
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
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
                "SELECT id, parent_id, name, kind, genre, brief, position, created_at, updated_at \
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
            "INSERT INTO org_nodes (id, parent_id, name, kind, genre, brief, position, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(id) DO UPDATE SET parent_id = excluded.parent_id, name = excluded.name, \
             kind = excluded.kind, genre = excluded.genre, brief = excluded.brief, \
             position = excluded.position, updated_at = excluded.updated_at",
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
            ],
        )?;
        tx.commit()?;
        Ok(stored)
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
            &format!("SELECT COUNT(*) FROM tasks WHERE assignee = ?1 AND {}", Self::NON_TERMINAL_SQL),
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
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO projects (id, title, request, status, secretary_summary, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                project.id.to_string(),
                project.title,
                project.request,
                project.status.as_str(),
                project.secretary_summary,
                format_rfc3339(project.created_at)?,
                format_rfc3339(project.updated_at)?,
            ],
        )?;
        Ok(())
    }

    fn project_get(&self, id: ProjectId) -> Result<Option<Project>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT id, title, request, status, secretary_summary, created_at, updated_at \
                 FROM projects WHERE id = ?1",
                params![id.to_string()],
                Self::project_row,
            )
            .optional()?;
        row.transpose()
    }

    fn project_list(&self) -> Result<Vec<Project>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, title, request, status, secretary_summary, created_at, updated_at \
             FROM projects ORDER BY created_at DESC, id DESC",
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
            return Err(StoreError::Invalid(format!("project not found: {project_id}")));
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
            "SELECT id, project_id, seq, title, description, status, created_at, updated_at \
             FROM milestones WHERE project_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![project_id.to_string()], Self::milestone_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn milestone_set_status(&self, id: MilestoneId, status: MilestoneStatus) -> Result<bool, StoreError> {
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

        store
            .release_lease(task.id, "worker-a")
            .expect("release");

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
        assert!(running_after.lease.is_none(), "leaving running must release the lease");

        // 既に done の子は触らない。
        assert_eq!(store.get(already_done.id).expect("get").expect("some").status, Status::Done);
        // 他タスクの子（parent_id が違う）も触らない。
        assert_eq!(store.get(unrelated.id).expect("get").expect("some").status, Status::Draft);
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
        assert!(store.acquire_lease(task.id, "run-1", StdDuration::from_secs(60)).unwrap());
        let extras = vec![
            Event::WorkerFinished { run_id: "run-1".into(), outcome: "done: x".into(), usage: None, role: None },
            Event::WorkerProgress { run_id: "run-1".into(), msg: "extra".into() },
        ];
        let outcome = store.apply_transition_with_events(task.id, Trigger::WorkerDone, extras).unwrap();
        assert_eq!(outcome.next, Status::Reviewing);
        let events = store.events_for(task.id).unwrap();
        let tail: Vec<(u64, String)> = events[events.len() - 3..]
            .iter()
            .map(|(seq, e)| (*seq, match e {
                Event::Transitioned { to, reason, .. } => format!("transitioned:{to:?}:{reason}"),
                Event::WorkerFinished { outcome, .. } => format!("finished:{outcome}"),
                Event::WorkerProgress { msg, .. } => format!("progress:{msg}"),
                _ => "other".into(),
            }))
            .collect();
        assert_eq!(tail, vec![
            (1, "transitioned:Reviewing:worker_done".to_string()),
            (2, "finished:done: x".to_string()),
            (3, "progress:extra".to_string()),
        ]);
        assert!(store.get(task.id).unwrap().unwrap().lease.is_none());
        // 無効な遷移では何も追記されない。
        let before = store.events_for(task.id).unwrap().len();
        assert!(store.apply_transition_with_events(task.id, Trigger::WorkerDone, vec![Event::ApprovalRequested]).is_err());
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
        let ev: Vec<Event> = store.events_for(plan.id).unwrap().into_iter().map(|(_, e)| e).collect();
        assert!(matches!(&ev[0], Event::Transitioned { to: Status::Done, reason, .. } if reason == "review_pass"));
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
        store.complete_plan(plan2.id, vec![], vec![c3.clone()], true).expect("complete_plan 2");
        let got = store.get(c3.id).unwrap().unwrap();
        assert_eq!(got.status, Status::Ready);
        let ev = store.events_for(c3.id).unwrap();
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[1].1, Event::Transitioned { from: Status::Draft, to: Status::Ready, reason } if reason == "accept"));
        assert_eq!(store.ready_tasks(10).unwrap().len(), 1);

        // 親が reviewing でなければ全体がロールバックされ、子は挿入されない。
        let mut not_reviewing = sample_task(Status::Ready);
        not_reviewing.kind = TaskKind::Plan;
        store.insert(&not_reviewing).unwrap();
        let mut c4 = sample_task(Status::Draft);
        c4.parent_id = Some(not_reviewing.id);
        let err = store.complete_plan(not_reviewing.id, vec![], vec![c4.clone()], true).unwrap_err();
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
        let ids = store.delegate_children(parent.id, "run-1", vec![a.clone(), b.clone()]).unwrap();
        assert_eq!(ids, vec![a.id, b.id]);
        for id in &ids {
            let t = store.get(*id).unwrap().unwrap();
            assert_eq!(t.status, Status::Ready);
            assert_eq!(reasons(&store, *id), vec!["accept"]);
        }
        let children = store.children(parent.id).unwrap();
        assert_eq!(children.iter().map(|t| t.id).collect::<Vec<_>>(), ids);
        let events = store.events_for(parent.id).unwrap();
        assert!(matches!(&events.last().unwrap().1, Event::Delegated { run_id, task_ids } if run_id == "run-1" && *task_ids == ids));
        assert_eq!(store.get(parent.id).unwrap().unwrap().status, Status::Running);
        // 親が違う子は拒否され、何も挿入されない。
        let mut stray = sample_task(Status::Draft);
        stray.parent_id = Some(a.id);
        assert!(store.delegate_children(parent.id, "run-2", vec![stray.clone()]).is_err());
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
        store.create_task(&task, vec![Event::ApprovalRequested]).unwrap();
        assert_eq!(store.get(task.id).unwrap().unwrap(), task);
        let ev = store.events_for(task.id).unwrap();
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[0].1, Event::Created { task: t } if t.id == task.id));
        assert_eq!(ev[1].1, Event::ApprovalRequested);
        // 同じ id の再作成は insert で失敗し、イベントも追記されない。
        assert!(store.create_task(&task, vec![Event::ApprovalRequested]).is_err());
        assert_eq!(store.events_for(task.id).unwrap().len(), 2);
    }

    /// ADR-0010 D7（P-7）: renew_lease は running かつ run_id が一致するときだけ期限を更新する。
    #[test]
    fn renew_lease_extends_only_the_matching_running_lease() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task(Status::Ready);
        store.insert(&task).unwrap();
        assert!(store.acquire_lease(task.id, "run-a", StdDuration::from_secs(5)).unwrap());
        let before = store.get(task.id).unwrap().unwrap().lease.unwrap().expires_at;
        assert!(store.renew_lease(task.id, "run-a", StdDuration::from_secs(3600)).unwrap());
        let after = store.get(task.id).unwrap().unwrap().lease.unwrap();
        assert_eq!(after.worker_run_id, "run-a");
        assert!(after.expires_at > before + time::Duration::seconds(3000));
        let col: String = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("SELECT lease_expires_at FROM tasks WHERE id = ?1", params![task.id.to_string()], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(col, format_rfc3339(after.expires_at).unwrap());
        // 別 run_id や running でないタスクには効かない。
        assert!(!store.renew_lease(task.id, "run-b", StdDuration::from_secs(1)).unwrap());
        store.apply_transition(task.id, Trigger::WorkerDone, None).unwrap();
        assert!(!store.renew_lease(task.id, "run-a", StdDuration::from_secs(1)).unwrap());
        assert!(!store.renew_lease(TaskId::new(), "run-a", StdDuration::from_secs(1)).unwrap());
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
            assert!(matches!(store.apply_transition(t.id, Trigger::Cancel, None), Err(StoreError::InvalidTransition(_))));
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
        store.apply_transition(approval.id, Trigger::Cancel, None).unwrap();
        assert_eq!(store.get(child.id).unwrap().unwrap().status, Status::Cancelled);
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

        store.apply_transition(parent.id, Trigger::ReviewPass, None).unwrap();
        assert_eq!(store.get(pending_approval.id).unwrap().unwrap().status, Status::Cancelled);
        assert_eq!(store.get(decided_approval.id).unwrap().unwrap().status, Status::Done);
        assert_eq!(store.get(exec_child.id).unwrap().unwrap().status, Status::Draft);
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

        assert!(store.acquire_lease(a.id, "run-a", StdDuration::from_secs(60)).unwrap());
        let outcome = store.apply_transition(a.id, Trigger::WorkerError { retryable: false }, None).unwrap();
        assert_eq!(outcome.next, Status::Failed);

        for id in [b.id, c.id] {
            let t = store.get(id).unwrap().unwrap();
            assert_eq!(t.status, Status::Cancelled);
            assert_eq!(t.attempts, 0);
            assert_eq!(reasons(&store, id), vec!["dependency_failed"]);
        }
        assert_eq!(store.get(d.id).unwrap().unwrap().status, Status::Done);
        assert_eq!(store.get(unrelated.id).unwrap().unwrap().status, Status::Ready);
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
        assert!(serde_json::to_string(&new).unwrap().contains(r#""provider":"acct-a""#));
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
        assert_eq!(ev, Event::WorkerFinished { run_id: "r".into(), outcome: "done: x".into(), usage: None, role: None });
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
        let (lease_worker_run_id, lease_expires_at): (Option<String>, Option<String>) = match &task.lease {
            Some(l) => (Some(l.worker_run_id.clone()), Some(format_rfc3339(l.expires_at).unwrap())),
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
            let ev = Event::Created { task: Box::new(a.clone()) };
            insert_legacy_event(&conn, a.id, 0, &ev);
            expected_a.push((0, ev));
            expected_global.push((a.id, 0));

            let ev = Event::Created { task: Box::new(b.clone()) };
            insert_legacy_event(&conn, b.id, 0, &ev);
            expected_b.push((0, ev));
            expected_global.push((b.id, 0));

            let ev = Event::Transitioned { from: Status::Draft, to: Status::Ready, reason: "accept".into() };
            insert_legacy_event(&conn, a.id, 1, &ev);
            expected_a.push((1, ev));
            expected_global.push((a.id, 1));

            let ev = Event::ApprovalRequested;
            insert_legacy_event(&conn, b.id, 1, &ev);
            expected_b.push((1, ev));
            expected_global.push((b.id, 1));

            let ev = Event::WorkerProgress { run_id: "r".into(), msg: "go".into() };
            insert_legacy_event(&conn, a.id, 2, &ev);
            expected_a.push((2, ev));
            expected_global.push((a.id, 2));
        }

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let since = store.events_since(0, 100).unwrap();
        assert_eq!(since.len(), 5);
        let got_order: Vec<(TaskId, u64)> = since.iter().map(|r| (r.task_id, r.seq)).collect();
        assert_eq!(got_order, expected_global, "events_since id order must match original rowid order");
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
        assert_eq!(applied_before, applied_after, "second open must not re-apply migrations");
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

        let got = store.get(task.id).unwrap().expect("task still readable after migration");
        assert_eq!(got.genre, None);
        assert_eq!(got.objective, task.objective);

        let genre_col: Option<String> = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("SELECT genre FROM tasks WHERE id = ?1", params![task.id.to_string()], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(genre_col, None, "migration must not invent a genre for pre-existing rows");
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
            conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap()
        };
        assert_eq!(mode.to_lowercase(), "wal");
    }

    /// Phase 9 監査（受け入れ 2）: 2 つの接続（ディスパッチャと taskctl / API 相当）が、読んでから書くトランザクションを
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
        ha.join().unwrap().expect("writer a never sees database is locked");
        hb.join().unwrap().expect("writer b never sees database is locked");
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
        assert!(store.event_rows_for(TaskId::new(), None, 10).unwrap().is_empty());
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
            .append_event(a.id, &Event::WorkerProgress { run_id: "r".into(), msg: "x".into() })
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

    fn task_with(title: &str, status: Status, kind: TaskKind, priority: i32, parent: Option<TaskId>) -> Task {
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
        let child_exec_ready = task_with("child a", Status::Ready, TaskKind::Execute, 0, Some(root.id));
        store.insert(&child_exec_ready).unwrap();
        let child_exec_done = task_with("child b", Status::Done, TaskKind::Execute, 0, Some(root.id));
        store.insert(&child_exec_done).unwrap();
        let child_approval = task_with("approve 100%", Status::Ready, TaskKind::Approval, 0, Some(root.id));
        store.insert(&child_approval).unwrap();
        let other_root = task_with("other_root", Status::Draft, TaskKind::Execute, 0, None);
        store.insert(&other_root).unwrap();

        // statuses: 複数指定は OR。
        let f = ListFilter { statuses: vec![Status::Ready, Status::Draft], ..Default::default() };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(ids, [root.id, child_exec_ready.id, child_approval.id, other_root.id].into_iter().collect());
        assert_eq!(page.total, 4);

        // kind
        let f = ListFilter { kinds: vec![TaskKind::Approval], ..Default::default() };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
        assert_eq!(page.items.iter().map(|t| t.id).collect::<Vec<_>>(), vec![child_approval.id]);
        assert_eq!(page.total, 1);

        // parent
        let f = ListFilter { parent_id: Some(root.id), ..Default::default() };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(ids, [child_exec_ready.id, child_exec_done.id, child_approval.id].into_iter().collect());
        assert_eq!(page.total, 3);

        // root_only
        let f = ListFilter { root_only: true, ..Default::default() };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(ids, [root.id, other_root.id].into_iter().collect());
        assert_eq!(page.total, 2);

        // text_contains: リテラルな '%' を含む検索語（エスケープが効いているか）。
        let percent_task = task_with("100% done", Status::Ready, TaskKind::Execute, 0, None);
        store.insert(&percent_task).unwrap();
        let no_percent_task = task_with("100 done", Status::Ready, TaskKind::Execute, 0, None);
        store.insert(&no_percent_task).unwrap();
        let f = ListFilter { text_contains: Some("100%".to_string()), ..Default::default() };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
        let ids: HashSet<_> = page.items.iter().map(|t| t.id).collect();
        assert_eq!(ids, [child_approval.id, percent_task.id].into_iter().collect());
        assert!(!ids.contains(&no_percent_task.id));

        // ADR-0014 D2: text_contains は objective も対象にする（ASCII の大文字小文字は区別しない）。
        let mut by_objective = task_with("plain title", Status::Ready, TaskKind::Execute, 0, None);
        by_objective.objective = "migrate the billing service".to_string();
        store.insert(&by_objective).unwrap();
        let f = ListFilter { text_contains: Some("billing".to_string()), ..Default::default() };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
        assert_eq!(page.items.iter().map(|t| t.id).collect::<Vec<_>>(), vec![by_objective.id]);
        let f = ListFilter { text_contains: Some("BILLING".to_string()), ..Default::default() };
        assert_eq!(store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap().total, 1);
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

        let f = ListFilter { genres: vec!["coding".to_string()], ..Default::default() };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
        assert_eq!(page.items.iter().map(|t| t.id).collect::<Vec<_>>(), vec![coding.id]);
        assert_eq!(page.total, 1);

        let f = ListFilter {
            genres: vec!["coding".to_string(), "literature".to_string()],
            ..Default::default()
        };
        let page = store.list_page(&f, ListOrder::CreatedDesc, None, 10).unwrap();
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

        let page = store.list_page(&ListFilter::default(), ListOrder::Dispatch, None, 10).unwrap();
        assert_eq!(page.items.iter().map(|t| t.id).collect::<Vec<_>>(), expected_desc);

        let page = store.list_page(&ListFilter::default(), ListOrder::CreatedDesc, None, 10).unwrap();
        assert_eq!(page.items.iter().map(|t| t.id).collect::<Vec<_>>(), expected_desc);

        let page = store.list_page(&ListFilter::default(), ListOrder::UpdatedDesc, None, 10).unwrap();
        assert_eq!(page.items.iter().map(|t| t.id).collect::<Vec<_>>(), expected_desc);

        // ids[0] は priority 最小・最も古い。cancel して updated_at を更新すると UpdatedDesc の先頭になる。
        store.apply_transition(ids[0], Trigger::Cancel, None).unwrap();
        let page = store.list_page(&ListFilter::default(), ListOrder::UpdatedDesc, None, 10).unwrap();
        assert_eq!(page.items[0].id, ids[0]);
        // Dispatch 順は status を見ないので変わらない（cancelled でも一覧には出る）。
        let page = store.list_page(&ListFilter::default(), ListOrder::Dispatch, None, 10).unwrap();
        assert_eq!(page.items.iter().map(|t| t.id).collect::<Vec<_>>(), expected_desc);
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

        let full = store.list_page(&ListFilter::default(), ListOrder::Dispatch, None, 100).unwrap();
        assert_eq!(full.total, 7);

        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = store.list_page(&ListFilter::default(), ListOrder::Dispatch, cursor.as_deref(), 3).unwrap();
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
            store.list_page(&ListFilter::default(), ListOrder::Dispatch, Some("not-a-cursor"), 10),
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
        let old = r#"{"type":"provider_throttled","provider":"acct-a","until":"2024-01-01T00:00:00Z"}"#;
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
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/v1/event.schema.json");
        let generated = serde_json::to_string_pretty(&event_row_schema_value()).unwrap() + "\n";
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(committed, generated, "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-core`");
    }

    // ---- ADR-0033 D1/D2（Phase 23）: 組織・案件・途中目標 ----

    fn org_node(id: &str, parent: Option<&str>, kind: OrgKind) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
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
        store.org_upsert(&org_node("secretary", None, OrgKind::Secretary)).expect("secretary");
    }

    /// 版数 5 の DB を開くと 6 が適用され、もう一度開いても何も起きない（冪等）。既存行は壊れない。
    #[test]
    fn open_migrates_schema_5_db_to_6_and_reapplying_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema5.sqlite3");
        let task = sample_task(Status::Draft);
        {
            let conn = Connection::open(&path).unwrap();
            for sql in [MIGRATION_0001, MIGRATION_0002, MIGRATION_0003, MIGRATION_0004, MIGRATION_0005] {
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
        assert_eq!(store.schema_version().unwrap(), 6);
        let got = store.get(task.id).unwrap().expect("old row still readable");
        assert_eq!(got.project_id, None);
        assert_eq!(got.milestone_id, None);
        assert_eq!(got.assignee, None);
        assert!(store.org_list().unwrap().is_empty());
        seed_secretary(&store);
        drop(store);

        // 2 回目に開いても 0006 は再適用されず（適用済み）、中身も残る。
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 6);
        assert_eq!(store.org_list().unwrap().len(), 1);
        let applied: i64 = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM schema_migrations WHERE version = 6", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(applied, 1, "migration 6 must be recorded exactly once");
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
        assert_eq!(updated.created_at, stored.created_at, "created_at is kept on update");
        assert_eq!(updated.name, "コーディング部");
        assert_eq!(updated.genre.as_deref(), Some("coding"));

        // 並び順は position（同値なら id）の昇順。
        let mut infra = org_node("infra", Some("secretary"), OrgKind::Department);
        infra.position = 1;
        store.org_upsert(&infra).unwrap();
        let ids: Vec<String> = store.org_list().unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(ids, vec!["secretary".to_string(), "infra".into(), "coding".into()]);
    }

    #[test]
    fn org_upsert_rejects_a_second_secretary_and_cycles() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_secretary(&store);
        store.org_upsert(&org_node("coding", Some("secretary"), OrgKind::Department)).unwrap();
        store.org_upsert(&org_node("poc", Some("coding"), OrgKind::Section)).unwrap();

        let err = store.org_upsert(&org_node("boss", None, OrgKind::Secretary)).unwrap_err();
        assert!(matches!(err, StoreError::Org(OrgError::DuplicateSecretary { .. })), "{err}");
        let err = store
            .org_upsert(&org_node("coding", Some("coding"), OrgKind::Department))
            .unwrap_err();
        assert!(matches!(err, StoreError::Org(OrgError::Cycle { .. })), "{err}");
        let err = store.org_upsert(&org_node("x", Some("ghost"), OrgKind::Section)).unwrap_err();
        assert!(matches!(err, StoreError::Org(OrgError::UnknownParent { .. })), "{err}");
        assert_eq!(store.org_list().unwrap().len(), 3, "nothing was written by the failed upserts");
    }

    #[test]
    fn org_delete_refuses_while_a_task_is_open_or_children_remain() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_secretary(&store);
        store.org_upsert(&org_node("coding", Some("secretary"), OrgKind::Department)).unwrap();
        store.org_upsert(&org_node("poc", Some("coding"), OrgKind::Section)).unwrap();

        let mut task = sample_task(Status::Ready);
        task.assignee = Some("poc".into());
        store.insert(&task).unwrap();

        let err = store.org_delete("poc").unwrap_err();
        assert!(matches!(err, StoreError::InUse { .. }), "{err}");
        // 子を抱えた部も消せない。
        let err = store.org_delete("coding").unwrap_err();
        assert!(matches!(err, StoreError::InUse { .. }), "{err}");
        // タスクが終端になれば消せる。
        store.apply_transition(task.id, Trigger::Cancel, None).unwrap();
        assert!(store.org_delete("poc").unwrap());
        assert!(store.org_get("poc").unwrap().is_none());
        assert!(!store.org_delete("poc").unwrap(), "deleting a missing node is Ok(false)");
    }

    fn sample_project() -> Project {
        let now = OffsetDateTime::now_utc();
        Project {
            id: ProjectId::new(),
            title: "Pluvio の新テーマ".into(),
            request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証".into(),
            status: ProjectStatus::Proposed,
            secretary_summary: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn projects_and_milestones_round_trip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let project = sample_project();
        store.project_create(&project).unwrap();
        assert_eq!(store.project_get(project.id).unwrap().as_ref(), Some(&project));
        assert_eq!(store.project_list().unwrap().len(), 1);

        assert!(store.project_set_status(project.id, ProjectStatus::Active).unwrap());
        let got = store.project_get(project.id).unwrap().unwrap();
        assert_eq!(got.status, ProjectStatus::Active);
        assert!(!store.project_set_status(ProjectId::new(), ProjectStatus::Done).unwrap());

        let first = store
            .milestone_create(project.id, "関連研究を棚卸しする", "候補を 3 本", MilestoneStatus::Proposed)
            .unwrap();
        let second = store
            .milestone_create(project.id, "小さな検証を回す", "", MilestoneStatus::Proposed)
            .unwrap();
        assert_eq!((first.seq, second.seq), (1, 2), "seq is numbered per project");
        assert_eq!(
            store.milestone_list(project.id).unwrap().iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![first.id, second.id]
        );
        assert!(store.milestone_set_status(second.id, MilestoneStatus::Approved).unwrap());
        assert_eq!(store.milestone_list(project.id).unwrap()[1].status, MilestoneStatus::Approved);
        assert!(!store.milestone_set_status(MilestoneId::new(), MilestoneStatus::Reached).unwrap());

        let err = store
            .milestone_create(ProjectId::new(), "無い案件", "", MilestoneStatus::Proposed)
            .unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)), "{err}");
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

        let filter = ListFilter { project_id: Some(project.id), ..ListFilter::default() };
        let page = store.list_page(&filter, ListOrder::CreatedDesc, None, 10).unwrap();
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
        let mut json: serde_json::Value = serde_json::from_str(&serde_json::to_string(&task).unwrap()).unwrap();
        let obj = json.as_object_mut().unwrap();
        assert!(!obj.contains_key("project_id"), "None is skipped on serialization");
        obj.remove("genre");
        let back: Task = serde_json::from_value(json).unwrap();
        assert_eq!(back.project_id, None);
        assert_eq!(back.assignee, None);
    }
}
