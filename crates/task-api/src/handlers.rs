//! ルーティングとハンドラ（`docs/gui/api.md` §2〜§3）。HTTP の写像だけを行い、判断は task-ops / ストアに任せる。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{FromRequestParts, Path, RawQuery, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use task_core::{
    EventRow, ListFilter, ListOrder, Milestone, MilestoneId, MilestoneStatus, OrgNode, Project, ProjectId,
    ProjectStatus, SqliteStore, Status, StoreError, Task, TaskId, TaskKind, TaskStore,
};
use task_ops::OpsError;
use task_ops::add::NewTaskSpec;
use task_ops::plan::NewPlanSpec;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::admin::{
    AccountAdminError, AdminRequest, ClusterAdminError, ProviderCreateBody, ProviderPatchBody, read_provider_file,
    valid_adapter, valid_provider_id, write_provider_file,
};
use crate::files::{self, FileRequest, FileTarget, RunFile};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, ops_problem, store_problem};
use crate::query::{QueryParams, event_type_name, parse_snake, parse_task_id};
use crate::schema::API_V1_SCHEMA_JSON;
use crate::state::ApiState;
use crate::types::{
    AccountCheckResponse, AccountCreateBody, AccountList, AccountLoginCodeBody, AccountLoginResult, AccountLoginStart,
    AccountStats, AccountView, AnswerBody, ArtifactList, CancelBody, ClusterConnectCodeBody, ClusterConnectResult,
    ClusterConnectStart, ClusterView, Clusters, DaemonView, DbInfo, DecisionBody, EventsPage, Health,
    MilestoneCreateBody, MilestonePatchBody, OrgCreateBody, OrgList, OrgPatchBody, ProjectCreateBody, ProjectDetail,
    ProjectList, ProjectPatchBody, ProjectTaskView, ProviderCheckResponse, ProviderConfigView, ProviderView,
    Providers, ReloadResult, RetryBody, RunList, SecretList, SecretPutBody, SecretPutResult, SecretView,
    ValidationError,
};
use crate::{API_VERSION, MAX_BODY_BYTES};

pub(crate) type ApiResult = Result<Response, ApiProblem>;

const JSON_CONTENT_TYPE: &str = "application/json; charset=utf-8";
const TITLE_QUERY_MAX_CHARS: usize = 200;
/// ADR-0033 D2: `GET /projects/{id}` が返す仕事の木の上限（GUI が一目で見る図なので十分に大きく取る）。
const PROJECT_TASKS_LIMIT: usize = 2_000;

/// ADR-0024 D2 / ADR-0025 D1: `account_pool = true` は claude-code/codex だけ、かつ `[accounts]` にそのアダプタの
/// 根ディレクトリが設定済みのときだけ有効。
fn check_account_pool_adapter(state: &ApiState, adapter: &str) -> Result<(), ApiProblem> {
    let Some(account_adapter) = task_core::AccountAdapter::parse(adapter) else {
        return Err(ApiProblem::invalid_provider(
            "account_pool = true requires adapter = \"claude-code\" or \"codex\"",
        ));
    };
    if !state.inner.accounts_roots.contains_key(&account_adapter) {
        return Err(ApiProblem::invalid_provider(format!(
            "account_pool = true requires the [accounts] section to configure a root for adapter {adapter:?}"
        )));
    }
    Ok(())
}

pub(crate) fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/inbox", get(inbox))
        .route("/api/v1/tasks", get(list_tasks).post(create_task))
        .route("/api/v1/tasks/{id}", get(task_detail))
        .route("/api/v1/tasks/{id}/events", get(task_events))
        .route("/api/v1/tasks/{id}/runs", get(task_runs))
        .route("/api/v1/tasks/{id}/runs/{run_id}/request", get(run_request))
        .route("/api/v1/tasks/{id}/runs/{run_id}/prompt", get(run_prompt))
        .route("/api/v1/tasks/{id}/runs/{run_id}/stdout", get(run_stdout))
        .route("/api/v1/tasks/{id}/runs/{run_id}/stderr", get(run_stderr))
        .route("/api/v1/tasks/{id}/runs/{run_id}/result", get(run_result))
        .route("/api/v1/tasks/{id}/artifacts", get(artifact_list))
        .route("/api/v1/tasks/{id}/artifacts/{idx}", get(artifact_body))
        .route("/api/v1/tasks/{id}/approve", post(approve))
        .route("/api/v1/tasks/{id}/reject", post(reject))
        .route("/api/v1/tasks/{id}/answer", post(answer))
        .route("/api/v1/tasks/{id}/cancel", post(cancel))
        .route("/api/v1/tasks/{id}/retry", post(retry))
        .route("/api/v1/plans", post(create_plan))
        .route("/api/v1/replay", post(replay))
        .route("/api/v1/graph", get(graph))
        .route("/api/v1/events", get(events))
        .route("/api/v1/stream", get(crate::sse::stream))
        .route("/api/v1/providers", get(providers).post(create_provider))
        .route("/api/v1/providers/{id}", patch(patch_provider).delete(delete_provider))
        .route("/api/v1/providers/{id}/check", post(check_provider))
        .route("/api/v1/reload", post(reload))
        .route("/api/v1/accounts", get(accounts).post(create_account))
        .route("/api/v1/accounts/{id}", delete(delete_account))
        .route("/api/v1/accounts/{id}/check", post(check_account))
        .route("/api/v1/accounts/{id}/login", post(start_account_login).delete(cancel_account_login))
        .route("/api/v1/accounts/{id}/login/code", post(submit_account_login_code))
        .route("/api/v1/clusters", get(clusters))
        .route("/api/v1/clusters/{id}/connect", post(start_cluster_connect).delete(cancel_cluster_connect))
        .route("/api/v1/clusters/{id}/connect/code", post(submit_cluster_connect_code))
        .route("/api/v1/secrets", get(secrets_list))
        .route("/api/v1/secrets/{id}", put(put_secret).delete(delete_secret))
        .route("/api/v1/org", get(org_list).post(create_org_node))
        .route("/api/v1/org/{id}", patch(patch_org_node).delete(delete_org_node))
        // ADR-0033 D4（Phase 24）: 対話。実装は `crate::conversation`。
        .route(
            "/api/v1/org/{id}/messages",
            get(crate::conversation::list_messages).post(crate::conversation::post_message),
        )
        .route("/api/v1/projects", get(project_list).post(create_project))
        .route("/api/v1/projects/{id}", get(project_detail).patch(patch_project))
        .route("/api/v1/projects/{id}/milestones", post(create_milestone))
        .route("/api/v1/milestones/{id}", patch(patch_milestone))
        .merge(crate::project_plan::routes())
        .merge(crate::memory::routes())
        .merge(crate::reports::routes())
        .merge(crate::approvals::routes())
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

pub(crate) fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
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
pub(crate) async fn read_json<T: DeserializeOwned>(body: Body, empty_is_object: bool) -> Result<T, ApiProblem> {
    let bytes = read_body(body).await?;
    let text: &[u8] = if empty_is_object && bytes.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        &bytes
    };
    serde_json::from_slice(text).map_err(|e| ApiProblem::bad_request(format!("invalid JSON body: {e}")))
}

/// ADR-0026 D7 / ADR-0027 D3 / ADR-0030 D2: `command`/`args`/`settings`/`env_from_secrets` は
/// `[[providers]]`/`providers.d/*.toml` の行にしか書けない。`command`/`args` を HTTP から差し替えられると
/// `[api]` のトークンだけで任意コマンド実行に道が開くので、`POST /providers` と `PATCH /providers/{id}` の
/// 本文にこのいずれかのキーがあれば、値の型や中身を見る前に拒否する（`env_from_secrets` は実行コマンドの
/// 差し替えではないが、`ProviderConfigFile` の素通り用フィールドと同じ扱いにして往復で失われないようにする）。
fn reject_provider_command_and_args(map: &serde_json::Map<String, serde_json::Value>) -> Result<(), ApiProblem> {
    if map.contains_key("command")
        || map.contains_key("args")
        || map.contains_key("settings")
        || map.contains_key("env_from_secrets")
    {
        return Err(ApiProblem::invalid_provider(
            "command, args, settings, and env_from_secrets cannot be set through the admin API; edit providers.d/<id>.toml by hand (ADR-0026 D7, ADR-0027 D3, ADR-0030 D2)",
        ));
    }
    Ok(())
}

/// `read_json` と同じだが、先に §ADR-0026 D7 / ADR-0027 D3 / ADR-0030 D2 の
/// `command`/`args`/`settings`/`env_from_secrets` 拒否を通す（`ProviderCreateBody`/`ProviderPatchBody` は
/// このキーを知らないので、素の `read_json` では黙って無視されてしまう）。
async fn read_provider_json<T: DeserializeOwned>(body: Body, empty_is_object: bool) -> Result<T, ApiProblem> {
    let bytes = read_body(body).await?;
    let text: &[u8] = if empty_is_object && bytes.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        &bytes
    };
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(text) {
        reject_provider_command_and_args(&map)?;
    }
    serde_json::from_slice(text).map_err(|e| ApiProblem::bad_request(format!("invalid JSON body: {e}")))
}

fn load_task(store: &SqliteStore, id: TaskId) -> Result<Task, ApiProblem> {
    store
        .get(id)
        .map_err(store_problem)?
        .ok_or_else(|| ApiProblem::task_not_found(id))
}

pub(crate) fn no_query(raw: &Option<String>) -> Result<(), ApiProblem> {
    QueryParams::parse(raw.as_deref(), &[]).map(|_| ())
}

async fn fallback() -> ApiProblem {
    ApiProblem::not_found()
}

async fn method_not_allowed() -> ApiProblem {
    ApiProblem::method_not_allowed()
}


// ---- ADR-0033 D1/D2（Phase 23）: 組織・案件・途中目標 ----
//
// 読み取り（`GET /org`、`GET /projects`、`GET /projects/{id}`）は他の読み取りと同じで無認証でよい。
// 組織の編集（POST/PATCH/DELETE `/org`）は**管理系**なので `token_file` 未設定でも 401（ADR-0017 D1 と同じ規律）。
// 案件と途中目標の作成・状態変更は人の操作（`POST /tasks` と同じ扱い）なので通常の認証だけ。

/// `id` を組織のノードとして読む（存在しなければ 404）。
fn load_org_node(store: &SqliteStore, id: &str) -> Result<OrgNode, ApiProblem> {
    store
        .org_get(id)
        .map_err(store_problem)?
        .ok_or_else(|| ApiProblem::org_node_not_found(id))
}

pub(crate) fn parse_project_id(raw: &str) -> Result<ProjectId, ApiProblem> {
    raw.parse::<ProjectId>().map_err(|_| ApiProblem::project_not_found(raw))
}

fn parse_milestone_id(raw: &str) -> Result<MilestoneId, ApiProblem> {
    raw.parse::<MilestoneId>().map_err(|_| ApiProblem::milestone_not_found(raw))
}

/// 監査 L-1: 組織のノードの `genre` は設定の `[[genres]]` にあるものだけ（分野を 1 つも設定していない
/// 構成では検証しない。`POST /tasks` の `genre` と同じ規律。ADR-0027 D1）。
fn validate_genre(state: &ApiState, genre: Option<&str>) -> Result<(), ApiProblem> {
    let Some(genre) = genre else { return Ok(()) };
    if state.inner.genres.is_empty() || state.inner.genres.iter().any(|g| g.id == genre) {
        return Ok(());
    }
    Err(ApiProblem::validation(vec![ValidationError {
        field: Some("genre".into()),
        message: format!("unknown genre: {genre:?}"),
    }]))
}

async fn org_list(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let items = state
        .blocking(|store| store.org_list().map_err(store_problem))
        .await?;
    Ok(json_response(StatusCode::OK, &OrgList { items }))
}

async fn create_org_node(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let create: OrgCreateBody = read_json(body, false).await?;
    // 監査 L-1: `genre` は `[[genres]]` にあるものだけ受ける（`genres` が空の設定では検証しない）。
    validate_genre(&state, create.genre.as_deref())?;
    let node = state
        .blocking(move |store| {
            if store.org_get(&create.id).map_err(store_problem)?.is_some() {
                return Err(ApiProblem::org_node_exists(&create.id));
            }
            let now = OffsetDateTime::now_utc();
            let node = OrgNode {
                id: create.id,
                parent_id: create.parent_id,
                name: create.name,
                kind: create.kind,
                genre: create.genre,
                brief: create.brief.unwrap_or_default(),
                position: create.position.unwrap_or(0),
                created_at: now,
                updated_at: now,
            };
            store.org_upsert(&node).map_err(store_problem)
        })
        .await?;
    tracing::info!(who = "admin", op = "org_create", org_id = %node.id, "admin: org node created");
    let mut response = json_response(StatusCode::CREATED, &node);
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/org/{}", node.id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
}

async fn patch_org_node(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let patch: OrgPatchBody = read_json(body, true).await?;
    if let Some(genre) = &patch.genre {
        validate_genre(&state, genre.as_deref())?;
    }
    let node = state
        .blocking(move |store| {
            let mut node = load_org_node(store, &id)?;
            if let Some(name) = patch.name {
                node.name = name;
            }
            if let Some(kind) = patch.kind {
                node.kind = kind;
            }
            if let Some(parent_id) = patch.parent_id {
                node.parent_id = Some(parent_id);
            }
            if let Some(genre) = patch.genre {
                node.genre = genre;
            }
            if let Some(brief) = patch.brief {
                node.brief = brief;
            }
            if let Some(position) = patch.position {
                node.position = position;
            }
            node.updated_at = OffsetDateTime::now_utc();
            store.org_upsert(&node).map_err(store_problem)
        })
        .await?;
    tracing::info!(who = "admin", op = "org_patch", org_id = %node.id, "admin: org node updated");
    Ok(json_response(StatusCode::OK, &node))
}

async fn delete_org_node(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    state
        .blocking(move |store| {
            load_org_node(store, &id)?;
            store.org_delete(&id).map_err(store_problem)?;
            tracing::info!(who = "admin", op = "org_delete", org_id = %id, "admin: org node deleted");
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn project_list(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let items = state
        .blocking(|store| store.project_list().map_err(store_problem))
        .await?;
    Ok(json_response(StatusCode::OK, &ProjectList { items }))
}

/// 案件を作る。**管理系**（`token_file` 未設定でも 401）: 直後に秘書の run を起こす経路なので、
/// `POST /org/{id}/messages` と同じ規律にする（監査 M-4）。
async fn create_project(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let create: ProjectCreateBody = read_json(body, false).await?;
    if create.title.trim().is_empty() {
        return Err(ApiProblem::validation(vec![ValidationError {
            field: Some("title".into()),
            message: "title must not be blank".into(),
        }]));
    }
    if create.request.trim().is_empty() {
        return Err(ApiProblem::validation(vec![ValidationError {
            field: Some("request".into()),
            message: "request must not be blank".into(),
        }]));
    }
    let roles = state.inner.roles.clone();
    let genres = state.inner.genres.clone();
    let conversation_genre = state.inner.conversation_genre.clone();
    let project = state
        .blocking(move |store| {
            let now = OffsetDateTime::now_utc();
            let project = Project {
                id: ProjectId::new(),
                title: create.title,
                request: create.request,
                // ADR-0033 D2: 作った直後は `proposed`（秘書が理解確認と方針を返すまで人の返事待ち）。
                status: ProjectStatus::Proposed,
                secretary_summary: None,
                created_at: now,
                updated_at: now,
            };
            store.project_create(&project).map_err(store_problem)?;
            // SPEC §7 / ADR-0033 D4: 案件を受けたら、秘書が最初に「理解の確認・大まかな方針・最初の
            // 途中目標の提案」を返す。ここは対話を 1 回起こすだけ（中身はプロンプトの仕事）。
            crate::conversation::greet_the_secretary(store, &project, &roles, &genres, &conversation_genre);
            Ok(project)
        })
        .await?;
    let mut response = json_response(StatusCode::CREATED, &project);
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/projects/{}", project.id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
}

async fn project_detail(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let project_id = parse_project_id(&id)?;
    let detail = state
        .blocking(move |store| {
            let Some(project) = store.project_get(project_id).map_err(store_problem)? else {
                return Err(ApiProblem::project_not_found(&project_id.to_string()));
            };
            let milestones = store.milestone_list(project_id).map_err(store_problem)?;
            // ADR-0033 D2: 案件の仕事の木 = `tasks WHERE project_id = ?`（DAG は `parent_id` / `depends_on`）。
            let filter = ListFilter { project_id: Some(project_id), ..ListFilter::default() };
            let page = store
                .list_page(&filter, ListOrder::CreatedDesc, None, PROJECT_TASKS_LIMIT)
                .map_err(store_problem)?;
            let tasks = page
                .items
                .into_iter()
                .map(|task| ProjectTaskView {
                    conversation: task_core::is_conversation(&task),
                    support: task_core::support_kind(&task).map(str::to_string),
                    id: task.id,
                    title: task.title,
                    status: task.status,
                    parent_id: task.parent_id,
                    depends_on: task.depends_on,
                    assignee: task.assignee,
                    milestone_id: task.milestone_id,
                })
                .collect();
            Ok(ProjectDetail { project, milestones, tasks })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &detail))
}

async fn patch_project(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let project_id = parse_project_id(&id)?;
    let patch: ProjectPatchBody = read_json(body, false).await?;
    let project = state
        .blocking(move |store| {
            if !store.project_set_status(project_id, patch.status).map_err(store_problem)? {
                return Err(ApiProblem::project_not_found(&project_id.to_string()));
            }
            store
                .project_get(project_id)
                .map_err(store_problem)?
                .ok_or_else(|| ApiProblem::project_not_found(&project_id.to_string()))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &project))
}

async fn create_milestone(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let project_id = parse_project_id(&id)?;
    let create: MilestoneCreateBody = read_json(body, false).await?;
    if create.title.trim().is_empty() {
        return Err(ApiProblem::validation(vec![ValidationError {
            field: Some("title".into()),
            message: "title must not be blank".into(),
        }]));
    }
    let milestone: Milestone = state
        .blocking(move |store| {
            if store.project_get(project_id).map_err(store_problem)?.is_none() {
                return Err(ApiProblem::project_not_found(&project_id.to_string()));
            }
            store
                .milestone_create(
                    project_id,
                    &create.title,
                    create.description.as_deref().unwrap_or(""),
                    create.status.unwrap_or(MilestoneStatus::Proposed),
                )
                .map_err(store_problem)
        })
        .await?;
    let mut response = json_response(StatusCode::CREATED, &milestone);
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/milestones/{}", milestone.id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
}

async fn patch_milestone(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let milestone_id = parse_milestone_id(&id)?;
    let patch: MilestonePatchBody = read_json(body, false).await?;
    let milestone = state
        .blocking(move |store| {
            if !store.milestone_set_status(milestone_id, patch.status).map_err(store_problem)? {
                return Err(ApiProblem::milestone_not_found(&milestone_id.to_string()));
            }
            // 更新後の行を返す（案件が分からないと引けないので、状態を変えた後に案件ごと引き直す）。
            for project in store.project_list().map_err(store_problem)? {
                if let Some(found) = store
                    .milestone_list(project.id)
                    .map_err(store_problem)?
                    .into_iter()
                    .find(|m| m.id == milestone_id)
                {
                    return Ok(found);
                }
            }
            Err(ApiProblem::milestone_not_found(&milestone_id.to_string()))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &milestone))
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
        &["status", "kind", "genre", "parent", "project", "root_only", "q", "order", "limit", "cursor"],
    )?;
    let mut filter = ListFilter::default();
    for status in query.list("status") {
        filter.statuses.push(parse_snake::<Status>("status", status)?);
    }
    for kind in query.list("kind") {
        filter.kinds.push(parse_snake::<TaskKind>("kind", kind)?);
    }
    // ADR-0027 D1: `genre` は自由記述なので `kind` と違って列挙型の検証はしない（完全一致だけ）。
    for genre in query.list("genre") {
        filter.genres.push(genre.to_string());
    }
    filter.parent_id = query.task_id("parent")?;
    // ADR-0033 D2: 案件で絞る（案件の仕事の木。`GET /projects/{id}` は同じ絞り込みを使う）。
    if let Some(raw) = query.single("project")? {
        filter.project_id = Some(raw.parse::<ProjectId>().map_err(|_| {
            ApiProblem::bad_request("query parameter `project` must be a ULID")
        })?);
    }
    filter.root_only = query.bool("root_only")?.unwrap_or(false);
    if let Some(text) = query.single("q")? {
        if text.chars().count() > TITLE_QUERY_MAX_CHARS {
            return Err(ApiProblem::bad_request("query parameter `q` must be at most 200 characters"));
        }
        if !text.is_empty() {
            filter.text_contains = Some(text.to_string());
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
    // ADR-0016 M3 / ADR-0027 D1: 省略された tier / adapter / 予算は `[[roles]]` の既定 → `[[genres]]` の
    // `default_role` の既定 → 全体の既定で埋める。API は常に完全な設定を持つので、`genres` が設定されて
    // いれば知らない `genre` / `genre` と `role` の不整合は常に検証する（taskctl の「`--config` 無し」の
    // 緩さはここには無い）。
    let roles = state.inner.roles.clone();
    let genres = state.inner.genres.clone();
    let task = state
        .blocking(move |store| {
            task_ops::add::create_task_with_roles(store, spec, &roles, &genres, OffsetDateTime::now_utc())
                .map_err(|e| ops_problem(store, e, None))
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

// ---- 8〜10 + 32. GET /tasks/{id}/runs/{run_id}/{stdout|stderr|result|request} ----

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

/// ADR-0023 M1: claude-code / codex が実際に渡したプロンプト文面。
async fn run_prompt(
    State(state): State<ApiState>,
    Params(params): Params<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> ApiResult {
    run_file(state, params, raw, headers, RunFile::Prompt).await
}

/// ADR-0023 D2: ワーカーに渡した `RunRequest`。
async fn run_request(
    State(state): State<ApiState>,
    Params(params): Params<(String, String)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> ApiResult {
    run_file(state, params, raw, headers, RunFile::Request).await
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

/// Phase 31（実機の事故、2026-09-18）: `failed`/`cancelled` を複製してやり直す。
async fn retry(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let RetryBody { accept } = read_json(body, true).await?;
    let result = state
        .blocking(move |store| {
            task_ops::retry::retry_task(store, id, accept, OffsetDateTime::now_utc())
                .map_err(|e| ops_problem(store, e, Some("retry")))
        })
        .await?;
    let mut response = json_response(StatusCode::CREATED, &result);
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/tasks/{}", result.task_id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
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

/// ADR-0017 M4: `reload` 後は `config_view.providers`（起動時に固定）ではなく、次 tick のスナップショットに
/// 乗った一覧を正とする（`Dispatcher::set_snapshot_providers` が更新する）。最初の tick 前だけ静的な値にフォールバックする。
fn current_providers(state: &ApiState, snapshot: Option<&task_ops::daemon::DaemonSnapshot>) -> Vec<ProviderConfigView> {
    match snapshot {
        Some(s) if !s.providers.is_empty() || state.inner.config_view.providers.is_empty() => s
            .providers
            .iter()
            .map(|p| ProviderConfigView {
                id: p.id.clone(),
                adapter: p.adapter.clone(),
                tiers: p.tiers.clone(),
                concurrency: p.concurrency,
                model: p.model.clone(),
                env_keys: p.env_keys.clone(),
                account_pool: p.account_pool,
            })
            .collect(),
        _ => state.inner.config_view.providers.clone(),
    }
}

async fn providers(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let snapshot = state.snapshot();
    let providers = current_providers(&state, snapshot.as_ref());
    let ids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
    let inner = Arc::clone(&state.inner);
    let today = OffsetDateTime::now_utc().date();
    let stats = state
        .blocking(move |store| {
            let mut guard = inner.stats.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.catch_up(store).map_err(store_problem)?;
            Ok(ids.iter().map(|id| guard.view(id, today)).collect::<Vec<_>>())
        })
        .await?;
    let items = providers
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
            // ADR-0022 D2: スナップショットに載っている確認の記録（無ければ null）。
            last_check: snapshot
                .as_ref()
                .and_then(|s| s.providers.iter().find(|live| live.id == provider.id))
                .and_then(|live| live.last_check.clone()),
            stats,
            account_pool: provider.account_pool,
        })
        .collect();
    Ok(json_response(StatusCode::OK, &Providers { items }))
}

// ---- 27〜31. プロバイダ管理（ADR-0017） ----

async fn create_provider(State(state): State<ApiState>, headers: HeaderMap, RawQuery(raw): RawQuery, body: Body) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(dir) = state.inner.providers_dir.clone() else {
        return Err(ApiProblem::providers_admin_unavailable());
    };
    let create: ProviderCreateBody = read_provider_json(body, false).await?;
    if !valid_provider_id(&create.id) {
        return Err(ApiProblem::bad_request(
            "id must be 1-64 ASCII alphanumeric/-/_ characters",
        ));
    }
    if !valid_adapter(&create.adapter) {
        return Err(ApiProblem::bad_request(
            "adapter must be one of fake, claude-code, codex, acp, paperqa, local-deep-research",
        ));
    }
    if create.concurrency.is_some_and(|c| c == 0) {
        return Err(ApiProblem::bad_request("concurrency must be >= 1"));
    }
    // ADR-0024 D2 / ADR-0025 D1 / S1: `account_pool = true` は claude-code/codex だけ、かつ `[accounts]` に
    // そのアダプタの根ディレクトリが設定済みのときだけ有効。
    if create.account_pool {
        check_account_pool_adapter(&state, &create.adapter)?;
    }
    let path = crate::admin::provider_file_path(&dir, &create.id);
    if path.exists() {
        return Err(ApiProblem::provider_exists(&create.id));
    }
    let file = create.into_file();
    write_provider_file(&dir, &file).map_err(|e| ApiProblem::internal(e.to_string()))?;
    tracing::info!(who = "admin", op = "provider_create", provider_id = %file.id, adapter = %file.adapter, "admin: provider created");
    let mut response = json_response(StatusCode::CREATED, &file.to_view());
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/providers/{}", file.id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
}

async fn patch_provider(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(dir) = state.inner.providers_dir.clone() else {
        return Err(ApiProblem::providers_admin_unavailable());
    };
    // id はファイル名に使う（`provider_file_path`）。`create_provider` と同じ検証をここでも通さないと、
    // `..%2F` のような id でディレクトリの外のファイルを読み書きできてしまう（監査で発見）。
    if !valid_provider_id(&id) {
        return Err(ApiProblem::provider_not_found(&id));
    }
    let path = crate::admin::provider_file_path(&dir, &id);
    if !path.exists() {
        return Err(ApiProblem::provider_not_found(&id));
    }
    let patch: ProviderPatchBody = read_provider_json(body, true).await?;
    if patch.concurrency.is_some_and(|c| c == 0) {
        return Err(ApiProblem::bad_request("concurrency must be >= 1"));
    }
    let current = read_provider_file(&path).map_err(|e| ApiProblem::internal(e.to_string()))?;
    let updated = patch.apply(current);
    // ADR-0024 D2 / ADR-0025 D1 / S1: patch 後の組み合わせも検証する（`id`/`adapter` は patch で変わらない）。
    if updated.account_pool {
        check_account_pool_adapter(&state, &updated.adapter)?;
    }
    write_provider_file(&dir, &updated).map_err(|e| ApiProblem::internal(e.to_string()))?;
    tracing::info!(who = "admin", op = "provider_patch", provider_id = %id, "admin: provider patched");
    Ok(json_response(StatusCode::OK, &updated.to_view()))
}

async fn delete_provider(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(dir) = state.inner.providers_dir.clone() else {
        return Err(ApiProblem::providers_admin_unavailable());
    };
    // id はファイル名に使う（`provider_file_path`）。`create_provider` と同じ検証をここでも通さないと、
    // `..%2F` のような id でディレクトリの外のファイルを削除できてしまう（監査で発見）。
    if !valid_provider_id(&id) {
        return Err(ApiProblem::provider_not_found(&id));
    }
    let path = crate::admin::provider_file_path(&dir, &id);
    if !path.exists() {
        return Err(ApiProblem::provider_not_found(&id));
    }
    std::fs::remove_file(&path).map_err(|e| ApiProblem::internal(e.to_string()))?;
    tracing::info!(who = "admin", op = "provider_delete", provider_id = %id, "admin: provider deleted");
    Ok(json_response(StatusCode::OK, &serde_json::json!({})))
}

async fn reload(State(state): State<ApiState>, headers: HeaderMap, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::providers_admin_unavailable());
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx.send(AdminRequest::Reload { reply: reply_tx }).await.is_err() {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx).await {
        Ok(Ok(Ok(()))) => {
            tracing::info!(who = "admin", op = "reload", "admin: providers reloaded");
            Ok(json_response(StatusCode::OK, &ReloadResult { reloaded: true }))
        }
        Ok(Ok(Err(message))) => Err(ApiProblem::bad_request(format!("invalid config: {message}"))),
        Ok(Err(_)) => Err(ApiProblem::internal("taskd dropped the reload request")),
        Err(_) => Err(ApiProblem::internal("reload timed out")),
    }
}

async fn check_provider(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::providers_admin_unavailable());
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::Check { provider_id: id.clone(), reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(40), reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => return Err(ApiProblem::internal("taskd dropped the check request")),
        Err(_) => return Err(ApiProblem::internal("check timed out")),
    };
    match outcome {
        Ok(outcome) => {
            tracing::info!(
                who = "admin", op = "provider_check", provider_id = %id, result = ?outcome.result,
                detail = outcome.detail.as_deref().unwrap_or(""), "admin: provider checked"
            );
            Ok(json_response(
                StatusCode::OK,
                &ProviderCheckResponse {
                    result: outcome.result,
                    checked_at: now_rfc3339(),
                    detail: outcome.detail,
                },
            ))
        }
        Err(crate::admin::CheckError::NotFound) => Err(ApiProblem::provider_not_found(&id)),
        Err(crate::admin::CheckError::ConfigInvalid(message)) => {
            Err(ApiProblem::bad_request(format!("invalid config: {message}")))
        }
        Err(crate::admin::CheckError::Unavailable(message)) => Err(ApiProblem::internal(message)),
    }
}

// ---- Phase 13/14（ADR-0024, ADR-0025）: アカウントのプール（claude-code / codex） ----

/// D3.29 `GET /accounts`: フィルタシステムのスキャン（`logged_in`・`dir`）+ スナップショットの観測値 + 集計を merge する。
/// 読み取りなので認証は不要（管理系は 3.30 以降）。両方のアダプタを `adapter` → `id` の順で返す（ADR-0025 D6）。
async fn accounts(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    if state.inner.accounts_roots.is_empty() {
        return Ok(json_response(
            StatusCode::OK,
            &AccountList { root: None, roots: std::collections::HashMap::new(), max_runs_per_account: 0, items: Vec::new() },
        ));
    }
    let snapshot = state.snapshot();
    let mut items = Vec::new();
    for adapter in task_core::AccountAdapter::ALL {
        let Some(root) = state.inner.accounts_roots.get(&adapter) else { continue };
        let dirs = crate::accounts::scan_accounts(root, adapter);
        let ids: Vec<String> = dirs.iter().map(|d| d.id.clone()).collect();
        let inner = Arc::clone(&state.inner);
        let adapter_str = adapter.as_str().to_string();
        let stats = state
            .blocking(move |store| {
                let mut guard = inner.account_stats.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                guard.catch_up(store).map_err(store_problem)?;
                Ok(ids.iter().map(|id| guard.view(&adapter_str, id)).collect::<Vec<_>>())
            })
            .await?;
        for (d, stats) in dirs.iter().zip(stats) {
            let live = snapshot
                .as_ref()
                .and_then(|s| s.accounts.iter().find(|a| a.adapter == adapter.as_str() && a.id == d.id));
            items.push(AccountView {
                adapter: adapter.as_str().to_string(),
                id: d.id.clone(),
                dir: d.dir.display().to_string(),
                logged_in: d.logged_in,
                in_use: live.map(|l| l.in_use).unwrap_or(0),
                usage: live.and_then(|l| l.usage.as_ref()).map(crate::accounts::usage_view_from_live),
                score: live.and_then(|l| l.score),
                excluded_reason: live.and_then(|l| l.excluded_reason.clone()),
                cooldown: live.and_then(|l| l.cooldown.as_ref()).map(crate::accounts::cooldown_view_from_live),
                last_check: live.and_then(|l| l.last_check.clone()),
                login_pending: live.map(|l| l.login_pending).unwrap_or(false),
                stats,
            });
        }
    }
    let roots: std::collections::HashMap<String, Option<String>> = task_core::AccountAdapter::ALL
        .into_iter()
        .map(|a| (a.as_str().to_string(), state.inner.accounts_roots.get(&a).map(|p| p.display().to_string())))
        .collect();
    Ok(json_response(
        StatusCode::OK,
        &AccountList {
            root: roots.get(task_core::AccountAdapter::ClaudeCode.as_str()).cloned().flatten(),
            roots,
            max_runs_per_account: state.inner.max_runs_per_account,
            items,
        },
    ))
}

/// 3.30 `POST /accounts`: ディレクトリを 0700 で作る。task-api 自身は `Dispatcher`/`AccountBook` に触れない
/// （次の選択のタイミングで taskd がディレクトリを見つける。ADR-0024 D1）。`adapter`（既定 `claude-code`）が
/// 指す根ディレクトリが設定されていなければ 409（ADR-0025 D6）。
async fn create_account(State(state): State<ApiState>, headers: HeaderMap, RawQuery(raw): RawQuery, body: Body) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let create: AccountCreateBody = read_json(body, false).await?;
    if !crate::accounts::valid_account_id(&create.id) {
        return Err(ApiProblem::bad_request("id must be 1-64 ASCII alphanumeric/-/_ characters"));
    }
    let Some(account_adapter) = task_core::AccountAdapter::parse(&create.adapter) else {
        return Err(ApiProblem::bad_request("adapter must be claude-code or codex"));
    };
    let Some(root) = state.inner.accounts_roots.get(&account_adapter).cloned() else {
        return Err(ApiProblem::accounts_unavailable());
    };
    let dir = root.join(&create.id);
    // N7: `exists()` してから作る（TOCTOU）のではなく、`DirBuilder::create` の `AlreadyExists` を使って
    // 作成そのものを排他にする。親（`root`）は先に `create_dir_all` で用意する（無ければ）。
    std::fs::create_dir_all(&root).map_err(|e| ApiProblem::internal(e.to_string()))?;
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(ApiProblem::account_exists(&create.id)),
        Err(e) => return Err(ApiProblem::internal(e.to_string())),
    }
    tracing::info!(who = "admin", op = "account_create", account_id = %create.id, adapter = %account_adapter, "admin: account created");
    let view = AccountView {
        adapter: account_adapter.as_str().to_string(),
        id: create.id.clone(),
        dir: dir.display().to_string(),
        logged_in: false,
        in_use: 0,
        usage: None,
        score: None,
        excluded_reason: None,
        cooldown: None,
        last_check: None,
        login_pending: false,
        stats: AccountStats::default(),
    };
    let mut response = json_response(StatusCode::CREATED, &view);
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/accounts/{}", create.id)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
}

/// 3.31 `DELETE /accounts/{id}`: taskd 側へ委譲する（S2+S8）。`<root>/.removed/<id>-<unix秒>/` へ移す
/// （認証ファイルは消さない）。task-api 自身はファイルを動かさない: スナップショットの `in_use` はポーリング
/// 間隔だけ古くなりうる（レース）ので、`account_in_use`（running/reviewing を直接見る、ディスパッチャの
/// 権威ある値）を持つ taskd 側でチェックしてから移動する。`?adapter=`（省略時 claude-code。ADR-0025 D6）。
async fn delete_account(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let adapter = QueryParams::parse(raw.as_deref(), &["adapter"])?.account_adapter()?;
    require_admin(&state, &headers)?;
    if state.inner.accounts_roots.is_empty() {
        return Err(ApiProblem::accounts_unavailable());
    }
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::accounts_unavailable());
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::AccountRemove { adapter, id: id.clone(), reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx).await {
        Ok(Ok(Ok(()))) => {
            tracing::info!(who = "admin", op = "account_delete", account_id = %id, %adapter, "admin: account removed");
            Ok(json_response(StatusCode::OK, &serde_json::json!({})))
        }
        Ok(Ok(Err(e))) => Err(account_admin_error(&id, e)),
        Ok(Err(_)) => Err(ApiProblem::internal("taskd dropped the account remove request")),
        Err(_) => Err(ApiProblem::internal("account remove timed out")),
    }
}

fn account_admin_error(id: &str, err: AccountAdminError) -> ApiProblem {
    match err {
        AccountAdminError::NotFound => ApiProblem::account_not_found(id),
        // S6: taskd 側の都合で完了できなかった（`[accounts]` 未設定・チャネルが閉じている等）のは
        // サーバの内部エラーではなく、GUI が「アカウント管理は使えない」と表示すべき状態。
        AccountAdminError::Unavailable(_) => ApiProblem::accounts_unavailable(),
        AccountAdminError::LoginNotStarted => ApiProblem::login_not_started(),
        AccountAdminError::LoginFailed(message) => ApiProblem::login_failed(message),
        AccountAdminError::InUse => ApiProblem::account_in_use(id),
        AccountAdminError::LoginCodeNotSupported => ApiProblem::login_code_not_supported(),
    }
}

/// 3.32 `POST /accounts/{id}/check`（ADR-0024 D6, ADR-0025 D4）: taskd 側で実行する（task-api はプロセスを
/// 起動しない）。`?adapter=`（省略時 claude-code）。
async fn check_account(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let adapter = QueryParams::parse(raw.as_deref(), &["adapter"])?.account_adapter()?;
    require_admin(&state, &headers)?;
    if state.inner.accounts_roots.is_empty() {
        return Err(ApiProblem::accounts_unavailable());
    }
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::accounts_unavailable());
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::AccountCheck { adapter, id: id.clone(), reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(70), reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => return Err(ApiProblem::internal("taskd dropped the check request")),
        Err(_) => return Err(ApiProblem::internal("check timed out")),
    };
    match outcome {
        Ok(outcome) => {
            tracing::info!(who = "admin", op = "account_check", account_id = %id, %adapter, result = ?outcome.result, "admin: account checked");
            Ok(json_response(
                StatusCode::OK,
                &AccountCheckResponse {
                    result: outcome.result,
                    checked_at: now_rfc3339(),
                    detail: outcome.detail,
                    usage: outcome
                        .observation
                        .as_ref()
                        .map(|obs| crate::accounts::usage_view_from_observation(obs, "check")),
                },
            ))
        }
        Err(e) => Err(account_admin_error(&id, e)),
    }
}

/// 3.33 `POST /accounts/{id}/login`（ADR-0024 D7, ADR-0025 D5）: ログインを開始する（claude-code は
/// `claude auth login`、codex は `codex login --device-auth`）。`?adapter=`（省略時 claude-code）。
async fn start_account_login(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let adapter = QueryParams::parse(raw.as_deref(), &["adapter"])?.account_adapter()?;
    require_admin(&state, &headers)?;
    if state.inner.accounts_roots.is_empty() {
        return Err(ApiProblem::accounts_unavailable());
    }
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::accounts_unavailable());
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::AccountLoginStart { adapter, id: id.clone(), reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(20), reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => return Err(ApiProblem::internal("taskd dropped the login request")),
        Err(_) => return Err(ApiProblem::internal("login start timed out")),
    };
    match outcome {
        Ok(started) => {
            // D5: URL・認可コードはログに出さない。
            tracing::info!(who = "admin", op = "account_login_start", account_id = %id, %adapter, "admin: account login started");
            let kind = match adapter {
                task_core::AccountAdapter::ClaudeCode => "paste_code",
                task_core::AccountAdapter::Codex => "device_code",
            };
            Ok(json_response(
                StatusCode::OK,
                &AccountLoginStart {
                    kind: kind.to_string(),
                    url: started.url,
                    user_code: started.user_code,
                    expires_at: crate::accounts::rfc3339_unix(started.expires_at_unix),
                },
            ))
        }
        Err(e) => Err(account_admin_error(&id, e)),
    }
}

/// 3.34 `POST /accounts/{id}/login/code`（ADR-0024 D7）: コードは受け取ってもログにも応答にも出さない。
/// claude-code のみ（ADR-0025 D5）。`?adapter=codex` は 409 `login_code_not_supported`。
async fn submit_account_login_code(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    let adapter = QueryParams::parse(raw.as_deref(), &["adapter"])?.account_adapter()?;
    require_admin(&state, &headers)?;
    if state.inner.accounts_roots.is_empty() {
        return Err(ApiProblem::accounts_unavailable());
    }
    if adapter != task_core::AccountAdapter::ClaudeCode {
        return Err(ApiProblem::login_code_not_supported());
    }
    let AccountLoginCodeBody { code } = read_json(body, false).await?;
    if code.trim().is_empty() {
        return Err(ApiProblem::validation(vec![ValidationError {
            field: Some("code".to_string()),
            message: "code must not be blank".to_string(),
        }]));
    }
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::accounts_unavailable());
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::AccountLoginCode { id: id.clone(), code, reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(40), reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => return Err(ApiProblem::internal("taskd dropped the login code request")),
        Err(_) => return Err(ApiProblem::internal("login code timed out")),
    };
    match outcome {
        Ok(result) => {
            tracing::info!(who = "admin", op = "account_login_code", account_id = %id, ok = result.ok, "admin: account login code submitted");
            Ok(json_response(
                StatusCode::OK,
                &AccountLoginResult {
                    result: if result.ok { "ok" } else { "failed" }.to_string(),
                    detail: result.detail,
                },
            ))
        }
        Err(e) => Err(account_admin_error(&id, e)),
    }
}

/// 3.35 `DELETE /accounts/{id}/login`: 進行中のログインを止める（無ければ何もしない）。`?adapter=`（省略時
/// claude-code。ADR-0025 D5）。
async fn cancel_account_login(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let adapter = QueryParams::parse(raw.as_deref(), &["adapter"])?.account_adapter()?;
    require_admin(&state, &headers)?;
    if state.inner.accounts_roots.is_empty() {
        return Err(ApiProblem::accounts_unavailable());
    }
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::accounts_unavailable());
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::AccountLoginCancel { adapter, id: id.clone(), reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx).await {
        Ok(Ok(Ok(()))) => {
            tracing::info!(who = "admin", op = "account_login_cancel", account_id = %id, %adapter, "admin: account login cancelled");
            Ok(json_response(StatusCode::OK, &serde_json::json!({})))
        }
        Ok(Ok(Err(e))) => Err(account_admin_error(&id, e)),
        Ok(Err(_)) => Err(ApiProblem::internal("taskd dropped the login cancel request")),
        Err(_) => Err(ApiProblem::internal("login cancel timed out")),
    }
}

// ---- 23. GET /clusters ----

async fn clusters(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let snapshot = state.snapshot();
    let now = OffsetDateTime::now_utc();
    let items = state
        .inner
        .config_view
        .clusters
        .iter()
        .map(|cluster| {
            let live = snapshot.as_ref().and_then(|s| s.clusters.iter().find(|live| live.id == cluster.id));
            let (cooldown_until, cooldown_remaining_secs) = live
                .and_then(|live| live.cooldown_until.as_ref())
                .and_then(|until| OffsetDateTime::parse(until, &Rfc3339).ok())
                .filter(|until| *until > now)
                .map(|until| (Some(rfc3339(until)), Some((until - now).whole_seconds().max(0) as u64)))
                .unwrap_or((None, None));
            ClusterView {
                id: cluster.id.clone(),
                host: cluster.host.clone(),
                concurrency: cluster.concurrency,
                sync: cluster.sync.clone(),
                delete_on_push: cluster.delete_on_push,
                has_setup: cluster.has_setup,
                env_keys: cluster.env_keys.clone(),
                rsync_excludes: cluster.rsync_excludes.clone(),
                in_use: live.map(|live| live.in_use),
                connected: live.map(|live| live.connected),
                cooldown_until,
                cooldown_remaining_secs,
                auth: cluster.auth.clone(),
                connect_pending: live.map(|live| live.connect_pending).unwrap_or(false),
            }
        })
        .collect();
    Ok(json_response(StatusCode::OK, &Clusters { items }))
}

// ---- ADR-0032 D5: クラスタへの接続を GUI から張る（すべて管理系: `token_file` 未設定でも 401） ----

/// 指定した id が `[[clusters]]` にあるか（無ければ 404。admin_tx へ渡す前にここで弾く）。
fn require_known_cluster(state: &ApiState, id: &str) -> Result<(), ApiProblem> {
    if state.inner.config_view.clusters.iter().any(|c| c.id == id) {
        Ok(())
    } else {
        Err(ApiProblem::cluster_not_found(id))
    }
}

fn cluster_admin_error(id: &str, err: ClusterAdminError) -> ApiProblem {
    match err {
        ClusterAdminError::NotFound => ApiProblem::cluster_not_found(id),
        ClusterAdminError::NotSupported => ApiProblem::cluster_connect_not_supported(),
        ClusterAdminError::NotStarted => ApiProblem::cluster_connect_not_started(),
        ClusterAdminError::InvalidCode => ApiProblem::cluster_connect_code_invalid(),
        ClusterAdminError::Failed(detail) => ApiProblem::cluster_connect_failed(detail),
    }
}

/// `POST /clusters/{id}/connect`（ADR-0032 D5）: taskd 側で ssh の子プロセスを張る／借りる。
/// プロンプト文字列はログには出さない（ユーザ名・ホスト名が入るため）。
async fn start_cluster_connect(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    require_known_cluster(&state, &id)?;
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::ClusterConnectStart { id: id.clone(), reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(40), reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => return Err(ApiProblem::internal("taskd dropped the cluster connect request")),
        Err(_) => return Err(ApiProblem::internal("cluster connect timed out")),
    };
    match outcome {
        Ok(started) => {
            // D4/D5: プロンプト文字列はログに出さない（ユーザ名・ホスト名が入るため）。
            tracing::info!(who = "admin", op = "cluster_connect", cluster = %id, "admin: cluster connect started");
            Ok(json_response(
                StatusCode::OK,
                &ClusterConnectStart {
                    kind: started.kind,
                    prompt: started.prompt,
                    expires_at: started.expires_at_unix.map(crate::accounts::rfc3339_unix),
                },
            ))
        }
        Err(e) => Err(cluster_admin_error(&id, e)),
    }
}

/// `POST /clusters/{id}/connect/code`（ADR-0032 D4/D5）: コードは受け取ってもログにも応答にも出さない。
async fn submit_cluster_connect_code(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    require_known_cluster(&state, &id)?;
    // 監査指摘 D-6 と同じ規律: 型違いで serde のエラー文が値を反射しないよう、専用のメッセージに差し替える。
    let ClusterConnectCodeBody { code } = read_json(body, false)
        .await
        .map_err(|e| if e.status() == StatusCode::BAD_REQUEST { ApiProblem::cluster_connect_body_invalid() } else { e })?;
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| c.is_control()) {
        return Err(ApiProblem::cluster_connect_code_invalid());
    }
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::ClusterConnectCode { id: id.clone(), code, reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(40), reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => return Err(ApiProblem::internal("taskd dropped the cluster connect code request")),
        Err(_) => return Err(ApiProblem::internal("cluster connect code timed out")),
    };
    match outcome {
        Ok(result) => {
            tracing::info!(who = "admin", op = "cluster_connect_code", cluster = %id, ok = result.ok, "admin: cluster connect code submitted");
            Ok(json_response(StatusCode::OK, &ClusterConnectResult { ok: result.ok, detail: result.detail }))
        }
        Err(e) => Err(cluster_admin_error(&id, e)),
    }
}

/// `DELETE /clusters/{id}/connect`（ADR-0032 D5）: 進行中の接続を取り消す、または張った接続を切る。
async fn cancel_cluster_connect(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    require_known_cluster(&state, &id)?;
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx
        .send(AdminRequest::ClusterConnectCancel { id: id.clone(), reply: reply_tx })
        .await
        .is_err()
    {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx).await {
        Ok(Ok(Ok(()))) => {
            tracing::info!(who = "admin", op = "cluster_disconnect", cluster = %id, "admin: cluster connect cancelled");
            Ok(json_response(StatusCode::OK, &serde_json::json!({})))
        }
        Ok(Ok(Err(e))) => Err(cluster_admin_error(&id, e)),
        Ok(Err(_)) => Err(ApiProblem::internal("taskd dropped the cluster disconnect request")),
        Err(_) => Err(ApiProblem::internal("cluster disconnect timed out")),
    }
}

// ---- 秘密（API キー等）の管理（ADR-0030、Phase 20。すべて管理系: `token_file` 未設定でも 401） ----

async fn secrets_list(State(state): State<ApiState>, headers: HeaderMap, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(dir) = state.inner.secrets_dir.clone() else {
        return Err(ApiProblem::secrets_unavailable());
    };
    let metas = crate::secrets::list_secret_files(&dir).map_err(|e| ApiProblem::internal(e.to_string()))?;
    let mut items: Vec<SecretView> = metas
        .into_iter()
        .map(|m| SecretView {
            used_by: state.inner.secret_usage.get(&m.id).cloned().unwrap_or_default(),
            id: m.id,
            updated_at: Some(m.updated_at),
            fingerprint: Some(m.fingerprint),
        })
        .collect();
    // 設定（`env_from_secrets`）が参照しているのに、まだ値が入っていない id も「未設定」として並べる。
    // これが無いと、GUI は「鍵を入れるべき場所」を出せない（ADR-0030 D3 の `used_by` の意図）。
    let present: std::collections::HashSet<&str> = items.iter().map(|i| i.id.as_str()).collect();
    let mut missing: Vec<SecretView> = state
        .inner
        .secret_usage
        .iter()
        .filter(|(id, _)| !present.contains(id.as_str()))
        .map(|(id, used_by)| SecretView {
            id: id.clone(),
            updated_at: None,
            fingerprint: None,
            used_by: used_by.clone(),
        })
        .collect();
    missing.sort_by(|a, b| a.id.cmp(&b.id));
    items.extend(missing);
    Ok(json_response(StatusCode::OK, &SecretList { dir: Some(dir.display().to_string()), items }))
}

/// `id` はファイル名に使う（`secret_file_path`）。パストラバーサル防止のため、無効な形は
/// `PATCH`/`DELETE /providers/{id}` と同じく 404 `secret_not_found` にする（本文を見る前に判定する）。
async fn put_secret(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(dir) = state.inner.secrets_dir.clone() else {
        return Err(ApiProblem::secrets_unavailable());
    };
    if !crate::secrets::valid_secret_id(&id) {
        return Err(ApiProblem::secret_not_found(&id));
    }
    // ADR-0030 D3 の規律「値はログにも応答にも出さない」を、解析エラーの経路でも守る。`read_json` の
    // 400 は serde_json のエラー文をそのまま返すので、型違い（`{"value": 12345678}` 等）だと値の
    // リテラルが応答に反射する。ここだけは本文を見ないメッセージに差し替える（監査指摘 D-6）。
    let put: SecretPutBody = read_json(body, false)
        .await
        .map_err(|e| if e.status() == StatusCode::BAD_REQUEST { ApiProblem::secret_body_invalid() } else { e })?;
    if put.value.trim().is_empty() {
        return Err(ApiProblem::secret_value_invalid());
    }
    crate::secrets::write_secret_file(&dir, &id, &put.value).map_err(|e| ApiProblem::internal(e.to_string()))?;
    // ADR-0030 D3: 応答の fingerprint は、以後の `GET /secrets` と一致するよう読み取り側と同じ
    // trim（末尾改行を落とす）を経た値から計算する。
    let fingerprint = crate::secrets::fingerprint(crate::secrets::trim_secret_value(&put.value));
    let updated_at = now_rfc3339();
    tracing::info!(who = "admin", op = "secret_put", secret_id = %id, "admin: secret stored");
    Ok(json_response(StatusCode::OK, &SecretPutResult { id, updated_at, fingerprint }))
}

async fn delete_secret(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(dir) = state.inner.secrets_dir.clone() else {
        return Err(ApiProblem::secrets_unavailable());
    };
    if !crate::secrets::valid_secret_id(&id) {
        return Err(ApiProblem::secret_not_found(&id));
    }
    let path = crate::secrets::secret_file_path(&dir, &id);
    if !path.exists() {
        return Err(ApiProblem::secret_not_found(&id));
    }
    std::fs::remove_file(&path).map_err(|e| ApiProblem::internal(e.to_string()))?;
    tracing::info!(who = "admin", op = "secret_delete", secret_id = %id, "admin: secret deleted");
    Ok(json_response(StatusCode::OK, &serde_json::json!({})))
}

// ---- 24. GET /daemon, 25. GET /config, 26. GET /schema ----

async fn daemon(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    Ok(json_response(
        StatusCode::OK,
        &DaemonView {
            now: now_rfc3339(),
            // ADR-0033 D3: `reports` だけは API が埋める（`last_notified_at` は API のメモリにある）。
            snapshot: daemon_snapshot_with_reports(&state),
        },
    ))
}

/// ADR-0033 D3 / D5: ディスパッチャのスナップショットに、秘書レベルの未読の報告・通知の判定・未決定の
/// 認可の件数を載せる。
fn daemon_snapshot_with_reports(state: &ApiState) -> Option<task_ops::daemon::DaemonSnapshot> {
    let mut snapshot = state.snapshot()?;
    snapshot.reports = crate::reports::reports_live(&state.inner.store, crate::reports::last_notified_at(state));
    snapshot.approvals_pending = crate::approvals::approvals_pending(&state.inner.store);
    Some(snapshot)
}

async fn config(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let snapshot = state.snapshot();
    let mut view = state.inner.config_view.clone();
    view.providers = current_providers(&state, snapshot.as_ref());
    Ok(json_response(StatusCode::OK, &view))
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
                clusters: Default::default(),
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
                clusters: vec![],
                roles: vec![],
                genres: vec![],
                delegation: task_core::DelegationLimits::default(),
                api: ApiConfigView {
                    bind: "127.0.0.1:7710".into(),
                    auth_required: false,
                    allowed_hosts: vec![],
                },
            },
            roles: vec![],
            genres: vec![],
            conversation_genre: task_core::CONVERSATION_GENRE.to_string(),
            taskd_version: "test".into(),
            instance_id: "01J00000000000000000000000".into(),
            started_at: "2026-09-14T00:00:00Z".into(),
            providers_dir: None,
            admin_tx: None,
            accounts_roots: std::collections::HashMap::new(),
            max_runs_per_account: 0,
            secrets_dir: None,
            secret_usage: std::collections::HashMap::new(),
            memory_dir: None,
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
