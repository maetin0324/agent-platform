//! 対話（ADR-0033 D4 / Phase 24）: `POST /org/{id}/messages` と `GET /org/{id}/messages`。
//!
//! SPEC §3.4「組織の木を見て誰に言うかを決め、その担当に直接言う。相手は人なので先週の議論の続きとして
//! 話せる」。ハンドラは HTTP の写像だけを行い、判断は `task_ops::conversation` に任せる（LLM は呼ばない）。
//!
//! - **話しかける**のは変更系のうち**管理系**（`token_file` 未設定でも 401）。人格を持つノードに指示を
//!   出す経路なので、`POST /org`（組織の編集）と同じ規律にする。
//! - **読む**のは他の読み取りと同じで無認証でよい（GUI がポーリング / SSE で見る）。
//! - 応答は **202**（同期で返事を待たない。返事は run が終わってから `messages` に増える）。

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{Message, ProjectId, TaskId, TaskStore};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, no_query, read_json};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, ops_problem, store_problem};
use crate::state::ApiState;

/// `GET /org/{id}/messages` の既定の件数（`limit` で変えられる）。
pub const DEFAULT_MESSAGE_LIMIT: usize = 50;
/// `limit` の上限。
pub const MAX_MESSAGE_LIMIT: usize = 500;

/// `POST /org/{id}/messages` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessagePostBody {
    /// 本文（空白だけは 422）。
    pub text: String,
    /// この案件についての話なら案件の id（省略すると案件に紐づかない雑談になる）。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
}

/// `POST /org/{id}/messages` の応答（202）。返事は待たずに、GUI が `GET /org/{id}/messages` で拾う。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MessageAccepted {
    /// 入った `role = "user"` の行の id。
    pub message_id: String,
    /// そのノードの run を起こすために作られた対話用タスク。
    pub task_id: TaskId,
}

/// `GET /org/{id}/messages` の応答（古い順）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MessageList {
    pub items: Vec<Message>,
}

/// 人がノードに話しかける（ADR-0033 D4）。
pub(crate) async fn post_message(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let post: MessagePostBody = read_json(body, false).await?;
    let roles = state.inner.roles.clone();
    let genres = state.inner.genres.clone();
    let conversation_genre = state.inner.conversation_genre.clone();
    let started = state
        .blocking(move |store| {
            // 知らないノードは 404（検証の 422 ではなく、URL が指すものが無い）。
            if store.org_get(&id).map_err(store_problem)?.is_none() {
                return Err(ApiProblem::org_node_not_found(&id));
            }
            task_ops::conversation::start(
                store,
                &id,
                post.project_id,
                &post.text,
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
        op = "org_message",
        org_id = %started.message.node_id,
        task_id = %started.task.id,
        "admin: message sent to an org node"
    );
    Ok(json_response(
        StatusCode::ACCEPTED,
        &MessageAccepted {
            message_id: started.message.id.to_string(),
            task_id: started.task.id,
        },
    ))
}

/// そのノードとのやり取りを古い順に返す（ADR-0033 D4）。
pub(crate) async fn list_messages(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let query = crate::query::QueryParams::parse(raw.as_deref(), &["project", "limit"])?;
    let project_id = match query.single("project")? {
        Some(raw) => Some(
            raw.parse::<ProjectId>()
                .map_err(|_| ApiProblem::project_not_found(raw))?,
        ),
        None => None,
    };
    let limit = query.limit("limit", DEFAULT_MESSAGE_LIMIT, MAX_MESSAGE_LIMIT)?;
    let items = state
        .blocking(move |store| {
            if store.org_get(&id).map_err(store_problem)?.is_none() {
                return Err(ApiProblem::org_node_not_found(&id));
            }
            store.message_list(&id, project_id, limit).map_err(store_problem)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &MessageList { items }))
}

/// SPEC §7 / ADR-0033 D4: 案件を作った直後に、秘書へ依頼文をそのまま話しかける（返事に案件の理解の確認・
/// 大まかな方針・最初の途中目標の提案を含めるのは**プロンプトの仕事**で、ここは対話を 1 回起こすだけ）。
/// 秘書がいない（組織を種蒔きしていない）構成では何もしない。失敗しても案件の作成は成功のままにする。
pub(crate) fn greet_the_secretary(
    store: &task_core::SqliteStore,
    project: &task_core::Project,
    roles: &[task_core::RoleSpec],
    genres: &[task_core::GenreSpec],
    conversation_genre: &str,
) {
    let Some(secretary) = store
        .org_list()
        .unwrap_or_default()
        .into_iter()
        .find(|n| n.kind == task_core::OrgKind::Secretary)
    else {
        return;
    };
    match task_ops::conversation::start(
        store,
        &secretary.id,
        Some(project.id),
        &project.request,
        roles,
        genres,
        conversation_genre,
        OffsetDateTime::now_utc(),
    ) {
        Ok(started) => {
            tracing::info!(
                project_id = %project.id,
                org_id = %secretary.id,
                task_id = %started.task.id,
                "project created: asked the secretary for its understanding, plan and first milestone"
            );
        }
        Err(e) => {
            tracing::warn!(project_id = %project.id, error = %e, "could not start the secretary's first reply");
        }
    }
}
