//! API v1 の JSON Schema（`docs/gui/api.md` §7、ADR-0013 D8）。ルートは `ApiV1Schema`（1 フィールド = 1 公開型）。
//! 生成物は `docs/api/v1/api-v1.schema.json` にコミットし、`committed_schema_matches_generated` で一致を確かめる
//! （`UPDATE_SCHEMA=1 cargo test -p task-api` で再生成）。

use schemars::JsonSchema;
use task_core::{EventRow, Task};
use task_ops::add::NewTaskSpec;
use task_ops::daemon::DaemonSnapshot;
use task_ops::gate::TransitionResult;
use task_ops::graph::Graph;
use task_ops::inbox::Inbox;
use task_ops::plan::NewPlanSpec;
use task_ops::replay::ReplayReport;
use task_ops::view::{TaskDetail, TaskList};

use crate::types::{
    AccountCheckResponse, AccountList, AccountLoginResult, AccountLoginStart, AccountView, AnswerBody, ArtifactList,
    CancelBody, Clusters, ConfigView, DaemonView, DecisionBody, EventsPage, Health, Problem, ProviderCheckResponse,
    ProviderConfigView, Providers, ReloadResult, RunList, StreamHeartbeat, StreamHello, StreamReset,
};

/// コミット済みのスキーマ（`GET /schema` の本体）。
pub const API_V1_SCHEMA_JSON: &str = include_str!("../../../docs/api/v1/api-v1.schema.json");

/// スキーマ生成のルート。
#[derive(JsonSchema)]
pub struct ApiV1Schema {
    pub health: Health,
    pub problem: Problem,
    pub inbox: Inbox,
    pub task_list: TaskList,
    pub task: Task,
    pub task_detail: TaskDetail,
    pub events_page: EventsPage,
    pub run_list: RunList,
    pub artifact_list: ArtifactList,
    pub graph: Graph,
    pub new_task: NewTaskSpec,
    pub new_plan: NewPlanSpec,
    pub decision: DecisionBody,
    pub answer: AnswerBody,
    pub cancel: CancelBody,
    pub transition_result: TransitionResult,
    pub replay_report: ReplayReport,
    pub providers: Providers,
    /// `POST /api/v1/providers` と `PATCH /api/v1/providers/{id}` の応答（ADR-0017）。
    pub provider_config: ProviderConfigView,
    pub reload: ReloadResult,
    pub provider_check: ProviderCheckResponse,
    pub clusters: Clusters,
    /// Phase 13（ADR-0024）: Claude アカウントのプール。
    pub account_list: AccountList,
    pub account: AccountView,
    pub account_check: AccountCheckResponse,
    pub account_login_start: AccountLoginStart,
    pub account_login_result: AccountLoginResult,
    pub daemon: DaemonView,
    pub config: ConfigView,
    pub stream_hello: StreamHello,
    pub stream_event: EventRow,
    pub stream_daemon: DaemonSnapshot,
    pub stream_heartbeat: StreamHeartbeat,
    pub stream_reset: StreamReset,
}

/// 生成したスキーマ（`serde_json::Value`）。
pub fn api_v1_schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(ApiV1Schema);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

/// コミットするファイルの内容（`to_string_pretty` + 末尾改行 1 つ）。
pub fn api_v1_schema_json() -> String {
    let mut text = serde_json::to_string_pretty(&api_v1_schema_value()).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_schema_matches_generated() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/v1/api-v1.schema.json");
        let generated = api_v1_schema_json();
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap_or_else(|e| panic!("write {path}: {e}"));
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(
            committed, generated,
            "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-api`"
        );
    }

    #[test]
    fn schema_uses_defs_once_for_shared_types() {
        let value = api_v1_schema_value();
        let defs = value.get("$defs").and_then(|d| d.as_object()).cloned().unwrap_or_default();
        for name in ["Task", "Event", "EventRow", "DaemonSnapshot", "Status"] {
            assert!(defs.contains_key(name), "missing $defs/{name}");
        }
        assert!(!value.to_string().contains("$dynamicRef"));
    }
}
