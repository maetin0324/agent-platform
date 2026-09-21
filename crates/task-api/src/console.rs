//! ADR-0048 D1（Phase 60a）: Console の**読み取り側**。
//!
//! - `GET /console?scope=all|project:<id>|node:<id>&since=<cursor>&limit=` — 初期表示用の履歴
//!   （時刻順、カーソル付き）。
//! - `GET /console/stream?scope=…` — SSE。同じブロックの形を、起きた順に流す。
//!
//! `POST /console/instruct`（D3。Phase 60b）: 素の文の入力口。`@<node-id> ` で始まる文か
//! `scope=node:<id>` はそのノードとの対話（既存の `POST /org/{id}/messages` と同じ経路）に、
//! それ以外は CoS（根ノード）との対話 run になる。ここは**入口を選ぶだけ**（判断は
//! `task_ops::conversation::start`。LLM は呼ばない）。
//!
//! 読み取りは状態を変えない。引くのは `events` / `messages` / `approvals` / `reports` / `milestones`
//! の 5 つで、どれも上限付き（`EVENT_WINDOW` / `limit`）。LLM も判断も無い（DESIGN 原則 1〜4）。
//!
//! ブロックの束ね方・1 行の作り方・カーソルは `task_ops::console` にある（`GET /tasks/{id}/timeline`
//! と同じ「時刻で 1 本に並べる」規則を共有する）。

use std::collections::{HashMap, HashSet};

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    ApprovalStore, COS_ID, Event, EventRow, KnowledgeRunState, KnowledgeRunStore, ListFilter,
    ListOrder, Message, MessageRole, MilestoneStatus, NodeSessionStore, OrgKind, ProjectId,
    ReportFilter, ReportStore, SessionKind, SqliteStore, Status, Task, TaskId, TaskStore,
};
use task_ops::console::{ConsoleCursor, at_nanos, group_progress, task_line};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, json_response, no_query, read_json, rfc3339};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, ops_problem, store_problem};
use crate::query::{QueryParams, parse_task_id};
use crate::state::ApiState;
use crate::types::{ConsoleBlock, ConsolePage, MilestoneReviewView};

/// `GET /console` の既定の件数。
pub(crate) const DEFAULT_LIMIT: usize = 100;
/// `limit` の上限（ADR-0048 D1: 初期表示は軽く）。
pub const MAX_LIMIT: usize = 200;
/// 1 回に読む `events` の最大件数（カーソルからの読み進み幅）。
pub const EVENT_WINDOW: usize = 4_000;
/// 対話・認可・報告を 1 回に読む最大件数（`limit` の 4 倍、ただしこの上限まで）。
const SIDE_WINDOW_MAX: usize = 800;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new()
        .route("/api/v1/console", axum::routing::get(console))
        .route("/api/v1/console/stream", axum::routing::get(stream))
        .route("/api/v1/console/instruct", axum::routing::post(instruct))
        // ADR-0054 D1（Phase 67）: CoS の継続セッションを捨てる（次の run から新規セッション）。
        .route(
            "/api/v1/console/new-conversation",
            axum::routing::post(new_conversation),
        )
        // ADR-0048 D2: 折り畳んだ `progress` を開いたときに取る、その run の全行。
        .route(
            "/api/v1/tasks/{id}/runs/{run_id}/events",
            axum::routing::get(run_events),
        )
}

// ---- POST /console/instruct（ADR-0048 D3）----

/// `POST /console/instruct` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstructBody {
    /// 本文（空白だけは 422。判定は `task_ops::conversation::start` に任せる）。
    pub text: String,
    /// 省略・`all` は CoS。`node:<id>` はそのノード。`project:<id>` は CoS にその案件を紐づける
    /// （`text` が `@<node-id> ` で始まればそちらが勝つ）。
    #[serde(default)]
    pub scope: Option<String>,
}

/// `POST /console/instruct` の応答（202）。`POST /org/{id}/messages` と同じ形に `node_id` を添える。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConsoleInstructAccepted {
    /// 入った `role = "user"` の行の id。
    pub message_id: String,
    /// そのノードの run を起こすために作られた対話用タスク。
    pub task_id: TaskId,
    /// 実際に話しかけた相手（`node:<id>` / `@<node-id>` ならそのノード、それ以外は CoS の id）。
    pub node_id: String,
}

/// 誰に話しかけるか（ADR-0048 D3）。判断は決定的（`@` の有無と `scope` の形だけを見る）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum InstructTarget {
    /// 明示のノード（`scope=node:<id>` か `@<node-id> ` 始まりの文）。
    Node(String),
    /// CoS（根ノード）。`Some` なら案件に紐づける。
    Cos(Option<ProjectId>),
}

/// 文が `@<node-id> ` で始まっていれば `(node_id, 残りの本文)`。空白の無い `@node` だけ・
/// 先頭の `@` の直後が空白のときは対象なし（`None`）。
fn mention(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix('@')?;
    let (node_id, remainder) = rest.split_once(char::is_whitespace)?;
    if node_id.is_empty() {
        return None;
    }
    Some((node_id, remainder.trim_start()))
}

/// `scope` と本文の `@` から、話しかける相手と実際に送る本文を決める（ADR-0048 D3）。
fn resolve_target(scope: Option<&str>, text: &str) -> Result<(InstructTarget, String), ApiProblem> {
    match Scope::parse(scope)? {
        Scope::Node(id) => Ok((InstructTarget::Node(id), text.to_string())),
        Scope::Project(id) => match mention(text) {
            Some((node_id, rest)) => {
                Ok((InstructTarget::Node(node_id.to_string()), rest.to_string()))
            }
            None => Ok((InstructTarget::Cos(Some(id)), text.to_string())),
        },
        Scope::All => match mention(text) {
            Some((node_id, rest)) => {
                Ok((InstructTarget::Node(node_id.to_string()), rest.to_string()))
            }
            None => Ok((InstructTarget::Cos(None), text.to_string())),
        },
    }
}

/// `POST /console/instruct`（管理系。`POST /org/{id}/messages` と同じ規律）。
async fn instruct(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let post: InstructBody = read_json(body, false).await?;
    let (target, text) = resolve_target(post.scope.as_deref(), &post.text)?;
    let roles = state.inner.roles.clone();
    let genres = state.inner.genres.clone();
    let conversation_genre = state.inner.conversation_genre.clone();
    let started = state
        .blocking(move |store| {
            let (node_id, project_id) = match target {
                InstructTarget::Node(node_id) => {
                    if store.org_get(&node_id).map_err(store_problem)?.is_none() {
                        return Err(ApiProblem::org_node_not_found(&node_id));
                    }
                    (node_id, None)
                }
                InstructTarget::Cos(project_id) => {
                    let cos = store
                        .org_list()
                        .map_err(store_problem)?
                        .into_iter()
                        .find(|n| n.kind == OrgKind::Secretary)
                        .ok_or_else(|| ApiProblem::org_node_not_found("cos"))?;
                    (cos.id, project_id)
                }
            };
            task_ops::conversation::start(
                store,
                &node_id,
                project_id,
                &text,
                &roles,
                &genres,
                &conversation_genre,
                OffsetDateTime::now_utc(),
            )
            .map_err(|e| ops_problem(store, e, None))
        })
        .await?;
    tracing::info!(
        who = "admin",
        op = "console_instruct",
        node_id = %started.message.node_id,
        task_id = %started.task.id,
        "admin: instructed the console"
    );
    Ok(json_response(
        StatusCode::ACCEPTED,
        &ConsoleInstructAccepted {
            message_id: started.message.id.to_string(),
            task_id: started.task.id,
            node_id: started.message.node_id,
        },
    ))
}

// ---- POST /console/new-conversation（ADR-0054 D1。Phase 67）----

/// `POST /console/new-conversation`: CoS の継続セッション（`node_sessions`）を捨てる。次の対話 run は
/// 新規セッションから始まる（前置きは全量に戻り、ADR-0033 D4 の対話履歴の末尾 20 件が「これまでの
/// 要約」として乗る。`task_dispatch::sessions::FreshReason::NoActive` と同じ経路）。書くのは
/// `node_sessions` の 1 行だけ（ディスパッチへの依存は無い、thin な管理操作）。現役セッションが
/// 既に無くても 204（結果として「無い」状態にするだけなので、無かったことをエラーにしない）。
async fn new_conversation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    state
        .blocking(move |store| {
            let retired = store
                .node_session_retire(COS_ID, SessionKind::Conversation, None, OffsetDateTime::now_utc())
                .map_err(store_problem)?;
            tracing::info!(
                who = "admin",
                op = "console_new_conversation",
                retired,
                "admin: retired the CoS continuation session"
            );
            Ok(())
        })
        .await?;
    Ok(axum::response::IntoResponse::into_response(
        StatusCode::NO_CONTENT,
    ))
}

// ---- 範囲 ----

/// `scope=all|project:<id>|node:<id>`（ADR-0048 D1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Project(ProjectId),
    Node(String),
}

impl Scope {
    /// 省略（`None`）は `all`。形が違えば 400。
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, ApiProblem> {
        match value.map(str::trim) {
            None | Some("") | Some("all") => Ok(Scope::All),
            Some(s) => match s.split_once(':') {
                Some(("project", id)) => {
                    id.parse::<ProjectId>().map(Scope::Project).map_err(|_| {
                        ApiProblem::bad_request(format!("`{id}` is not a project id (ULID)"))
                    })
                }
                Some(("node", id)) if !id.is_empty() => Ok(Scope::Node(id.to_string())),
                _ => Err(ApiProblem::bad_request(
                    "query parameter `scope` must be `all`, `project:<id>` or `node:<id>`",
                )),
            },
        }
    }

    fn project(&self) -> Option<ProjectId> {
        match self {
            Scope::Project(id) => Some(*id),
            _ => None,
        }
    }

    fn node(&self) -> Option<&str> {
        match self {
            Scope::Node(id) => Some(id.as_str()),
            _ => None,
        }
    }

    /// そのタスクがこの範囲に入るか（案件はタスクの `project_id`、ノードは `assignee`）。
    fn covers(&self, task: &Task) -> bool {
        match self {
            Scope::All => true,
            Scope::Project(id) => task.project_id == Some(*id),
            Scope::Node(id) => task.assignee.as_deref() == Some(id.as_str()),
        }
    }
}

// ---- GET /console ----

async fn console(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["scope", "since", "limit"])?;
    let scope = Scope::parse(query.single("scope")?)?;
    let since = parse_since(query.single("since")?)?;
    let limit = query.limit("limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let page = state
        .blocking(move |store| collect(store, &scope, since.as_ref(), limit))
        .await?;
    Ok(json_response(StatusCode::OK, &page))
}

pub(crate) fn parse_since(value: Option<&str>) -> Result<Option<ConsoleCursor>, ApiProblem> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => ConsoleCursor::decode(s)
            .map(Some)
            .ok_or_else(|| ApiProblem::bad_request(format!("`{s}` is not a console cursor"))),
    }
}

/// 履歴を 1 ページ組む（ADR-0048 D1）。
///
/// - `since` 無し: **いちばん新しい `limit` 件**（時刻の昇順で返す）。
/// - `since` 有り: そのカーソルより**後**の `limit` 件。
///
/// どの引きも上限付き（`events` は `EVENT_WINDOW`、他は `limit * 4`）。
pub(crate) fn collect(
    store: &SqliteStore,
    scope: &Scope,
    since: Option<&ConsoleCursor>,
    limit: usize,
) -> Result<ConsolePage, ApiProblem> {
    let mut blocks = Vec::new();
    let mut tasks = TaskCache::default();

    // 1. イベント（遷移・進行・質問）。
    let after_id = match since {
        Some(cursor) => cursor.event_id,
        None => {
            let latest = store.latest_event_id().map_err(store_problem)?;
            latest.saturating_sub(EVENT_WINDOW as u64)
        }
    };
    let rows = store
        .events_since(after_id, EVENT_WINDOW)
        .map_err(store_problem)?;
    blocks.extend(event_blocks(store, scope, &rows, &mut tasks)?);

    // 2. イベントに無いもの（対話・認可・途中目標・報告）。
    let side_limit = limit
        .saturating_mul(4)
        .clamp(DEFAULT_LIMIT, SIDE_WINDOW_MAX);
    let after_at = since.map(|c| c.at_nanos);
    blocks.extend(side_blocks(store, scope, after_at, side_limit, &mut tasks)?);

    // 3. 時刻順に並べ、カーソルで切って `limit` 件。
    blocks.sort_by_key(block_key);
    if let Some(since) = since {
        let key = (since.at_nanos, since.tie.clone());
        blocks.retain(|b| block_key(b) > key);
        blocks.truncate(limit);
    } else if blocks.len() > limit {
        blocks.drain(..blocks.len() - limit);
    }

    // 4. 次に読む位置（返したブロックまで）。イベントの読み進みは**返した分だけ**進める。
    let next_cursor = blocks
        .last()
        .map(|last| {
            let event_id = blocks
                .iter()
                .filter_map(|b| ConsoleCursor::decode(cursor_of(b)).map(|c| c.event_id))
                .max()
                .unwrap_or(0)
                .max(since.map(|c| c.event_id).unwrap_or(0));
            let (at_nanos, tie) = block_key(last);
            ConsoleCursor::new(at_nanos, tie, event_id).encode()
        })
        .or_else(|| since.map(ConsoleCursor::encode));
    Ok(ConsolePage {
        items: blocks,
        next_cursor,
    })
}

// ---- イベントから作るブロック ----

/// `events` の窓から `task` / `progress` / `question` を作る。
pub(crate) fn event_blocks(
    store: &SqliteStore,
    scope: &Scope,
    rows: &[EventRow],
    tasks: &mut TaskCache,
) -> Result<Vec<ConsoleBlock>, ApiProblem> {
    // 範囲に入るタスクの行だけを残す（消えたタスクの行は捨てる）。
    let mut mine: Vec<&EventRow> = Vec::new();
    for row in rows {
        if tasks
            .get(store, row.task_id)?
            .is_some_and(|t| scope.covers(t))
        {
            mine.push(row);
        }
    }

    let mut blocks = Vec::new();
    // 認可に同じ質問があるなら、質問ブロックは出さない（同じことを 2 回出さない）。
    let answered_questions = answered_texts(&mine);
    let approval_questions = approval_question_texts(store, scope)?;

    for row in &mine {
        let Some(task) = tasks.get(store, row.task_id)? else {
            continue;
        };
        match &row.event {
            Event::Transitioned { .. } => {
                if let Some(line) = task_line(row, task) {
                    blocks.push(ConsoleBlock::Task {
                        at: row.ts.clone(),
                        cursor: event_cursor(row).encode(),
                        task: line,
                    });
                }
            }
            Event::QuestionRaised { run_id, text } => {
                if approval_questions.contains(&(task.id, text.clone())) {
                    continue;
                }
                let answer = answered_questions.get(&(task.id, text.clone())).cloned();
                blocks.push(ConsoleBlock::Question {
                    at: row.ts.clone(),
                    cursor: event_cursor(row).encode(),
                    task_id: task.id,
                    run_id: run_id.clone(),
                    node_id: task.assignee.clone(),
                    project_id: task.project_id,
                    // 窓の中に回答が無くても、そのタスクが `blocked` を抜けていれば答え済み。
                    answered: answer.is_some() || task.status != Status::Blocked,
                    answer,
                    text: text.clone(),
                });
            }
            _ => {}
        }
    }

    // 進行は run ごとに 1 件へ束ねる（ADR-0048 D1 / D2）。**対話 run**（ADR-0054 D2。Phase 68）は
    // 折り畳んだ `progress` ではなく、「育つ返事」（`reply`、`state = streaming`）として流す
    // （human の直後に同じ吹き出しが thinking → tool call → text と育つ。それ以外の run は従来どおり
    // 折り畳みの `progress`）。
    let mut work_rows: Vec<&EventRow> = Vec::new();
    let mut conv_rows: Vec<&EventRow> = Vec::new();
    for row in mine.iter().copied() {
        if !matches!(&row.event, Event::WorkerProgress { .. }) {
            continue;
        }
        match tasks.get(store, row.task_id)? {
            Some(task) if task_core::is_conversation(task) => conv_rows.push(row),
            Some(_) => work_rows.push(row),
            None => {}
        }
    }
    for group in group_progress(work_rows.iter().copied()) {
        let Some(task) = tasks.get(store, group.task_id)? else {
            continue;
        };
        // 束の位置は「最初の行の時刻」、読み進みは「最後の行の id」。
        let last_id = mine
            .iter()
            .filter(|r| {
                r.task_id == group.task_id && progress_run(&r.event) == Some(group.run_id.as_str())
            })
            .map(|r| r.id)
            .max()
            .unwrap_or(0);
        let cursor = ConsoleCursor::new(
            at_nanos(&group.started_at),
            task_ops::console::progress_tie(group.task_id, &group.run_id),
            last_id,
        );
        blocks.push(ConsoleBlock::Progress {
            at: group.started_at.clone(),
            cursor: cursor.encode(),
            title: task.title.clone(),
            assignee: task.assignee.clone(),
            harness: task.worker_hint.adapter.clone(),
            tier: task.worker_hint.tier,
            project_id: task.project_id,
            progress: group,
        });
    }
    for accum in task_ops::console::group_conversation_progress(conv_rows.iter().copied()) {
        let Some(task) = tasks.get(store, accum.task_id)? else {
            continue;
        };
        let last_id = mine
            .iter()
            .filter(|r| {
                r.task_id == accum.task_id && progress_run(&r.event) == Some(accum.run_id.as_str())
            })
            .map(|r| r.id)
            .max()
            .unwrap_or(0);
        let cursor = ConsoleCursor::new(
            at_nanos(&accum.started_at),
            task_ops::console::progress_tie(accum.task_id, &accum.run_id),
            last_id,
        );
        blocks.push(ConsoleBlock::Reply {
            at: accum.started_at.clone(),
            cursor: cursor.encode(),
            // まだ `messages` の行は無い（run 中）ので、run を指す合成 id にする（`GET /console` の
            // 応答の中で一意であればよく、この文字列を別の API に渡すことはしない）。
            message_id: format!("streaming:{}:{}", accum.task_id, accum.run_id),
            node_id: task.assignee.clone().unwrap_or_default(),
            project_id: task.project_id,
            task_id: Some(task.id),
            run_id: Some(accum.run_id.clone()),
            text: accum.text.clone(),
            actions_result: None,
            state: task_ops::console::ConsoleReplyState::Streaming,
            thinking: accum.thinking.clone(),
            steps: accum.steps.clone(),
        });
    }
    Ok(blocks)
}

fn progress_run(event: &Event) -> Option<&str> {
    match event {
        Event::WorkerProgress { run_id, .. } => Some(run_id.as_str()),
        _ => None,
    }
}

/// 窓の中の `Event::Answered`（同じタスク・同じ質問文）。
fn answered_texts(rows: &[&EventRow]) -> HashMap<(TaskId, String), String> {
    let mut out = HashMap::new();
    for row in rows {
        if let Event::Answered { question, answer } = &row.event {
            out.insert((row.task_id, question.clone()), answer.clone());
        }
    }
    out
}

/// 認可として残っている質問（タスク × 質問文）。ここに有る質問はイベント側では出さない。
fn approval_question_texts(
    store: &SqliteStore,
    scope: &Scope,
) -> Result<HashSet<(TaskId, String)>, ApiProblem> {
    let mut out = HashSet::new();
    for approval in store
        .approval_list(None, scope.project(), scope.node())
        .map_err(store_problem)?
    {
        if let Some(task_id) = approval.task_id {
            out.insert((task_id, approval.question));
        }
    }
    Ok(out)
}

fn event_cursor(row: &EventRow) -> ConsoleCursor {
    ConsoleCursor::new(at_nanos(&row.ts), format!("e{}", row.id), row.id)
}

// ---- イベント以外のブロック（対話・認可・途中目標・報告）----

/// `after_nanos` より後（同時刻は取りこぼさないため閉区間）の対話・認可・途中目標・報告。
/// `after_nanos` が `None` なら、それぞれ新しい方から `limit` 件。
pub(crate) fn side_blocks(
    store: &SqliteStore,
    scope: &Scope,
    after_nanos: Option<i128>,
    limit: usize,
    tasks: &mut TaskCache,
) -> Result<Vec<ConsoleBlock>, ApiProblem> {
    let mut blocks = Vec::new();
    let after_at = after_nanos.map(nanos_to_rfc3339);

    // 1. 対話（人の発言と返事）。案件は `messages.project_id`、ノードは `messages.node_id` で絞る。
    let messages = store
        .message_page(scope.node(), scope.project(), after_at.as_deref(), limit)
        .map_err(store_problem)?;
    for message in messages {
        blocks.push(message_block(&message));
    }

    // 2. 認可（状態ごと）。
    for approval in store
        .approval_list(None, scope.project(), scope.node())
        .map_err(store_problem)?
    {
        let at = rfc3339(approval.decided_at.unwrap_or(approval.created_at));
        let nanos = at_nanos(&at);
        if after_nanos.is_some_and(|a| nanos < a) {
            continue;
        }
        blocks.push(ConsoleBlock::Approval {
            cursor: ConsoleCursor::new(nanos, format!("a{}", approval.id), 0).encode(),
            at,
            approval,
        });
    }

    // 3. 途中目標の提案（ADR-0038）。ノードの範囲には出さない（途中目標は案件のもの）。
    if !matches!(scope, Scope::Node(_)) {
        for milestone in proposed_milestones(store, scope)? {
            let at = rfc3339(milestone.created_at);
            let nanos = at_nanos(&at);
            if after_nanos.is_some_and(|a| nanos < a) {
                continue;
            }
            let review =
                task_ops::milestone_review::review_state(store, milestone.project_id, milestone.id)
                    .map_err(store_problem)?
                    .reply
                    .map(|reply| MilestoneReviewView {
                        message_id: reply.id.to_string(),
                        text: reply.text,
                        at: rfc3339(reply.created_at),
                    });
            blocks.push(ConsoleBlock::Milestone {
                cursor: ConsoleCursor::new(nanos, format!("ms{}", milestone.id), 0).encode(),
                at,
                milestone,
                review,
            });
        }
    }

    // 4. 報告（ADR-0034）。`node_id` / `project_id` は SQL で絞る。
    let filter = ReportFilter {
        project_id: scope.project(),
        node_id: scope.node().map(str::to_string),
        limit,
        ..ReportFilter::default()
    };
    for report in store.report_list(&filter).map_err(store_problem)? {
        let at = rfc3339(report.created_at);
        let nanos = at_nanos(&at);
        if after_nanos.is_some_and(|a| nanos < a) {
            continue;
        }
        blocks.push(ConsoleBlock::Report {
            cursor: ConsoleCursor::new(nanos, format!("r{}", report.id), 0).encode(),
            at,
            report,
        });
    }

    // 5. 知識整理 run（ADR-0047 D4 / D5。Phase 62）。`knowledge_runs` に `project_id`/`node_id` の
    // 列は無いので、元のタスク（`TaskCache` 経由）で絞る。`scheduled`（まだ適用されていない）は出さない。
    for run in store.knowledge_run_recent(limit).map_err(store_problem)? {
        if run.state == KnowledgeRunState::Scheduled {
            continue;
        }
        let Some(source) = tasks.get(store, run.task_id)?.cloned() else {
            continue;
        };
        match scope {
            Scope::Project(id) if source.project_id != Some(*id) => continue,
            Scope::Node(node_id) if source.assignee.as_deref() != Some(node_id.as_str()) => {
                continue;
            }
            _ => {}
        }
        let applied_at = run.applied_at.unwrap_or(run.created_at);
        let at = rfc3339(applied_at);
        let nanos = at_nanos(&at);
        if after_nanos.is_some_and(|a| nanos < a) {
            continue;
        }
        let via = run.via.clone();
        let summary = run.summary.unwrap_or_default();
        blocks.push(ConsoleBlock::Knowledge {
            cursor: ConsoleCursor::new(nanos, format!("k{}", run.task_id), 0).encode(),
            at,
            project_id: source.project_id,
            task_id: source.id,
            task_title: source.title,
            run_task_id: run.run_task_id,
            state: match run.state {
                KnowledgeRunState::Done => "applied".to_string(),
                KnowledgeRunState::Failed => "failed".to_string(),
                KnowledgeRunState::Scheduled => unreachable!("filtered above"),
            },
            ingested: summary.ingested,
            inbox: summary.inbox,
            discarded: summary.discarded,
            // ADR-0052 D2: 列（`knowledge_runs.via`）を正とし、無ければ summary の写しを使う。
            via: via.or(summary.via),
        });
    }

    Ok(blocks)
}

/// 提案中（`proposed`）の途中目標。`all` のときは案件をなめる（案件の数は人が作る数なので小さい）。
fn proposed_milestones(
    store: &SqliteStore,
    scope: &Scope,
) -> Result<Vec<task_core::Milestone>, ApiProblem> {
    let project_ids: Vec<ProjectId> = match scope.project() {
        Some(id) => vec![id],
        None => store
            .project_list()
            .map_err(store_problem)?
            .into_iter()
            .map(|p| p.id)
            .collect(),
    };
    let mut out = Vec::new();
    for project_id in project_ids {
        for milestone in store.milestone_list(project_id).map_err(store_problem)? {
            if milestone.status == MilestoneStatus::Proposed {
                out.push(milestone);
            }
        }
    }
    Ok(out)
}

fn message_block(message: &Message) -> ConsoleBlock {
    let at = rfc3339(message.created_at);
    let cursor = ConsoleCursor::new(at_nanos(&at), format!("m{}", message.id), 0).encode();
    match message.role {
        MessageRole::User => ConsoleBlock::Human {
            at,
            cursor,
            message_id: message.id.to_string(),
            node_id: message.node_id.clone(),
            project_id: message.project_id,
            task_id: message.task_id,
            text: message.text.clone(),
        },
        MessageRole::Node => ConsoleBlock::Reply {
            at,
            cursor,
            message_id: message.id.to_string(),
            node_id: message.node_id.clone(),
            project_id: message.project_id,
            task_id: message.task_id,
            run_id: message.run_id.clone(),
            text: message.text.clone(),
            actions_result: message.metadata.clone(),
            // ADR-0054 D2（Phase 68）: `messages` に確定した返事は常に `done`（育つ途中の
            // `thinking`/`steps` はもう意味を持たない）。
            state: task_ops::console::ConsoleReplyState::Done,
            thinking: None,
            steps: Vec::new(),
        },
    }
}

/// Unix ナノ秒 → RFC 3339（SQL の `created_at >= ?` に渡す）。
fn nanos_to_rfc3339(nanos: i128) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map(rfc3339)
        .unwrap_or_default()
}

// ---- 共通 ----

/// ブロックの `cursor`（どの種でも持っている）。
pub(crate) fn cursor_of(block: &ConsoleBlock) -> &str {
    match block {
        ConsoleBlock::Human { cursor, .. }
        | ConsoleBlock::Reply { cursor, .. }
        | ConsoleBlock::Task { cursor, .. }
        | ConsoleBlock::Progress { cursor, .. }
        | ConsoleBlock::Question { cursor, .. }
        | ConsoleBlock::Approval { cursor, .. }
        | ConsoleBlock::Milestone { cursor, .. }
        | ConsoleBlock::Report { cursor, .. }
        | ConsoleBlock::Knowledge { cursor, .. } => cursor,
    }
}

/// 並びのキー（時刻、同時刻は `tie`）。`GET /tasks/{id}/timeline` と同じく**時刻として**比べる
/// （RFC 3339 を文字列で比べると小数秒で順が狂う。Phase 53 の監査）。
pub(crate) fn block_key(block: &ConsoleBlock) -> (i128, String) {
    match ConsoleCursor::decode(cursor_of(block)) {
        Some(cursor) => (cursor.at_nanos, cursor.tie),
        None => (0, String::new()),
    }
}

/// `TaskId` → `Task` の引き当てを 1 回で済ませる（窓の中のタスクの数だけ引く）。
#[derive(Default)]
pub(crate) struct TaskCache {
    by_id: HashMap<TaskId, Option<Task>>,
}

impl TaskCache {
    pub(crate) fn get(
        &mut self,
        store: &SqliteStore,
        id: TaskId,
    ) -> Result<Option<&Task>, ApiProblem> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.by_id.entry(id) {
            let task = store.get(id).map_err(store_problem)?;
            slot.insert(task);
        }
        Ok(self.by_id.get(&id).and_then(|t| t.as_ref()))
    }
}

// ---- GET /tasks/{id}/runs/{run_id}/events ----

/// `GET /tasks/{id}/runs/{run_id}/events`（ADR-0048 D2: 折り畳んだ `progress` を開いたときの全行）。
/// その run に紐づくイベントだけを `seq` 昇順で返す（`GET /tasks/{id}/events` の絞り込み版）。
pub(crate) async fn run_events(
    State(state): State<ApiState>,
    crate::handlers::Params((id, run_id)): crate::handlers::Params<(String, String)>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["after_seq", "limit"])?;
    let task_id = parse_task_id(&id)?;
    let after_seq = query.u64("after_seq")?;
    let limit = query.limit("limit", 500, 5_000)?;
    let page = state
        .blocking(move |store| {
            if store.get(task_id).map_err(store_problem)?.is_none() {
                return Err(ApiProblem::task_not_found(task_id));
            }
            let rows = store
                .event_rows_for(task_id, after_seq, RUN_EVENTS_SCAN)
                .map_err(store_problem)?;
            let mut items: Vec<EventRow> = rows
                .into_iter()
                .filter(|row| run_of(&row.event) == Some(run_id.as_str()))
                .collect();
            let has_more = items.len() > limit;
            items.truncate(limit);
            Ok(crate::types::EventsPage { items, has_more })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &page))
}

/// `run_events` が 1 回になめるイベントの上限（タスクの全イベントは `GET /tasks/{id}/events`）。
const RUN_EVENTS_SCAN: usize = 20_000;

/// そのイベントが属する run（run に紐づかないイベントは `None`）。
fn run_of(event: &Event) -> Option<&str> {
    match event {
        Event::WorkerStarted { run_id, .. }
        | Event::WorkerProgress { run_id, .. }
        | Event::ArtifactProduced { run_id, .. }
        | Event::WorkerFinished { run_id, .. }
        | Event::ReviewVerdict { run_id, .. }
        | Event::QuestionRaised { run_id, .. }
        | Event::Delegated { run_id, .. } => Some(run_id.as_str()),
        _ => None,
    }
}

/// 案件の全タスクを引く（`Scope::Project` の範囲検査を SQL 側でやりたくなったときの入口。
/// いまは `Scope::covers` が `Task.project_id` を見るので使っていない）。
#[allow(dead_code)]
fn project_task_ids(
    store: &SqliteStore,
    project_id: ProjectId,
    limit: usize,
) -> Result<Vec<TaskId>, ApiProblem> {
    let filter = ListFilter {
        project_id: Some(project_id),
        ..ListFilter::default()
    };
    Ok(store
        .list_page(&filter, ListOrder::CreatedDesc, None, limit)
        .map_err(store_problem)?
        .items
        .into_iter()
        .map(|t| t.id)
        .collect())
}

// ---- GET /console/stream（SSE）----

/// `progress` の更新をまとめる最小間隔（ADR-0048 D1: run ごとに 1 秒に 1 回まで）。
pub const PROGRESS_COALESCE: std::time::Duration = std::time::Duration::from_secs(1);
/// イベント以外（対話・認可・途中目標・報告）を見に行く間隔。
pub const SIDE_POLL: std::time::Duration = std::time::Duration::from_secs(1);
/// 送った「イベント以外」のカーソルを覚えておく上限（同じものを 2 回流さないため）。
const SENT_SIDE_MAX: usize = 2_000;

/// `GET /console/stream` の最初のフレーム（`event: hello`）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ConsoleHello {
    /// いまの位置（`GET /console?since=` に渡せる）。
    pub cursor: String,
    /// `all` / `project:<id>` / `node:<id>`。
    pub scope: String,
    /// RFC 3339。
    pub now: String,
}

/// `GET /console/stream?scope=…`（ADR-0048 D1）。認証・接続数の上限は `GET /stream` と同じ。
pub(crate) async fn stream(
    State(state): State<ApiState>,
    RawQuery(raw): RawQuery,
) -> Result<axum::response::Response, ApiProblem> {
    let query = QueryParams::parse(raw.as_deref(), &["scope", "since"])?;
    let scope = Scope::parse(query.single("scope")?)?;
    let since = parse_since(query.single("since")?)?;
    let slot = state
        .try_open_stream()
        .ok_or_else(ApiProblem::too_many_streams)?;

    // 位置を決める: `since` があればそこから、無ければ「今」（履歴は `GET /console` で取る）。
    let cursor = match since {
        Some(cursor) => cursor,
        None => {
            let latest = state
                .blocking(|store| store.latest_event_id().map_err(store_problem))
                .await?;
            ConsoleCursor::new(now_nanos(), String::new(), latest)
        }
    };
    let hello = ConsoleHello {
        cursor: cursor.encode(),
        scope: scope_label(&scope),
        now: crate::handlers::now_rfc3339(),
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(crate::sse::CHANNEL_CAPACITY);
    if let Some(bytes) = crate::sse::frame("hello", None, &hello)
        && tx.try_send(bytes).is_err()
    {
        return Err(ApiProblem::internal("stream buffer is unavailable"));
    }
    tokio::spawn(run_stream(state, slot, tx, scope, cursor));

    let body = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|bytes| (Ok::<axum::body::Bytes, std::convert::Infallible>(bytes), rx))
    });
    let mut response = axum::response::Response::new(axum::body::Body::from_stream(body));
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        crate::sse::X_ACCEL_BUFFERING,
        axum::http::HeaderValue::from_static("no"),
    );
    Ok(response)
}

fn scope_label(scope: &Scope) -> String {
    match scope {
        Scope::All => "all".to_string(),
        Scope::Project(id) => format!("project:{id}"),
        Scope::Node(id) => format!("node:{id}"),
    }
}

fn now_nanos() -> i128 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos()
}

/// 購読ループ（`crate::sse::run` と同じ骨格: ポーリング + heartbeat + 切断・停止で終わる）。
async fn run_stream(
    state: ApiState,
    _slot: crate::state::StreamSlot,
    tx: tokio::sync::mpsc::Sender<axum::body::Bytes>,
    scope: Scope,
    mut cursor: ConsoleCursor,
) {
    let tuning = state.tuning;
    let mut shutdown = state.inner.shutdown.subscribe();
    let mut poll = tokio::time::interval(tuning.poll_interval);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + tuning.heartbeat_interval,
        tuning.heartbeat_interval,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut pending = PendingProgress::default();
    let mut sent_side: HashSet<String> = HashSet::new();
    let mut last_side = tokio::time::Instant::now() - SIDE_POLL;

    loop {
        let keep_going = tokio::select! {
            biased;
            _ = tx.closed() => false,
            _ = crate::sse::closed(&mut shutdown) => false,
            _ = heartbeat.tick() => {
                let beat = crate::types::StreamHeartbeat { now: crate::handlers::now_rfc3339() };
                crate::sse::send(&state, &tx, crate::sse::frame("heartbeat", None, &beat)).await
            }
            _ = poll.tick() => {
                poll_console(&state, &tx, &scope, &mut cursor, &mut pending, &mut sent_side, &mut last_side).await
            }
        };
        if !keep_going {
            break;
        }
    }
}

/// run ごとに溜めている `progress`（1 秒に 1 回まで送る）。
#[derive(Default)]
struct PendingProgress {
    blocks: HashMap<String, ConsoleBlock>,
    last_emit: HashMap<String, tokio::time::Instant>,
}

/// 1 回のポーリング。送信路が閉じたら `false`。DB のエラーは次の tick で拾い直す。
#[allow(clippy::too_many_arguments)]
async fn poll_console(
    state: &ApiState,
    tx: &tokio::sync::mpsc::Sender<axum::body::Bytes>,
    scope: &Scope,
    cursor: &mut ConsoleCursor,
    pending: &mut PendingProgress,
    sent_side: &mut HashSet<String>,
    last_side: &mut tokio::time::Instant,
) -> bool {
    let want_side = last_side.elapsed() >= SIDE_POLL;
    let after_id = cursor.event_id;
    let after_nanos = cursor.at_nanos;
    let scope_for_task = scope.clone();
    let fetched = state
        .blocking(move |store| {
            let mut cache = TaskCache::default();
            let rows = store
                .events_since(after_id, EVENT_WINDOW)
                .map_err(store_problem)?;
            let watermark = rows.last().map(|r| r.id).unwrap_or(after_id);
            let events = event_blocks(store, &scope_for_task, &rows, &mut cache)?;
            let side = if want_side {
                side_blocks(
                    store,
                    &scope_for_task,
                    Some(after_nanos),
                    DEFAULT_LIMIT,
                    &mut cache,
                )?
            } else {
                Vec::new()
            };
            Ok((events, side, watermark))
        })
        .await;
    let (events, side, watermark) = match fetched {
        Ok(v) => v,
        Err(problem) => {
            tracing::warn!(
                code = problem.code(),
                "console SSE poll failed; retrying on the next tick"
            );
            return true;
        }
    };
    cursor.event_id = watermark;
    if want_side {
        *last_side = tokio::time::Instant::now();
    }

    // イベント由来: `progress` は run ごとに溜め、それ以外はそのまま流す。
    let mut immediate: Vec<ConsoleBlock> = Vec::new();
    for block in events {
        match &block {
            ConsoleBlock::Progress { progress, .. } => {
                let key = task_ops::console::progress_tie(progress.task_id, &progress.run_id);
                match pending.blocks.get_mut(&key) {
                    Some(ConsoleBlock::Progress {
                        progress: acc,
                        cursor,
                        ..
                    }) => {
                        task_ops::console::merge_progress(acc, progress);
                        if let ConsoleBlock::Progress { cursor: next, .. } = &block {
                            *cursor = next.clone();
                        }
                    }
                    _ => {
                        pending.blocks.insert(key, block);
                    }
                }
            }
            // ADR-0054 D2（Phase 68）: 育つ返事（`state = streaming`）も `progress` と同じく run ごとに
            // 溜める（1 秒に 1 回まで流す）。`messages` から作った確定済みの `reply`（`side` 経由）は
            // ここを通らない。
            ConsoleBlock::Reply {
                task_id: Some(task_id),
                run_id: Some(run_id),
                state: task_ops::console::ConsoleReplyState::Streaming,
                ..
            } => {
                let key = task_ops::console::progress_tie(*task_id, run_id);
                match pending.blocks.get_mut(&key) {
                    Some(ConsoleBlock::Reply {
                        text: acc_text,
                        thinking: acc_thinking,
                        steps: acc_steps,
                        cursor: acc_cursor,
                        ..
                    }) => {
                        if let ConsoleBlock::Reply {
                            text,
                            thinking,
                            steps,
                            cursor: next_cursor,
                            ..
                        } = &block
                        {
                            acc_text.push_str(text);
                            if thinking.is_some() {
                                *acc_thinking = thinking.clone();
                            }
                            acc_steps.extend(steps.iter().cloned());
                            *acc_cursor = next_cursor.clone();
                        }
                    }
                    _ => {
                        pending.blocks.insert(key, block);
                    }
                }
            }
            _ => immediate.push(block),
        }
    }
    // 「イベント以外」は同じものを 2 回流さない（`after` は同時刻を取りこぼさない閉区間なので重なる）。
    for block in side {
        let key = cursor_of(&block).to_string();
        if sent_side.contains(&key) {
            continue;
        }
        if sent_side.len() >= SENT_SIDE_MAX {
            sent_side.clear();
        }
        sent_side.insert(key);
        immediate.push(block);
    }

    immediate.sort_by_key(block_key);
    for block in &immediate {
        if !send_block(state, tx, block).await {
            return false;
        }
        cursor.at_nanos = cursor.at_nanos.max(block_key(block).0);
    }

    // 溜めた `progress` のうち、前に送ってから 1 秒たったものを出す。
    let due: Vec<String> = pending
        .blocks
        .keys()
        .filter(|key| {
            pending
                .last_emit
                .get(*key)
                .is_none_or(|last| last.elapsed() >= PROGRESS_COALESCE)
        })
        .cloned()
        .collect();
    for key in due {
        let Some(block) = pending.blocks.remove(&key) else {
            continue;
        };
        if !send_block(state, tx, &block).await {
            return false;
        }
        pending.last_emit.insert(key, tokio::time::Instant::now());
    }
    true
}

async fn send_block(
    state: &ApiState,
    tx: &tokio::sync::mpsc::Sender<axum::body::Bytes>,
    block: &ConsoleBlock,
) -> bool {
    crate::sse::send(state, tx, crate::sse::frame("console.block", None, block)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `scope` の 3 種と、形の違うものは 400。
    #[test]
    fn scopes_parse_all_project_and_node() {
        assert_eq!(Scope::parse(None).ok(), Some(Scope::All));
        assert_eq!(Scope::parse(Some("all")).ok(), Some(Scope::All));
        assert_eq!(
            Scope::parse(Some("node:secretary")).ok(),
            Some(Scope::Node("secretary".into()))
        );
        let id = ProjectId::new();
        assert_eq!(
            Scope::parse(Some(&format!("project:{id}"))).ok(),
            Some(Scope::Project(id))
        );
        assert!(Scope::parse(Some("project:nope")).is_err());
        assert!(Scope::parse(Some("node:")).is_err());
        assert!(Scope::parse(Some("bogus")).is_err());
    }

    /// カーソルの往復と、形の違うものは 400。
    #[test]
    fn since_is_an_opaque_cursor() {
        assert!(parse_since(None).ok().flatten().is_none());
        assert!(parse_since(Some("")).ok().flatten().is_none());
        let cursor = ConsoleCursor::new(42, "e7", 7);
        assert_eq!(
            parse_since(Some(&cursor.encode())).ok().flatten(),
            Some(cursor)
        );
        assert!(parse_since(Some("nope")).is_err());
    }
}
