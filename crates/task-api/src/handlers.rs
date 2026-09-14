//! ルーティングとハンドラ（`docs/gui/api.md` §2〜§3）。HTTP の写像だけを行い、判断は task-ops / ストアに任せる。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{FromRequestParts, Path, RawQuery, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use task_core::{EventRow, ListFilter, ListOrder, SqliteStore, Status, StoreError, Task, TaskId, TaskKind, TaskStore};
use task_ops::OpsError;
use task_ops::add::NewTaskSpec;
use task_ops::plan::NewPlanSpec;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::files::{self, FileRequest, FileTarget, RunFile};
use crate::problem::{ApiProblem, ops_problem, store_problem};
use crate::query::{QueryParams, event_type_name, parse_snake, parse_task_id};
use crate::schema::API_V1_SCHEMA_JSON;
use crate::state::ApiState;
use crate::types::{
    AnswerBody, ArtifactList, CancelBody, DaemonView, DbInfo, DecisionBody, EventsPage, Health, ProviderView, Providers,
    RunList, ValidationError,
};
use crate::{API_VERSION, MAX_BODY_BYTES};

type ApiResult = Result<Response, ApiProblem>;

const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";
const TITLE_QUERY_MAX_CHARS: usize = 200;

pub(crate) fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/inbox", get(inbox))
        .route("/api/v1/tasks", get(list_tasks).post(create_task))
        .route("/api/v1/tasks/{id}", get(task_detail))
        .route("/api/v1/tasks/{id}/events", get(task_events))
        .route("/api/v1/tasks/{id}/runs", get(task_runs))
        .route("/api/v1/tasks/{id}/runs/{run_id}/stdout", get(run_stdout))
        .route("/api/v1/tasks/{id}/runs/{run_id}/stderr", get(run_stderr))
        .route("/api/v1/tasks/{id}/runs/{run_id}/result", get(run_result))
        .route("/api/v1/tasks/{id}/artifacts", get(artifact_list))
        .route("/api/v1/tasks/{id}/artifacts/{idx}", get(artifact_body))
        .route("/api/v1/tasks/{id}/approve", post(approve))
        .route("/api/v1/tasks/{id}/reject", post(reject))
        .route("/api/v1/tasks/{id}/answer", post(answer))
        .route("/api/v1/tasks/{id}/cancel", post(cancel))
        .route("/api/v1/plans", post(create_plan))
        .route("/api/v1/replay", post(replay))
        .route("/api/v1/graph", get(graph))
        .route("/api/v1/events", get(events))
        .route("/api/v1/stream", get(crate::sse::stream))
        .route("/api/v1/providers", get(providers))
        .route("/api/v1/daemon", get(daemon))
        .route("/api/v1/config", get(config))
        .route("/api/v1/schema", get(schema))
        .fallback(fallback)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::guard,
        ))
        .with_state(state)
}

// ---- 共通 ----

pub(crate) fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| t.to_string())
}

pub(crate) fn now_rfc3339() -> String {
    rfc3339(OffsetDateTime::now_utc())
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(body) => {
            let mut response = Response::new(Body::from(body));
            *response.status_mut() = status;
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON_CONTENT_TYPE));
            response
        }
        Err(e) => ApiProblem::internal(format!("failed to serialize the response: {e}")).into_response(),
    }
}

fn created_task(task: &Task) -> Response {
    let mut response = json_response(StatusCode::CREATED, task);
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/tasks/{}", task.id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    response
}

/// path の値。解析の失敗は 400 `bad_request`（axum の既定の text 応答にしない）。
pub(crate) struct Params<T>(pub(crate) T);

impl<S, T> FromRequestParts<S> for Params<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiProblem;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Params(value)),
            Err(rejection) => Err(ApiProblem::bad_request(rejection.body_text())),
        }
    }
}

/// 本文を 1 MiB まで読む（超えたら 413）。
async fn read_body(body: Body) -> Result<Vec<u8>, ApiProblem> {
    let mut stream = body.into_data_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiProblem::bad_request(format!("failed to read the request body: {e}")))?;
        if buf.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(ApiProblem::payload_too_large());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// JSON 本文を解析する。構文誤り・未知フィールド・型誤りは 400。`empty_is_object` なら空本体を `{}` とみなす。
async fn read_json<T: DeserializeOwned>(body: Body, empty_is_object: bool) -> Result<T, ApiProblem> {
    let bytes = read_body(body).await?;
    let text: &[u8] = if empty_is_object && bytes.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        &bytes
    };
    serde_json::from_slice(text).map_err(|e| ApiProblem::bad_request(format!("invalid JSON body: {e}")))
}

fn load_task(store: &SqliteStore, id: TaskId) -> Result<Task, ApiProblem> {
    store
        .get(id)
        .map_err(store_problem)?
        .ok_or_else(|| ApiProblem::task_not_found(id))
}

fn no_query(raw: &Option<String>) -> Result<(), ApiProblem> {
    QueryParams::parse(raw.as_deref(), &[]).map(|_| ())
}

async fn fallback() -> ApiProblem {
    ApiProblem::not_found()
}

async fn method_not_allowed() -> ApiProblem {
    ApiProblem::method_not_allowed()
}

// ---- 1. GET /health ----

async fn health(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let schema_version = state
        .blocking(|store| store.schema_version().map_err(store_problem))
        .await?;
    let inner = &state.inner;
    Ok(json_response(
        StatusCode::OK,
        &Health {
            api_version: API_VERSION.to_string(),
            schema_version,
            taskd_version: inner.taskd_version.clone(),
            instance_id: inner.instance_id.clone(),
            started_at: inner.started_at.clone(),
            now: now_rfc3339(),
            db: DbInfo {
                journal_mode: inner.journal_mode.clone(),
                busy_timeout_ms: inner.busy_timeout_ms,
            },
        },
    ))
}

// ---- 2. GET /inbox ----

async fn inbox(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let snapshot = state.snapshot();
    let ctx = state.inner.view.clone();
    let inbox = state
        .blocking(move |store| {
            let root = ctx.workspace_root.clone();
            let mut inbox = task_ops::inbox::inbox(
                store,
                snapshot.as_ref(),
                &ctx,
                OffsetDateTime::now_utc(),
                &|task: &Task, run_id: &str| files::read_evidence(task, &root, run_id),
            )
            .map_err(|e| ops_problem(store, e, None))?;
            for item in &mut inbox.approvals {
                let (Some(parent), Some(run)) = (item.parent.as_ref(), item.last_run.as_mut()) else {
                    continue;
                };
                if run.files.is_none()
                    && let Some(task) = store.get(parent.id).map_err(store_problem)?
                {
                    run.files = Some(files::run_files(&task, &root, &run.run_id));
                }
            }
            Ok(inbox)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &inbox))
}

// ---- 3. GET /tasks ----

async fn list_tasks(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = QueryParams::parse(
        raw.as_deref(),
        &["status", "kind", "parent", "root_only", "q", "order", "limit", "cursor"],
    )?;
    let mut filter = ListFilter::default();
    for status in query.list("status") {
        filter.statuses.push(parse_snake::<Status>("status", status)?);
    }
    for kind in query.list("kind") {
        filter.kinds.push(parse_snake::<TaskKind>("kind", kind)?);
    }
    filter.parent_id = query.task_id("parent")?;
    filter.root_only = query.bool("root_only")?.unwrap_or(false);
    if let Some(text) = query.single("q")? {
        if text.chars().count() > TITLE_QUERY_MAX_CHARS {
            return Err(ApiProblem::bad_request("query parameter `q` must be at most 200 characters"));
        }
        if !text.is_empty() {
            filter.title_contains = Some(text.to_string());
        }
    }
    let order = match query.single("order")? {
        None | Some("updated_desc") => ListOrder::UpdatedDesc,
        Some("dispatch") => ListOrder::Dispatch,
        Some("created_desc") => ListOrder::CreatedDesc,
        Some(other) => return Err(ApiProblem::bad_request(format!("unknown order `{other}`"))),
    };
    let limit = query.limit("limit", 100, 500)?;
    let cursor = query.single("cursor")?.filter(|c| !c.is_empty()).map(str::to_string);
    let ctx = state.inner.view.clone();
    let list = state
        .blocking(move |store| {
            task_ops::view::task_list(
                store,
                &filter,
                order,
                cursor.as_deref(),
                limit,
                &ctx,
                OffsetDateTime::now_utc(),
            )
            .map_err(|e| match e {
                OpsError::Store(StoreError::Invalid(message)) if message.starts_with("invalid cursor") => {
                    ApiProblem::bad_request("invalid cursor")
                }
                other => ops_problem(store, other, None),
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &list))
}

// ---- 4. POST /tasks ----

async fn create_task(State(state): State<ApiState>, RawQuery(raw): RawQuery, body: Body) -> ApiResult {
    no_query(&raw)?;
    let spec: NewTaskSpec = read_json(body, false).await?;
    let task = state
        .blocking(move |store| {
            task_ops::add::create_task(store, spec, OffsetDateTime::now_utc()).map_err(|e| ops_problem(store, e, None))
        })
        .await?;
    Ok(created_task(&task))
}

// ---- 5. GET /tasks/{id} ----

async fn task_detail(State(state): State<ApiState>, Params(id): Params<String>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let ctx = state.inner.view.clone();
    let detail = state
        .blocking(move |store| {
            let mut detail = task_ops::view::task_detail(store, id, &ctx, OffsetDateTime::now_utc())
                .map_err(|e| ops_problem(store, e, None))?;
            let task = &detail.task;
            for run in &mut detail.runs {
                run.files = Some(files::run_files(task, &ctx.workspace_root, &run.run_id));
            }
            Ok(detail)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &detail))
}

// ---- 6. GET /tasks/{id}/events, 20. GET /events ----

/// `fetch(after, batch)` で読み進め、`keep` に合う行を `limit` 件まで集める。`key` は次の `after` にする値。
fn collect_events(
    limit: usize,
    filtered: bool,
    keep: impl Fn(&EventRow) -> bool,
    key: fn(&EventRow) -> u64,
    initial_after: Option<u64>,
    mut fetch: impl FnMut(Option<u64>, usize) -> Result<Vec<EventRow>, StoreError>,
) -> Result<EventsPage, StoreError> {
    let batch = if filtered {
        limit.saturating_add(1).max(1_000)
    } else {
        limit.saturating_add(1)
    };
    let mut items = Vec::new();
    let mut after = initial_after;
    loop {
        let rows = fetch(after, batch)?;
        let exhausted = rows.len() < batch;
        for row in rows {
            after = Some(key(&row));
            if keep(&row) {
                if items.len() == limit {
                    return Ok(EventsPage { items, has_more: true });
                }
                items.push(row);
            }
        }
        if exhausted {
            return Ok(EventsPage { items, has_more: false });
        }
    }
}

async fn task_events(State(state): State<ApiState>, Params(id): Params<String>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["after_seq", "limit", "types"])?;
    let id = parse_task_id(&id)?;
    let after_seq = match query.i64("after_seq")? {
        None | Some(-1) => None,
        Some(n) if n >= 0 => Some(n.unsigned_abs()),
        Some(_) => return Err(ApiProblem::bad_request("query parameter `after_seq` must be -1 or greater")),
    };
    let limit = query.limit("limit", 500, 5_000)?;
    let types = query.event_types()?;
    let page = state
        .blocking(move |store| {
            load_task(store, id)?;
            collect_events(
                limit,
                types.is_some(),
                |row| types.as_ref().is_none_or(|t| t.contains(event_type_name(&row.event))),
                |row| row.seq,
                after_seq,
                |after, batch| store.event_rows_for(id, after, batch),
            )
            .map_err(store_problem)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &page))
}

async fn events(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["after_id", "limit", "task_id", "types"])?;
    let after_id = query.u64("after_id")?.unwrap_or(0);
    let limit = query.limit("limit", 500, 5_000)?;
    let task_id = query.task_id("task_id")?;
    let types = query.event_types()?;
    let page = state
        .blocking(move |store| {
            let type_ok = |row: &EventRow| types.as_ref().is_none_or(|t| t.contains(event_type_name(&row.event)));
            match task_id {
                Some(task_id) => collect_events(
                    limit,
                    true,
                    |row| row.id > after_id && type_ok(row),
                    |row| row.seq,
                    None,
                    |after, batch| store.event_rows_for(task_id, after, batch),
                ),
                None => collect_events(
                    limit,
                    types.is_some(),
                    type_ok,
                    |row| row.id,
                    Some(after_id),
                    |after, batch| store.events_since(after.unwrap_or(after_id), batch),
                ),
            }
            .map_err(store_problem)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &page))
}

// ---- 7. GET /tasks/{id}/runs ----

async fn task_runs(State(state): State<ApiState>, Params(id): Params<String>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let root = state.inner.view.workspace_root.clone();
    let list = state
        .blocking(move |store| {
            let task = load_task(store, id)?;
            let rows = store.event_rows_for(id, None, usize::MAX).map_err(store_problem)?;
            let mut runs = task_ops::view::runs(&rows);
            for run in &mut runs {
                run.files = Some(files::run_files(&task, &root, &run.run_id));
            }
            Ok(RunList { runs })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &list))
}

// ---- 8〜10. GET /tasks/{id}/runs/{run_id}/{stdout|stderr|result} ----

async fn run_stdout(
    State(state): State<ApiState>,
    Params(params): Params<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> ApiResult {
    run_file(state, params, raw, headers, RunFile::Stdout).await
}

async fn run_stderr(
    State(state): State<ApiState>,
    Params(params): Params<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> ApiResult {
    run_file(state, params, raw, headers, RunFile::Stderr).await
}

async fn run_result(
    State(state): State<ApiState>,
    Params(params): Params<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> ApiResult {
    run_file(state, params, raw, headers, RunFile::Result).await
}

async fn run_file(
    state: ApiState,
    (id, run_id): (String, String),
    raw: Option<String>,
    headers: HeaderMap,
    file: RunFile,
) -> ApiResult {
    let request = FileRequest::parse(raw.as_deref(), &headers)?;
    let id = parse_task_id(&id)?;
    let root = state.inner.view.workspace_root.clone();
    let target = state
        .blocking(move |store| {
            let task = load_task(store, id)?;
            let path = files::resolve_run_file(&task, &root, &run_id, file)?;
            let size = files::file_size(&path)?;
            Ok(FileTarget {
                path,
                size,
                recorded_sha256: None,
                current_sha256: None,
            })
        })
        .await?;
    files::respond_file(target, &request).await
}

// ---- 11. GET /tasks/{id}/artifacts, 12. GET /tasks/{id}/artifacts/{idx} ----

async fn artifact_list(State(state): State<ApiState>, Params(id): Params<String>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let root = state.inner.view.workspace_root.clone();
    let list = state
        .blocking(move |store| {
            let task = load_task(store, id)?;
            let rows = store.event_rows_for(id, None, usize::MAX).map_err(store_problem)?;
            Ok(ArtifactList {
                items: files::artifact_views(&task, &root, &rows),
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &list))
}

async fn artifact_body(
    State(state): State<ApiState>,
    Params((id, idx)): Params<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> ApiResult {
    let request = FileRequest::parse(raw.as_deref(), &headers)?;
    let id = parse_task_id(&id)?;
    let idx: usize = idx
        .parse()
        .map_err(|_| ApiProblem::bad_request("artifact index must be a non-negative integer"))?;
    let root = state.inner.view.workspace_root.clone();
    let target = state
        .blocking(move |store| {
            let task = load_task(store, id)?;
            let rows = store.event_rows_for(id, None, usize::MAX).map_err(store_problem)?;
            let artifact = files::nth_artifact(&rows, idx).ok_or_else(|| ApiProblem::artifact_not_found(idx))?;
            let ws = files::canonical_workspace(&task, &root)?;
            let path = files::resolve_artifact(&ws, &artifact.path)?;
            let size = files::file_size(&path)?;
            let current_sha256 = files::current_sha256(&path, size);
            Ok(FileTarget {
                path,
                size,
                recorded_sha256: Some(artifact.sha256.clone()),
                current_sha256,
            })
        })
        .await?;
    files::respond_file(target, &request).await
}

// ---- 13〜16. POST /tasks/{id}/{approve|reject|answer|cancel} ----

async fn approve(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let DecisionBody { note, expected_status } = read_json(body, true).await?;
    let result = state
        .blocking(move |store| {
            task_ops::gate::approve(store, id, note, expected_status).map_err(|e| ops_problem(store, e, Some("approve")))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

async fn reject(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let DecisionBody { note, expected_status } = read_json(body, true).await?;
    let result = state
        .blocking(move |store| {
            task_ops::gate::reject(store, id, note, expected_status).map_err(|e| ops_problem(store, e, Some("reject")))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

async fn answer(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let AnswerBody { answer, expected_status } = read_json(body, false).await?;
    if answer.trim().is_empty() {
        return Err(ApiProblem::validation(vec![ValidationError {
            field: Some("answer".to_string()),
            message: "answer must not be blank".to_string(),
        }]));
    }
    let result = state
        .blocking(move |store| {
            task_ops::gate::answer(store, id, answer, expected_status).map_err(|e| ops_problem(store, e, Some("answer")))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

async fn cancel(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let CancelBody { expected_status } = read_json(body, true).await?;
    let result = state
        .blocking(move |store| {
            task_ops::gate::cancel(store, id, expected_status).map_err(|e| ops_problem(store, e, Some("cancel")))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

// ---- 17. POST /plans ----

async fn create_plan(State(state): State<ApiState>, RawQuery(raw): RawQuery, body: Body) -> ApiResult {
    no_query(&raw)?;
    let spec: NewPlanSpec = read_json(body, false).await?;
    let task = state
        .blocking(move |store| {
            task_ops::plan::create_plan(store, spec, OffsetDateTime::now_utc()).map_err(|e| ops_problem(store, e, None))
        })
        .await?;
    Ok(created_task(&task))
}

// ---- 18. POST /replay ----

/// `POST /replay` の本文は `{}`（空本体も可）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayBody {}

async fn replay(State(state): State<ApiState>, RawQuery(raw): RawQuery, body: Body) -> ApiResult {
    no_query(&raw)?;
    let ReplayBody {} = read_json(body, true).await?;
    let guard = state.try_begin_replay().ok_or_else(ApiProblem::replay_in_progress)?;
    let report = state
        .blocking(move |store| {
            // 要求が切断されても replay が終わるまで枠を持つ。
            let _guard = guard;
            task_ops::replay::replay(store).map_err(|e| ops_problem(store, e, None))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &report))
}

// ---- 19. GET /graph ----

async fn graph(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["root", "depth", "include_terminal"])?;
    let root = query.task_id("root")?;
    let depth = query
        .u64("depth")?
        .map(|d| u32::try_from(d).map_err(|_| ApiProblem::bad_request("query parameter `depth` is too large")))
        .transpose()?;
    let include_terminal = query.bool("include_terminal")?.unwrap_or(true);
    let graph = state
        .blocking(move |store| {
            task_ops::graph::graph(store, root, depth, include_terminal).map_err(|e| ops_problem(store, e, None))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &graph))
}

// ---- 22. GET /providers ----

async fn providers(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let ids: Vec<String> = state
        .inner
        .config_view
        .providers
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let inner = Arc::clone(&state.inner);
    let today = OffsetDateTime::now_utc().date();
    let stats = state
        .blocking(move |store| {
            let mut guard = inner.stats.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.catch_up(store).map_err(store_problem)?;
            Ok(ids.iter().map(|id| guard.view(id, today)).collect::<Vec<_>>())
        })
        .await?;
    let snapshot = state.snapshot();
    let items = state
        .inner
        .config_view
        .providers
        .iter()
        .zip(stats)
        .map(|(provider, stats)| ProviderView {
            id: provider.id.clone(),
            adapter: provider.adapter.clone(),
            tiers: provider.tiers.clone(),
            concurrency: provider.concurrency,
            model: provider.model.clone(),
            env_keys: provider.env_keys.clone(),
            in_use: snapshot
                .as_ref()
                .and_then(|s| s.providers.iter().find(|live| live.id == provider.id))
                .map(|live| live.in_use),
            cooldown: snapshot
                .as_ref()
                .and_then(|s| s.cooldowns.iter().find(|c| c.provider == provider.id))
                .cloned(),
            stats,
        })
        .collect();
    Ok(json_response(StatusCode::OK, &Providers { items }))
}

// ---- 23. GET /daemon, 24. GET /config, 25. GET /schema ----

async fn daemon(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    Ok(json_response(
        StatusCode::OK,
        &DaemonView {
            now: now_rfc3339(),
            snapshot: state.snapshot(),
        },
    ))
}

async fn config(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    Ok(json_response(StatusCode::OK, &state.inner.config_view))
}

async fn schema(RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let mut response = Response::new(Body::from(API_V1_SCHEMA_JSON));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/schema+json"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use axum::http::Request;
    use task_ops::view::ViewContext;
    use tower::ServiceExt;

    use super::*;
    use crate::types::{ApiConfigView, ConfigView, ReviewerConfigView};
    use crate::{ApiSettings, ApiState};

    fn state(dir: &std::path::Path) -> ApiState {
        let settings = ApiSettings {
            listen: "127.0.0.1:7710".parse().unwrap_or_else(|e| panic!("{e}")),
            token: None,
            allowed_hosts: vec![],
            db_path: dir.join("taskd.db"),
            busy_timeout: Duration::from_millis(5000),
            view: ViewContext {
                workspace_root: dir.join("ws"),
                retry_backoff_base: Duration::from_secs(0),
                retry_backoff_max: Duration::from_secs(0),
                max_requeues: 5,
            },
            config_view: ConfigView {
                config_path: String::new(),
                db: String::new(),
                workspace_root: String::new(),
                tick_ms: 2000,
                max_concurrency: 1,
                lease_grace_secs: 0,
                idle_timeout_secs: 0,
                kill_grace_secs: 0,
                review_timeout_secs: 0,
                error_cooldown_secs: 0,
                retry_backoff_base_secs: 0,
                retry_backoff_max_secs: 0,
                max_requeues: 5,
                plan_auto_accept: false,
                reviewer: ReviewerConfigView {
                    adapter: None,
                    tier: task_core::Tier::Standard,
                },
                providers: vec![],
                api: ApiConfigView {
                    bind: "127.0.0.1:7710".into(),
                    auth_required: false,
                    allowed_hosts: vec![],
                },
            },
            taskd_version: "test".into(),
            instance_id: "01J00000000000000000000000".into(),
            started_at: "2026-09-14T00:00:00Z".into(),
        };
        let (_tx, rx) = tokio::sync::watch::channel(None);
        ApiState::new(settings, rx).unwrap_or_else(|e| panic!("{e}"))
    }

    fn replay_request() -> Request<Body> {
        Request::post("/api/v1/replay")
            .header("host", "127.0.0.1:7710")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[tokio::test]
    async fn second_concurrent_replay_is_rejected_with_503() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let state = state(dir.path());
        let app = router(state.clone());

        let guard = state.try_begin_replay();
        assert!(guard.is_some());
        let busy = app.clone().oneshot(replay_request()).await.unwrap_or_else(|e| match e {});
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(busy.headers().get("retry-after").and_then(|v| v.to_str().ok()), Some("5"));
        let body = axum::body::to_bytes(busy.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(problem["code"], "replay_in_progress");

        drop(guard);
        let ok = app.oneshot(replay_request()).await.unwrap_or_else(|e| match e {});
        assert_eq!(ok.status(), StatusCode::OK);
        let _ = PathBuf::new();
    }
}
