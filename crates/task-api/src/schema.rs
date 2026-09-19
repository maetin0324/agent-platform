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
use task_ops::retry::RetryResult;
use task_ops::view::{TaskDetail, TaskList};

use crate::approvals::{ApprovalDecideBody, ApprovalDecideResult, ApprovalList, StandingRuleCreateBody, StandingRuleList};
use crate::conversation::{MessageAccepted, MessageList, MessagePostBody};
use crate::memory::MemoryView;
use crate::milestones::{MilestoneDecideBody, MilestoneDecided};
use crate::project_plan::{ProjectPlanAccepted, ProjectPlanBody};
use crate::types::{
    AccountCheckResponse, AccountList, AccountLoginResult, AccountLoginStart, AccountView, AnswerBody, ArtifactList,
    CancelBody, ClusterConnectResult, ClusterConnectStart, Clusters, ConfigView, DaemonView, DecisionBody,
    EventsPage, Health, MilestoneCreateBody, MilestonePatchBody, OrgCreateBody, OrgList, OrgPatchBody, Problem,
    ProjectCreateBody, ProjectDetail, ProjectList, ProjectPatchBody, ProviderCheckResponse, ProviderConfigView,
    Providers, ReleasePromoteAccepted, Releases, ReloadResult, RetryBody, RunList, SecretList, SecretPutResult,
    StreamHeartbeat, StreamHello, StreamReset,
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
    /// Phase 31（実機の事故、2026-09-18）: `POST /tasks/{id}/retry` の要求本文と応答。
    pub retry: RetryBody,
    pub retry_result: RetryResult,
    pub replay_report: ReplayReport,
    pub providers: Providers,
    /// `POST /api/v1/providers` と `PATCH /api/v1/providers/{id}` の応答（ADR-0017）。
    pub provider_config: ProviderConfigView,
    pub reload: ReloadResult,
    pub provider_check: ProviderCheckResponse,
    pub clusters: Clusters,
    /// ADR-0032 D5: `POST /clusters/{id}/connect` と `POST /clusters/{id}/connect/code` の応答。
    pub cluster_connect_start: ClusterConnectStart,
    pub cluster_connect_result: ClusterConnectResult,
    /// Phase 20（ADR-0030）: GUI から預かる秘密（API キー等）。`GET /secrets` と `PUT /secrets/{id}` の応答。
    pub secrets: SecretList,
    pub secret_put: SecretPutResult,
    /// Phase 13（ADR-0024）: Claude アカウントのプール。
    pub account_list: AccountList,
    pub account: AccountView,
    pub account_check: AccountCheckResponse,
    pub account_login_start: AccountLoginStart,
    pub account_login_result: AccountLoginResult,
    /// Phase 23（ADR-0033 D1）: 組織（一つ、役割の木）。
    pub org_list: OrgList,
    pub org_create: OrgCreateBody,
    pub org_patch: OrgPatchBody,
    /// Phase 23（ADR-0033 D2）: 案件と途中目標。
    pub project_list: ProjectList,
    pub project_create: ProjectCreateBody,
    pub project_patch: ProjectPatchBody,
    pub project_detail: ProjectDetail,
    pub milestone_create: MilestoneCreateBody,
    pub milestone_patch: MilestonePatchBody,
    /// Phase 41（ADR-0038 D2）: 途中目標の判定（`POST /milestones/{id}/decide`）。
    pub milestone_decide: MilestoneDecideBody,
    pub milestone_decided: MilestoneDecided,
    /// GUI 監査対応 Phase 29（ADR-0033 D4 追記）: 分解を起こす（`POST /projects/{id}/plan`）。
    pub project_plan: ProjectPlanBody,
    pub project_plan_accepted: ProjectPlanAccepted,
    /// GUI 監査対応 Phase 29 / H3（ADR-0033 D6）: 記憶を読む（`GET /org/{id}/memory`）。
    pub memory: MemoryView,
    /// Phase 25（ADR-0033 D3）: 報告（生成は決定的、圧縮は別 run）。
    pub report_list: crate::reports::ReportList,
    pub report_detail: crate::reports::ReportDetail,
    pub reports_read: crate::reports::ReportsReadBody,
    pub reports_read_result: crate::reports::ReportsReadResult,
    pub reports_notified: crate::reports::ReportsNotifiedResult,
    /// Phase 24（ADR-0033 D4）: 対話（`POST /org/{id}/messages` と `GET /org/{id}/messages`）。
    pub message_post: MessagePostBody,
    pub message_accepted: MessageAccepted,
    pub message_list: MessageList,
    /// Phase 26（ADR-0033 D5）: 認可（`GET /approvals` と `POST /approvals/{id}/decide`）。
    pub approval_list: ApprovalList,
    pub approval_decide: ApprovalDecideBody,
    pub approval_decide_result: ApprovalDecideResult,
    /// Phase 26（ADR-0033 D5）: 永続の認可（`GET /standing-rules` と `POST /standing-rules`）。
    pub standing_rule_list: StandingRuleList,
    pub standing_rule_create: StandingRuleCreateBody,
    /// Phase 39（ADR-0037 D4）: 通知（Discord）。`GET /notify` と `POST /notify/test` の応答。
    pub notify: crate::notify::NotifyView,
    pub notify_test: crate::notify::NotifyTestResult,
    /// Phase 48（ADR-0040 D6）: リリース。`GET /releases` と `POST /releases/{sha12}/promote` の応答。
    pub releases: Releases,
    pub release_promote: ReleasePromoteAccepted,
    /// Phase 52（ADR-0043 D1 / D6）: 案件のリポジトリと、タスクの作業ツリーの閲覧。
    pub repo_list: crate::types::RepoList,
    pub repo_create: crate::types::RepoCreateBody,
    pub repo_patch: crate::types::RepoPatchBody,
    pub tree: crate::types::TreeView,
    pub tree_file: crate::types::TreeFileView,
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
