//! クエリ文字列と path の値の解析。未知のキー・重複・型誤り・ULID でない id は 400 `bad_request`（api.md §1.5）。

use std::collections::HashSet;

use serde::de::DeserializeOwned;
use task_core::{AccountAdapter, Event, TaskId};

use crate::problem::ApiProblem;

pub(crate) struct QueryParams {
    pairs: Vec<(String, String)>,
}

impl QueryParams {
    /// `allowed` 以外のキーがあれば 400。
    pub(crate) fn parse(raw: Option<&str>, allowed: &[&str]) -> Result<Self, ApiProblem> {
        let pairs: Vec<(String, String)> =
            form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
        if let Some((key, _)) = pairs.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
            return Err(ApiProblem::bad_request(format!(
                "unknown query parameter `{key}`"
            )));
        }
        Ok(Self { pairs })
    }

    /// 1 回だけ現れるべきキーの値（重複は 400）。
    pub(crate) fn single(&self, key: &str) -> Result<Option<&str>, ApiProblem> {
        let mut values = self
            .pairs
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str());
        let first = values.next();
        if values.next().is_some() {
            return Err(ApiProblem::bad_request(format!(
                "query parameter `{key}` must not be repeated"
            )));
        }
        Ok(first)
    }

    /// 複数可のキー（`?k=a&k=b` と `?k=a,b` の両方）。空要素は捨てる。
    pub(crate) fn list(&self, key: &str) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(k, _)| k == key)
            .flat_map(|(_, v)| v.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub(crate) fn u64(&self, key: &str) -> Result<Option<u64>, ApiProblem> {
        self.single(key)?
            .map(|v| {
                v.trim().parse::<u64>().map_err(|_| {
                    ApiProblem::bad_request(format!(
                        "query parameter `{key}` must be a non-negative integer"
                    ))
                })
            })
            .transpose()
    }

    pub(crate) fn i64(&self, key: &str) -> Result<Option<i64>, ApiProblem> {
        self.single(key)?
            .map(|v| {
                v.trim().parse::<i64>().map_err(|_| {
                    ApiProblem::bad_request(format!("query parameter `{key}` must be an integer"))
                })
            })
            .transpose()
    }

    pub(crate) fn bool(&self, key: &str) -> Result<Option<bool>, ApiProblem> {
        match self.single(key)? {
            None => Ok(None),
            Some("true" | "1") => Ok(Some(true)),
            Some("false" | "0") => Ok(Some(false)),
            Some(_) => Err(ApiProblem::bad_request(format!(
                "query parameter `{key}` must be true, false, 1 or 0"
            ))),
        }
    }

    pub(crate) fn task_id(&self, key: &str) -> Result<Option<TaskId>, ApiProblem> {
        self.single(key)?.map(parse_task_id).transpose()
    }

    /// `limit`: 省略時 `default`、0 は 400、`max` 超は `max` に丸める。
    pub(crate) fn limit(&self, key: &str, default: usize, max: usize) -> Result<usize, ApiProblem> {
        match self.u64(key)? {
            None => Ok(default),
            Some(0) => Err(ApiProblem::bad_request(format!(
                "query parameter `{key}` must be at least 1"
            ))),
            Some(n) => Ok(usize::try_from(n).unwrap_or(usize::MAX).min(max)),
        }
    }

    /// ADR-0025 D6: `?adapter=` クエリ（省略時 `claude-code`）。`"claude-code"` / `"codex"` 以外は 400。
    pub(crate) fn account_adapter(&self) -> Result<AccountAdapter, ApiProblem> {
        match self.single("adapter")? {
            None => Ok(AccountAdapter::ClaudeCode),
            Some(s) => AccountAdapter::parse(s).ok_or_else(|| {
                ApiProblem::bad_request(format!(
                    "unknown adapter `{s}` (must be claude-code or codex)"
                ))
            }),
        }
    }

    /// `types`: `Event` の `type` 名のカンマ区切り。未知の名前は 400。省略（または空）は `None` = 全て。
    pub(crate) fn event_types(&self) -> Result<Option<HashSet<&'static str>>, ApiProblem> {
        let names = self.list("types");
        if names.is_empty() {
            return Ok(None);
        }
        let mut set = HashSet::new();
        for name in names {
            match EVENT_TYPES.iter().find(|known| **known == name) {
                Some(known) => {
                    set.insert(*known);
                }
                None => {
                    return Err(ApiProblem::bad_request(format!(
                        "unknown event type `{name}`"
                    )));
                }
            }
        }
        Ok(Some(set))
    }
}

pub(crate) fn parse_task_id(value: &str) -> Result<TaskId, ApiProblem> {
    value
        .parse::<TaskId>()
        .map_err(|_| ApiProblem::bad_request(format!("`{value}` is not a task id (ULID)")))
}

/// `snake_case` の列挙（`Status` / `TaskKind`）を serde 表現のまま解析する。
pub(crate) fn parse_snake<T: DeserializeOwned>(what: &str, value: &str) -> Result<T, ApiProblem> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|_| ApiProblem::bad_request(format!("unknown {what} `{value}`")))
}

/// `Event` の serde の `type` 名（`types` クエリの語彙）。
pub(crate) const EVENT_TYPES: [&str; 18] = [
    "created",
    "transitioned",
    "worker_started",
    "worker_progress",
    "artifact_produced",
    "worker_finished",
    "review_verdict",
    "approval_requested",
    "approval_decided",
    "answered",
    "provider_throttled",
    "cluster_unavailable",
    "delegated",
    "question_raised",
    "retried",
    "edited",
    // ADR-0046 D5（Phase 59）: matching が担当を決めた。
    "assigned",
    // ADR-0059 D3（Phase 99）: worktree が切れず `shared` に格下げした。
    "workspace_mode_downgraded",
];

pub(crate) fn event_type_name(event: &Event) -> &'static str {
    match event {
        Event::Created { .. } => "created",
        Event::Transitioned { .. } => "transitioned",
        Event::WorkerStarted { .. } => "worker_started",
        Event::WorkerProgress { .. } => "worker_progress",
        Event::ArtifactProduced { .. } => "artifact_produced",
        Event::WorkerFinished { .. } => "worker_finished",
        Event::ReviewVerdict { .. } => "review_verdict",
        Event::ApprovalRequested => "approval_requested",
        Event::ApprovalDecided { .. } => "approval_decided",
        Event::Answered { .. } => "answered",
        Event::ProviderThrottled { .. } => "provider_throttled",
        Event::ClusterUnavailable { .. } => "cluster_unavailable",
        Event::Delegated { .. } => "delegated",
        Event::QuestionRaised { .. } => "question_raised",
        Event::Retried { .. } => "retried",
        Event::Edited { .. } => "edited",
        Event::Assigned { .. } => "assigned",
        Event::WorkspaceModeDowngraded { .. } => "workspace_mode_downgraded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Status, TaskKind};

    #[test]
    fn unknown_and_repeated_keys_are_rejected() {
        assert!(QueryParams::parse(Some("limit=1&bogus=2"), &["limit"]).is_err());
        let q = QueryParams::parse(Some("limit=1&limit=2"), &["limit"])
            .unwrap_or_else(|_| panic!("parse"));
        assert!(q.single("limit").is_err());
    }

    #[test]
    fn lists_accept_repeated_and_comma_separated_values() {
        let q = QueryParams::parse(Some("status=ready,running&status=done"), &["status"])
            .unwrap_or_else(|_| panic!("parse"));
        assert_eq!(q.list("status"), vec!["ready", "running", "done"]);
        let statuses: Vec<Status> = q
            .list("status")
            .into_iter()
            .map(|s| parse_snake("status", s).unwrap_or_else(|_| panic!("status")))
            .collect();
        assert_eq!(statuses, vec![Status::Ready, Status::Running, Status::Done]);
        assert!(parse_snake::<TaskKind>("kind", "bogus").is_err());
    }

    #[test]
    fn limit_defaults_rejects_zero_and_clamps() {
        let parse = |raw: &str| {
            QueryParams::parse(Some(raw), &["limit"]).unwrap_or_else(|_| panic!("parse"))
        };
        assert_eq!(parse("").limit("limit", 100, 500).ok(), Some(100));
        assert!(parse("limit=0").limit("limit", 100, 500).is_err());
        assert!(parse("limit=-1").limit("limit", 100, 500).is_err());
        assert_eq!(parse("limit=9999").limit("limit", 100, 500).ok(), Some(500));
    }

    #[test]
    fn event_types_are_validated_against_the_event_vocabulary() {
        let q = QueryParams::parse(Some("types=transitioned,worker_finished"), &["types"])
            .unwrap_or_else(|_| panic!("parse"));
        let set = q.event_types().ok().flatten().unwrap_or_default();
        assert!(set.contains("transitioned") && set.contains("worker_finished") && set.len() == 2);
        let bad =
            QueryParams::parse(Some("types=nope"), &["types"]).unwrap_or_else(|_| panic!("parse"));
        assert!(bad.event_types().is_err());
    }
}
