//! 報告の API（ADR-0033 D3。Phase 25。`docs/gui/api.md` §3.30）。
//!
//! - `GET /reports` — 一覧（新しい順、絞り込みつき）。読み取りなので通常の認証だけ。
//! - `GET /reports/{id}` — 1 件（`sources` の中身も展開する）。
//! - `POST /reports/read` — 既読（**管理系**。トークン必須）。
//! - `POST /reports/notified` — 前回の通知時刻を今に進める（**管理系**）。
//!
//! 通知の判定（`notify_now`）は `task_core::report::notify_now` の決定的な関数で、ここは値を組むだけ。
//! `last_notified_at` は DB に列が無い（migration を足さない）ので、**API プロセスのメモリ**に持つ
//! 観測値として扱う（taskd を再起動すると「まだ通知していない」状態に戻る）。

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::report::{self, Report, ReportFilter, ReportId, ReportStore, ReportsLive};
use task_core::{ProjectId, SqliteStore};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, json_response, read_json, rfc3339};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, store_problem};
use crate::query::QueryParams;
use crate::state::ApiState;

/// 一覧の既定と上限（`limit`）。
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;

/// `GET /reports` の応答。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportList {
    pub items: Vec<Report>,
}

/// `GET /reports/{id}` の応答（`sources` の中身も展開して返す）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportDetail {
    pub report: Report,
    /// `report.sources` の順に引いた元の報告（見つからなかったものは飛ばす）。
    pub sources_expanded: Vec<Report>,
}

/// `POST /reports/read` の本文。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportsReadBody {
    pub ids: Vec<String>,
}

/// `POST /reports/read` の応答。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportsReadResult {
    /// 未読から既読に変わった件数。
    pub updated: usize,
}

/// `POST /reports/notified` の応答。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportsNotifiedResult {
    /// 進めた後の通知時刻（RFC 3339）。
    pub last_notified_at: String,
}

fn report_not_found(id: &str) -> ApiProblem {
    ApiProblem::new(StatusCode::NOT_FOUND, "report_not_found", format!("no report {id}"))
}

fn parse_report_id(raw: &str) -> Result<ReportId, ApiProblem> {
    raw.parse::<ReportId>().map_err(|_| report_not_found(raw))
}

/// ADR-0033 D3: スナップショットに載せる `reports`（未読の件数と通知の判定）。
/// ディスパッチャではなく API がここで組む（`last_notified_at` は API のメモリにあるため）。
pub(crate) fn reports_live(store: &SqliteStore, last_notified_at: Option<OffsetDateTime>) -> Option<ReportsLive> {
    let (unread_secretary, unread_bad_news) = store.report_unread_counts(0).ok()?;
    Some(ReportsLive {
        unread_secretary,
        unread_bad_news,
        last_notified_at: last_notified_at.map(rfc3339),
        notify_now: report::notify_now(
            unread_secretary,
            unread_bad_news,
            last_notified_at,
            OffsetDateTime::now_utc(),
        ),
    })
}

pub(crate) async fn list(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["project", "node", "level", "unread", "limit"])?;
    let project_id = match query.single("project")? {
        Some(raw) => Some(
            raw.parse::<ProjectId>()
                .map_err(|_| ApiProblem::bad_request("query parameter `project` must be a ULID"))?,
        ),
        None => None,
    };
    let node_id = query.single("node")?.map(str::to_string);
    let level = query
        .u64("level")?
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    let unread_only = query.bool("unread")?.unwrap_or(false);
    let limit = query
        .u64("limit")?
        .map(|v| usize::try_from(v).unwrap_or(MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    let filter = ReportFilter {
        project_id,
        node_id,
        level,
        unread_only,
        limit,
    };
    let items = state
        .blocking(move |store| store.report_list(&filter).map_err(store_problem))
        .await?;
    Ok(json_response(StatusCode::OK, &ReportList { items }))
}

pub(crate) async fn detail(
    State(state): State<ApiState>,
    crate::handlers::Params(id): crate::handlers::Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    let report_id = parse_report_id(&id)?;
    let detail = state
        .blocking(move |store| {
            let Some(report) = store.report_get(report_id).map_err(store_problem)? else {
                return Err(report_not_found(&report_id.to_string()));
            };
            let mut sources_expanded = Vec::new();
            for source in &report.sources {
                if let Some(found) = store.report_get(*source).map_err(store_problem)? {
                    sources_expanded.push(found);
                }
            }
            Ok(ReportDetail {
                report,
                sources_expanded,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &detail))
}

pub(crate) async fn mark_read(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
    body: Body,
) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    require_admin(&state, &headers)?;
    let read: ReportsReadBody = read_json(body, false).await?;
    let mut ids = Vec::with_capacity(read.ids.len());
    for raw in &read.ids {
        ids.push(parse_report_id(raw)?);
    }
    let updated = state
        .blocking(move |store| {
            store
                .report_mark_read(&ids, OffsetDateTime::now_utc())
                .map_err(store_problem)
        })
        .await?;
    tracing::info!(who = "admin", op = "reports_read", updated, "admin: reports marked as read");
    Ok(json_response(StatusCode::OK, &ReportsReadResult { updated }))
}

pub(crate) async fn notified(
    State(state): State<ApiState>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    require_admin(&state, &headers)?;
    let now = OffsetDateTime::now_utc();
    match state.inner.last_notified_at.lock() {
        Ok(mut cell) => *cell = Some(now),
        Err(_) => return Err(ApiProblem::internal("the notification state is poisoned")),
    }
    tracing::info!(who = "admin", op = "reports_notified", "admin: notification time advanced");
    Ok(json_response(
        StatusCode::OK,
        &ReportsNotifiedResult {
            last_notified_at: rfc3339(now),
        },
    ))
}

/// ハンドラ以外から使う（`GET /daemon` がスナップショットに `reports` を載せる）。
pub(crate) fn last_notified_at(state: &ApiState) -> Option<OffsetDateTime> {
    state.inner.last_notified_at.lock().ok().and_then(|cell| *cell)
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/v1/reports", get(list))
        .route("/api/v1/reports/read", post(mark_read))
        .route("/api/v1/reports/notified", post(notified))
        .route("/api/v1/reports/{id}", get(detail))
}
