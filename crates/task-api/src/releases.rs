//! リリースの一覧と昇格（ADR-0040 D6。Phase 48。`docs/gui/api.md` §3.54/§3.55）。
//!
//! - `GET /releases` — 読み取り（トークン不要）。`[selfdeploy] releases_dir` の下を読むだけ。
//! - `POST /releases/{sha12}/promote` — **管理系**（トークン必須）。リリースに同梱された
//!   `scripts/promote.sh` を detached で起こして 202 を返す。
//!
//! task-api はファイルシステムの規約（`manifest.json` / `gate.json` / `verify.json` / `current` の
//! symlink）を知らない。読み書きは taskd 側（`taskd::releases`）が `ReleaseSource` として渡す
//! （`reload` / `check` / `notify/test` が `AdminRequest` で taskd に委譲するのと同じ境界。
//! こちらは同期の読み取りなのでチャネルではなくトレイトにした）。
//!
//! **昇格を自動で呼ぶ経路は作らない**（ADR-0040 D5: 昇格は人が押す。この API か shell だけ）。

use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};

use crate::handlers::{ApiResult, json_response};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, store_problem};
use crate::state::ApiState;
use crate::types::{ReleaseItem, ReleasePromoteAccepted, ReleaseRunning, Releases};

/// `releases_dir` を読んだ結果（`running` と `instances` はハンドラが足す）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReleasesFs {
    pub current: Option<String>,
    pub previous: Option<String>,
    pub items: Vec<ReleaseItem>,
}

/// 昇格を受け付けられなかった理由（ハンドラが HTTP へ写す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleasePromoteError {
    /// その sha12 のリリースが無い（形が sha12 でないときも含む）→ 404。
    NotFound,
    /// `verify.json` が無い、または `ok` でない → 409。
    NotVerified(String),
    /// 既に `current` → 409。
    AlreadyCurrent,
    /// `promote.lock` の pid が生きている → 409。
    AlreadyPromoting,
    /// `scripts/promote.sh` が無い、起動できない、`[selfdeploy]` が無い → 409。
    Unavailable(String),
}

/// taskd が渡す「リリースのディレクトリ」。**決定的で、LLM もワーカーも関与しない**。
pub trait ReleaseSource: Send + Sync + 'static {
    /// `releases_dir` を読む（失敗しても落ちない。読めなかったリリースは `problem` 付きで出る）。
    fn list(&self) -> ReleasesFs;
    /// `<releases_dir>/<sha12>/scripts/promote.sh <sha12>` を detached で起こす。
    fn promote(&self, sha12: &str) -> Result<ReleasePromoteAccepted, ReleasePromoteError>;
}

/// `GET /releases`。
pub(crate) async fn list(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    let instances = state
        .blocking(move |store| {
            use task_core::TaskStore;
            store.instance_list().map_err(store_problem)
        })
        .await?;
    let fs = match state.inner.releases.clone() {
        Some(source) => tokio::task::spawn_blocking(move || source.list())
            .await
            .map_err(|e| ApiProblem::internal(format!("reading the releases directory failed: {e}")))?,
        None => ReleasesFs::default(),
    };
    Ok(json_response(
        StatusCode::OK,
        &Releases {
            current: fs.current,
            previous: fs.previous,
            running: ReleaseRunning {
                release: state.inner.release.clone(),
                role: state.inner.role.get().to_string(),
                instance_id: state.inner.instance_id.clone(),
            },
            instances,
            items: fs.items,
        },
    ))
}

/// `POST /releases/{sha12}/promote`（管理系）。
pub(crate) async fn promote(
    State(state): State<ApiState>,
    crate::handlers::Params(sha12): crate::handlers::Params<String>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    require_admin(&state, &headers)?;
    let Some(source) = state.inner.releases.clone() else {
        return Err(ApiProblem::release_not_promotable(
            "the [selfdeploy] section is not configured in taskd.toml",
        ));
    };
    let requested = sha12.clone();
    let outcome = tokio::task::spawn_blocking(move || source.promote(&requested))
        .await
        .map_err(|e| ApiProblem::internal(format!("starting promote.sh failed: {e}")))?;
    match outcome {
        Ok(accepted) => {
            // ADR-0040 D5: 昇格は人が押す。誰が押したかは記録に残す（値は sha12 だけ）。
            tracing::warn!(who = "admin", op = "release_promote", sha12 = %accepted.sha12, log = %accepted.log,
                "admin: promote.sh started");
            Ok(json_response(StatusCode::ACCEPTED, &accepted))
        }
        Err(ReleasePromoteError::NotFound) => Err(ApiProblem::release_not_found(&sha12)),
        Err(ReleasePromoteError::NotVerified(detail)) => Err(ApiProblem::release_not_promotable(detail)),
        Err(ReleasePromoteError::AlreadyCurrent) => Err(ApiProblem::release_not_promotable(format!(
            "{sha12} is already the current release"
        ))),
        Err(ReleasePromoteError::AlreadyPromoting) => Err(ApiProblem::release_not_promotable(format!(
            "a promotion of {sha12} is already running"
        ))),
        Err(ReleasePromoteError::Unavailable(detail)) => Err(ApiProblem::release_not_promotable(detail)),
    }
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/v1/releases", get(list))
        .route("/api/v1/releases/{sha12}/promote", post(promote))
}

/// `ApiSettings` が持つ型（`Option` なので `[selfdeploy]` が無い構成でも API は動く）。
pub type SharedReleaseSource = Arc<dyn ReleaseSource>;
