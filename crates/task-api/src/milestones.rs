//! 途中目標の判定（ADR-0038 D2。Phase 41）: `POST /milestones/{id}/decide`。
//!
//! SPEC §7 の「途中目標の達成ごとに人が判定し、Go か再設計」を、**人の 3 つの答え**に落とした入口。
//! 秘書が結果をまとめて次を提案し（レビューの対話 run。ADR-0038 D1）、人がそれに `ok` / `discuss` / `ng`
//! で答える。ハンドラは HTTP の写像だけを行い、判断は `task_ops::milestone_review` に任せる
//! （**達成にするのは人の `ok` だけ**で、LLM は呼ばない）。
//!
//! - **管理系**（`token_file` 未設定でも 401）: `POST /projects/{id}/plan` と同じ規律
//!   （人格を持つノードに仕事・言葉を送る経路）。
//! - 404（知らない途中目標）／409（`reached` 済み）／422（`discuss` / `ng` で `note` が空）。
//! - 応答は **202**（`ok` の計画 run も `discuss` / `ng` の対話も、run の終わりは待たない）。

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{Milestone, MilestoneDecision, MilestoneStatus, TaskId};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, no_query, parse_milestone_id, read_json};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, ops_problem};
use crate::state::ApiState;

/// `POST /milestones/{id}/decide` の要求本文（ADR-0038 D2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MilestoneDecideBody {
    /// `"ok"` / `"discuss"` / `"ng"`。
    pub decision: MilestoneDecision,
    /// 人の自由記述。`discuss` / `ng` では**必須**（空なら 422）。`ok` では任意で、計画の `note` と
    /// 秘書への `messages` に渡る。
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /milestones/{id}/decide` の応答（202）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MilestoneDecided {
    /// 適用した答え。
    pub decision: MilestoneDecision,
    /// 判定した途中目標（更新後）。
    pub milestone: Milestone,
    /// `ok` で承認して分解を始めた次の途中目標、`ng` で `redesigned` にした提案（無ければ省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_milestone: Option<Milestone>,
    /// `ok` で起きた分解（計画 run）のタスク。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_task_id: Option<TaskId>,
    /// `discuss` / `ng` で秘書に送った `role = "user"` の行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// その返事のために起きた対話用タスク。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_task_id: Option<TaskId>,
}

/// 途中目標が既に `reached`（人が `ok` を押した後）なら、もう判定はできない。
fn already_reached(milestone: &Milestone) -> ApiProblem {
    ApiProblem::new(
        StatusCode::CONFLICT,
        "milestone_reached",
        format!("milestone {} has already been reached", milestone.id),
    )
    .with_extra("milestone_status", milestone.status)
}

pub(crate) async fn decide_milestone(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let milestone_id = parse_milestone_id(&id)?;
    let post: MilestoneDecideBody = read_json(body, false).await?;
    let roles = state.inner.roles.clone();
    let genres = state.inner.genres.clone();
    let conversation_genre = state.inner.conversation_genre.clone();
    let decided = state
        .blocking(move |store| {
            let found = task_ops::milestone_review::find(store, milestone_id)
                .map_err(|e| ops_problem(store, e, None))?;
            let Some((project, milestone)) = found else {
                return Err(ApiProblem::milestone_not_found(&milestone_id.to_string()));
            };
            if milestone.status == MilestoneStatus::Reached {
                return Err(already_reached(&milestone));
            }
            task_ops::milestone_review::decide(
                store,
                &project,
                &milestone,
                post.decision,
                post.note.as_deref(),
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
        op = "milestone_decide",
        milestone_id = %milestone_id,
        decision = post_decision(post.decision),
        plan_task_id = decided.plan_task_id.map(|t| t.to_string()),
        conversation_task_id = decided.conversation_task_id.map(|t| t.to_string()),
        "admin: a milestone was decided"
    );
    Ok(json_response(
        StatusCode::ACCEPTED,
        &MilestoneDecided {
            decision: post.decision,
            milestone: decided.milestone,
            next_milestone: decided.next_milestone,
            plan_task_id: decided.plan_task_id,
            message_id: decided.message_id.map(|m| m.to_string()),
            conversation_task_id: decided.conversation_task_id,
        },
    ))
}

fn post_decision(decision: MilestoneDecision) -> &'static str {
    decision.as_str()
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new().route(
        "/api/v1/milestones/{id}/decide",
        axum::routing::post(decide_milestone),
    )
}
