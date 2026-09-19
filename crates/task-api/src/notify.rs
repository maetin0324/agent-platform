//! 通知の設定とテスト送信（ADR-0037 D4。Phase 39。`docs/gui/api.md` §3.33）。
//!
//! - `GET /notify` — 設定済みか、直近の送信 10 件（読み取り）。
//! - `POST /notify/test` — その場でテスト送信（**管理系**。トークン必須）。
//!
//! **webhook の URL は応答にも問題詳細にも出さない**（ADR-0030 D3 の秘密の規律そのまま）。
//! 出すのは「設定済みかどうか」と `fingerprint`（値の sha256 の先頭 8 桁）だけ。
//! 送信そのものは HTTP クライアントを持つ taskd へ `AdminRequest::NotifyTest` で委譲する
//! （task-api はネットワークに出ない。`reload` / `check` と同じ形）。

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::ProjectId;
use task_core::notify::{Notification, NotificationKind, NotificationStore};

use crate::admin::{AdminRequest, NotifyAdminError};
use crate::handlers::{ApiResult, json_response, rfc3339};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, store_problem};
use crate::secrets::fingerprint;
use crate::state::ApiState;

/// `GET /notify` に載せる直近の送信の件数（ADR-0037 D4）。
const RECENT_LIMIT: usize = 10;

/// `GET /notify` の応答。**URL は含まない**。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NotifyView {
    /// webhook の秘密が登録されていて、送れる状態か。
    pub configured: bool,
    /// `[notify] discord_webhook_secret`（GUI は「この id で API キー画面に登録して」と案内する）。
    pub secret_id: String,
    /// 登録済みのときだけ。値の sha256 の先頭 8 桁（値は復元できない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// `[notify] gui_base_url`（文面のリンクの根）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gui_base_url: Option<String>,
    /// 直近の送信（新しい順、最大 10 件）。
    pub recent: Vec<NotifyRecent>,
}

/// `GET /notify` の `recent[]` の 1 件（ADR-0037 D4）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NotifyRecent {
    pub kind: NotificationKind,
    /// 重複排除の鍵（途中目標 id / 認可 id / タスク id / 報告 id / 案件 id）。
    pub key: String,
    /// GUI がリンクを作るための案件 id（ADR-0037 D6 / GUI 依頼 G13i-P1）。`milestone_ready` はその
    /// 途中目標の案件、`secretary_reply` はその案件自身、他の種は `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    pub attempts: u32,
    /// `null` = まだ決着していない、`true` = 送れた、`false` = 諦めた。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// 失敗の理由（URL・ホスト名は含まない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<Notification> for NotifyRecent {
    fn from(n: Notification) -> Self {
        Self {
            kind: n.kind,
            key: n.key,
            project_id: n.project_id,
            created_at: rfc3339(n.created_at),
            sent_at: n.sent_at.map(rfc3339),
            attempts: n.attempts,
            ok: n.ok,
            error: n.error,
        }
    }
}

/// `POST /notify/test` の応答。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NotifyTestResult {
    pub ok: bool,
    /// 人が読む一行（失敗の種別だけ。URL・ホスト名は含まない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 秘密ファイルの中身の指紋（登録されていれば `Some`）。値そのものはここから出さない。
fn webhook_fingerprint(state: &ApiState) -> Option<String> {
    let dir = state.inner.secrets_dir.as_ref()?;
    let id = state.inner.notify_secret_id.as_str();
    if !crate::secrets::valid_secret_id(id) {
        return None;
    }
    let text = std::fs::read_to_string(dir.join(id)).ok()?;
    let value = crate::secrets::trim_secret_value(&text);
    if value.is_empty() {
        return None;
    }
    Some(fingerprint(value))
}

pub(crate) async fn view(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    let recent = state
        .blocking(move |store| {
            store
                .notification_recent(RECENT_LIMIT)
                .map(|rows| rows.into_iter().map(NotifyRecent::from).collect::<Vec<_>>())
                .map_err(store_problem)
        })
        .await?;
    let fingerprint = webhook_fingerprint(&state);
    Ok(json_response(
        StatusCode::OK,
        &NotifyView {
            configured: fingerprint.is_some(),
            secret_id: state.inner.notify_secret_id.clone(),
            fingerprint,
            gui_base_url: state.inner.notify_gui_base_url.clone(),
            recent,
        },
    ))
}

pub(crate) async fn test(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    require_admin(&state, &headers)?;
    crate::middleware::require_active(&state)?;
    if webhook_fingerprint(&state).is_none() {
        return Err(ApiProblem::notify_unavailable(format!(
            "no Discord webhook is registered; add it as the secret `{}`",
            state.inner.notify_secret_id
        )));
    }
    let Some(admin_tx) = state.inner.admin_tx.clone() else {
        return Err(ApiProblem::notify_unavailable(
            "taskd is not accepting admin requests, so the test cannot be sent",
        ));
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if admin_tx.send(AdminRequest::NotifyTest { reply: reply_tx }).await.is_err() {
        return Err(ApiProblem::internal("taskd is not accepting admin requests"));
    }
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(30), reply_rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => return Err(ApiProblem::internal("taskd dropped the notify test request")),
        Err(_) => return Err(ApiProblem::internal("the notify test timed out")),
    };
    match outcome {
        Ok(outcome) => {
            tracing::info!(
                who = "admin", op = "notify_test", ok = outcome.ok,
                detail = outcome.detail.as_deref().unwrap_or(""), "admin: notify test sent"
            );
            Ok(json_response(
                StatusCode::OK,
                &NotifyTestResult {
                    ok: outcome.ok,
                    detail: outcome.detail,
                },
            ))
        }
        Err(NotifyAdminError::Unavailable(detail)) => Err(ApiProblem::notify_unavailable(detail)),
    }
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/v1/notify", get(view))
        .route("/api/v1/notify/test", post(test))
}
