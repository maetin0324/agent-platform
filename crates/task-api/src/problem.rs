//! エラー応答（`application/problem+json`、`docs/gui/api.md` §1.5）と、`OpsError` / `StoreError` からの写像。
//!
//! ハンドラは `ApiProblem` を返すだけで、本体（`instance` = `X-Request-Id` を含む）は共通の middleware が
//! 描画する（応答の拡張に `PendingProblem` を載せて渡す）。

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Map, Value};
use task_core::{StoreError, TaskId, TaskStore};
use task_ops::OpsError;

use crate::types::{Problem, ValidationError};

pub(crate) const X_TASKD_SIZE: HeaderName = HeaderName::from_static("x-taskd-size");
const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

#[derive(Debug, Clone)]
pub(crate) struct ApiProblem {
    status: StatusCode,
    code: &'static str,
    detail: String,
    extra: Map<String, Value>,
    headers: Vec<(HeaderName, HeaderValue)>,
}

/// middleware に描画を任せるための印（応答の拡張）。
#[derive(Debug, Clone)]
pub(crate) struct PendingProblem(pub(crate) ApiProblem);

impl ApiProblem {
    pub(crate) fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
            extra: Map::new(),
            headers: Vec::new(),
        }
    }

    pub(crate) fn with_extra(mut self, key: &str, value: impl Serialize) -> Self {
        if let Ok(value) = serde_json::to_value(value) {
            self.extra.insert(key.to_string(), value);
        }
        self
    }

    pub(crate) fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.push((name, value));
        self
    }

    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", detail)
    }

    pub(crate) fn host_not_allowed() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "host_not_allowed",
            "the Host header is not in the allow list",
        )
    }

    pub(crate) fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "a valid bearer token is required").with_header(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"taskd\""),
        )
    }

    pub(crate) fn origin_forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "origin_forbidden",
            "requests carrying an Origin header are not accepted",
        )
    }

    pub(crate) fn path_forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "path_forbidden", detail)
    }

    pub(crate) fn task_not_found(id: TaskId) -> Self {
        Self::new(StatusCode::NOT_FOUND, "task_not_found", format!("task not found: {id}"))
    }

    pub(crate) fn run_not_found(run_id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "run_not_found", format!("run directory not found: {run_id}"))
    }

    pub(crate) fn artifact_not_found(idx: usize) -> Self {
        Self::new(StatusCode::NOT_FOUND, "artifact_not_found", format!("artifact index out of range: {idx}"))
    }

    pub(crate) fn file_not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "file_not_found", detail)
    }

    pub(crate) fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such endpoint")
    }

    /// ADR-0017: 指定した provider id が `providers.d/` に無い。
    pub(crate) fn provider_not_found(id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "provider_not_found", format!("provider not found: {id}"))
    }

    /// ADR-0017 D1: `POST /api/v1/providers` の id が既に `providers.d/<id>.toml` にある。
    pub(crate) fn provider_exists(id: &str) -> Self {
        Self::new(StatusCode::CONFLICT, "provider_exists", format!("provider already exists: {id}"))
    }

    /// ADR-0017 M1: `providers_include` が未設定で、管理系の書き込みができない。
    pub(crate) fn providers_admin_unavailable() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "providers_admin_unavailable",
            "the [api] provider admin endpoints require `providers_include` to be configured in taskd.toml",
        )
    }

    /// ADR-0024 D5: 指定した account id が `[accounts] claude_dir` に無い。
    pub(crate) fn account_not_found(id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "account_not_found", format!("account not found: {id}"))
    }

    /// ADR-0024 D5: `POST /api/v1/accounts` の id が既にディレクトリとして存在する。
    pub(crate) fn account_exists(id: &str) -> Self {
        Self::new(StatusCode::CONFLICT, "account_exists", format!("account already exists: {id}"))
    }

    /// ADR-0024 D5: `[accounts]` が設定されていない。
    pub(crate) fn accounts_unavailable() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "accounts_unavailable",
            "the [accounts] section is not configured in taskd.toml",
        )
    }

    /// ADR-0024 D5: `in_use > 0` のアカウントは削除できない。
    pub(crate) fn account_in_use(id: &str) -> Self {
        Self::new(StatusCode::CONFLICT, "account_in_use", format!("account is in use: {id}"))
    }

    /// ADR-0024 D5/D7: `login/code` を呼んだが進行中のログインが無い。
    pub(crate) fn login_not_started() -> Self {
        Self::new(StatusCode::CONFLICT, "login_not_started", "no login is in progress for this account")
    }

    /// ADR-0024 D5/D7: `login` の開始自体に失敗した（15 秒以内に URL が出ない等）。
    pub(crate) fn login_failed(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "login_failed", detail)
    }

    /// ADR-0025 D5: codex は `login/code` を使わない（device フローで完結する）。
    pub(crate) fn login_code_not_supported() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "login_code_not_supported",
            "this adapter's login does not use a code submission step",
        )
    }

    /// ADR-0024 D5: `account_pool = true` だが `adapter != "claude-code"`（API 側で判定できる範囲）。
    pub(crate) fn invalid_provider(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_provider", detail)
    }

    /// ADR-0030 D1: `[secrets]` が設定されていない。
    pub(crate) fn secrets_unavailable() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "secrets_unavailable",
            "the [secrets] section is not configured in taskd.toml",
        )
    }

    /// ADR-0030 D3: 指定した secret id が `[secrets] dir` に無い（または id の形が不正）。
    pub(crate) fn secret_not_found(id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "secret_not_found", format!("secret not found: {id}"))
    }

    /// ADR-0032 D5: 指定した cluster id が `[[clusters]]` に無い。
    pub(crate) fn cluster_not_found(id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "cluster_not_found", format!("cluster not found: {id}"))
    }

    /// ADR-0032 D5: `auth = "manual"` のクラスタに `connect` した（人の操作で接続する運用のまま）。
    pub(crate) fn cluster_connect_not_supported() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "cluster_connect_not_supported",
            "this cluster's auth is \"manual\"; connect it with scripts/cluster-login.sh instead",
        )
    }

    /// ADR-0032 D5: 進行中のセッションが無いのに `connect/code` を呼んだ。
    pub(crate) fn cluster_connect_not_started() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "cluster_connect_not_started",
            "no cluster connect session is in progress for this cluster",
        )
    }

    /// ADR-0032 D5: `POST /clusters/{id}/connect/code` の本文が JSON として読めない（構文誤り・型違い・
    /// 未知フィールド）。**serde のエラー文は返さない**（`secret_body_invalid` と同じ理由）。
    pub(crate) fn cluster_connect_body_invalid() -> Self {
        Self::bad_request("invalid JSON body: expected an object with a string `code`")
    }

    /// ADR-0032 D4/D5: コードが空・空白だけ・制御文字を含む（ssh には渡していない）。
    pub(crate) fn cluster_connect_code_invalid() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation",
            "code must not be blank or contain control characters",
        )
    }

    /// ADR-0032 D5: 接続そのものの失敗（ssh の失敗、タイムアウト等）。
    pub(crate) fn cluster_connect_failed(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "cluster_connect_failed", detail)
    }

    // ---- ADR-0033 D1/D2（Phase 23）: 組織・案件・途中目標 ----

    pub(crate) fn org_node_not_found(id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "org_node_not_found", format!("org node not found: {id}"))
    }

    /// `POST /org` の id が既にある（更新は `PATCH /org/{id}`）。
    pub(crate) fn org_node_exists(id: &str) -> Self {
        Self::new(StatusCode::CONFLICT, "org_node_exists", format!("org node already exists: {id}"))
    }

    /// ADR-0033 D1: 仕事を抱えている（または子を持つ）ノードは消せない。
    pub(crate) fn org_node_in_use(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "org_node_in_use", detail)
    }

    pub(crate) fn project_not_found(id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "project_not_found", format!("project not found: {id}"))
    }

    pub(crate) fn milestone_not_found(id: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "milestone_not_found", format!("milestone not found: {id}"))
    }

    /// ADR-0033 D6（GUI 監査対応 Phase 29）: `[memory]` が設定されていない。
    pub(crate) fn memory_unavailable() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "memory_unavailable",
            "the [memory] section is not configured in taskd.toml",
        )
    }

    /// ADR-0037 D4（Phase 39）: Discord の webhook がまだ登録されていない（`[secrets]` 自体が無い、
    /// `[notify] discord_webhook_secret` の秘密が無い、taskd が管理系を受けていない）。
    /// **URL も秘密のパスも文面に入れない**。
    pub(crate) fn notify_unavailable(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "notify_unavailable", detail)
    }

    /// この Problem の HTTP ステータス（`put_secret` が解析エラーだけを差し替えるために見る）。
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    /// ADR-0030 D3: `PUT /secrets/{id}` の本文が JSON として読めない（構文誤り・型違い・未知フィールド）。
    /// **serde のエラー文は返さない**。型違いのときに値のリテラルが応答へ反射するのを防ぐため
    /// （値はログにも応答にも出さない、の徹底。監査指摘 D-6）。
    pub(crate) fn secret_body_invalid() -> Self {
        Self::bad_request("invalid JSON body: expected an object with a string `value`")
    }

    /// ADR-0030 D3: `PUT /secrets/{id}` の値が空白だけ。
    pub(crate) fn secret_value_invalid() -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "validation", "value must not be empty or whitespace-only")
    }

    pub(crate) fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "method not allowed for this endpoint",
        )
    }

    pub(crate) fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "request body exceeds 1 MiB",
        )
    }

    pub(crate) fn unsupported_media_type() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be application/json",
        )
    }

    pub(crate) fn range_not_satisfiable(size: u64) -> Self {
        let problem = Self::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "range_not_satisfiable",
            format!("requested range is not satisfiable for a file of {size} bytes"),
        )
        .with_header(X_TASKD_SIZE, HeaderValue::from(size));
        match HeaderValue::from_str(&format!("bytes */{size}")) {
            Ok(value) => problem.with_header(header::CONTENT_RANGE, value),
            Err(_) => problem,
        }
    }

    pub(crate) fn validation(errors: Vec<ValidationError>) -> Self {
        let detail = errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "validation", detail).with_extra("errors", errors)
    }

    pub(crate) fn too_many_streams() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "too_many_streams",
            "too many concurrent event streams",
        )
        .with_header(header::RETRY_AFTER, HeaderValue::from_static("5"))
    }

    pub(crate) fn db_busy() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "db_busy", "the database is busy")
            .with_header(header::RETRY_AFTER, HeaderValue::from_static("1"))
    }

    pub(crate) fn replay_in_progress() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "replay_in_progress",
            "another replay is in progress",
        )
        .with_header(header::RETRY_AFTER, HeaderValue::from_static("5"))
    }

    pub(crate) fn internal(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", detail)
    }

    /// `instance` を `request_id` にして `application/problem+json` の応答を作る。
    pub(crate) fn render(self, request_id: &str) -> Response {
        let problem = Problem {
            r#type: format!("urn:taskd:problem:{}", self.code),
            title: self.code.replace('_', " "),
            status: self.status.as_u16(),
            detail: self.detail,
            code: self.code.to_string(),
            instance: format!("urn:taskd:request:{request_id}"),
            extra: self.extra,
        };
        let body = serde_json::to_vec(&problem).unwrap_or_else(|_| b"{}".to_vec());
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_CONTENT_TYPE));
        for (name, value) in self.headers {
            headers.insert(name, value);
        }
        response
    }
}

impl IntoResponse for ApiProblem {
    fn into_response(self) -> Response {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = self.status;
        response.extensions_mut().insert(PendingProblem(self));
        response
    }
}

/// `OpsError::Validation` の文言から対象のフィールドを推定する（api.md §1.5）。
pub(crate) fn validation_field(message: &str) -> Option<&'static str> {
    if message.starts_with("at least one acceptance criterion") {
        Some("acceptance")
    } else if message.starts_with("dependency ") {
        Some("depends_on")
    } else if message.starts_with("goal ") {
        Some("goal")
    } else if message.starts_with("title ") {
        Some("title")
    } else if message.starts_with("objective ") {
        Some("objective")
    } else if message.starts_with("parent ") {
        Some("parent")
    } else {
        None
    }
}

/// `OpsError` → HTTP（api.md §1.5 の写像表）。`trigger` は操作名（`approve` 等）。`InvalidState` の
/// `task_status` / `kind` は現在のタスクから読む。
pub(crate) fn ops_problem(store: &dyn TaskStore, err: OpsError, trigger: Option<&str>) -> ApiProblem {
    let detail = err.to_string();
    match err {
        OpsError::NotFound(id) => ApiProblem::task_not_found(id),
        OpsError::InvalidState { id, .. } => {
            let mut problem = ApiProblem::new(StatusCode::CONFLICT, "invalid_transition", detail);
            if let Ok(Some(task)) = store.get(id) {
                problem = problem.with_extra("task_status", task.status).with_extra("kind", task.kind);
            }
            if let Some(trigger) = trigger {
                problem = problem.with_extra("trigger", trigger);
            }
            problem
        }
        OpsError::Validation(message) => ApiProblem::validation(vec![ValidationError {
            field: validation_field(&message).map(str::to_string),
            message,
        }]),
        OpsError::Conflict { expected, actual } => ApiProblem::new(StatusCode::CONFLICT, "conflict", detail)
            .with_extra("expected", expected)
            .with_extra("actual", actual),
        OpsError::Store(err) => store_problem(err),
    }
}

/// `StoreError` → HTTP。`InvalidTransition` は 409、`SQLITE_BUSY` は 503 `db_busy`、その他は 500。
pub(crate) fn store_problem(err: StoreError) -> ApiProblem {
    match &err {
        StoreError::InvalidTransition(t) => ApiProblem::new(StatusCode::CONFLICT, "invalid_transition", err.to_string())
            .with_extra("task_status", t.status)
            .with_extra("kind", t.kind)
            .with_extra("trigger", t.trigger),
        StoreError::Sqlite(e) if is_busy(e) => ApiProblem::db_busy(),
        // ADR-0033 D1: 使用中は 409、組織の検証違反は 422（`OpsError::Validation` と同じ形）。
        StoreError::InUse { .. } => ApiProblem::org_node_in_use(err.to_string()),
        StoreError::Org(_) => ApiProblem::validation(vec![ValidationError {
            field: None,
            message: err.to_string(),
        }]),
        _ => ApiProblem::internal(err.to_string()),
    }
}

pub(crate) fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(failure.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{InvalidTransition, Status, TaskKind};

    #[test]
    fn validation_field_is_inferred_from_task_ops_messages() {
        assert_eq!(
            validation_field(
                "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)"
            ),
            Some("acceptance")
        );
        assert_eq!(validation_field("dependency 01J does not exist"), Some("depends_on"));
        assert_eq!(validation_field("goal must not be blank"), Some("goal"));
        assert_eq!(validation_field("title must not be blank"), Some("title"));
        assert_eq!(validation_field("objective must not be blank"), Some("objective"));
        assert_eq!(validation_field("parent 01J does not exist"), Some("parent"));
        assert_eq!(validation_field("graph has too many nodes"), None);
    }

    #[test]
    fn busy_sqlite_errors_map_to_db_busy_and_others_to_internal() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        );
        let problem = store_problem(StoreError::Sqlite(busy));
        assert_eq!(problem.code(), "db_busy");
        assert_eq!(problem.status, StatusCode::SERVICE_UNAVAILABLE);

        let other = store_problem(StoreError::Invalid("broken".into()));
        assert_eq!(other.code(), "internal");

        let invalid = store_problem(StoreError::InvalidTransition(InvalidTransition {
            status: Status::Done,
            kind: TaskKind::Execute,
            trigger: "approve",
        }));
        assert_eq!(invalid.code(), "invalid_transition");
        assert_eq!(invalid.extra.get("task_status"), Some(&Value::from("done")));
        assert_eq!(invalid.extra.get("kind"), Some(&Value::from("execute")));
        assert_eq!(invalid.extra.get("trigger"), Some(&Value::from("approve")));
    }
}
