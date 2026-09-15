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
