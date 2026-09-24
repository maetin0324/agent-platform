//! ADR-0056 D2: `org_list` / `org_get` / `org_create_node` / `org_mount_skill` / `org_unmount_skill`。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    EffectiveProfile, McpScope, NodeSessionStore, OrgKind, OrgNode, Profile, SessionKind, TaskStore,
};
use time::OffsetDateTime;

use super::{ToolDef, ToolError, ToolOutput, schema};
use crate::auth::AuthedClient;
use crate::state::McpState;

// ---- org_list ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OrgNodeSummary {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub kind: OrgKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_default: Option<String>,
    pub has_continuation_session: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListOutput {
    pub items: Vec<OrgNodeSummary>,
}

async fn list_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    _args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let items = state
        .blocking(move |store| -> Result<Vec<OrgNodeSummary>, ToolError> {
            let nodes = store
                .org_list()
                .map_err(|e| ToolError::internal(e.to_string()))?;
            let mut out = Vec::with_capacity(nodes.len());
            for node in &nodes {
                let effective = task_core::resolve_profile(&nodes, &node.id);
                let has_session = match node.kind {
                    OrgKind::Secretary => store
                        .node_session_active(&node.id, SessionKind::Conversation, None)
                        .map_err(|e| ToolError::internal(e.to_string()))?
                        .is_some(),
                    OrgKind::Department => store
                        .node_session_active(&node.id, SessionKind::Lead, None)
                        .map_err(|e| ToolError::internal(e.to_string()))?
                        .is_some(),
                    OrgKind::Section => false,
                };
                out.push(OrgNodeSummary {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    parent_id: node.parent_id.clone(),
                    kind: node.kind,
                    skills: effective.skills,
                    harness_default: effective.harness_default,
                    has_continuation_session: has_session,
                });
            }
            Ok(out)
        })
        .await?;
    ToolOutput::from_serialize(&ListOutput { items })
}

fn list_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(list_impl(state, client, args))
}

pub fn list_def() -> ToolDef {
    ToolDef {
        name: "org_list",
        description: "組織の木（id / name / parent / kind / skills / harness の既定 / 継続セッションの有無）。",
        scope: McpScope::OrgRead,
        input_schema: schema::<ListArgs>,
        call: list_call,
    }
}

// ---- org_get ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    pub node_id: String,
}

async fn get_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: GetArgs =
        serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let profile = state
        .blocking(
            move |store| -> Result<Option<EffectiveProfile>, ToolError> {
                let nodes = store
                    .org_list()
                    .map_err(|e| ToolError::internal(e.to_string()))?;
                if !nodes.iter().any(|n| n.id == args.node_id) {
                    return Ok(None);
                }
                Ok(Some(task_core::resolve_profile(&nodes, &args.node_id)))
            },
        )
        .await?;
    match profile {
        Some(p) => ToolOutput::from_serialize(&p),
        None => Err(ToolError::not_found("org node was not found")),
    }
}

fn get_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(get_impl(state, client, args))
}

pub fn get_def() -> ToolDef {
    ToolDef {
        name: "org_get",
        description: "実効 profile（EffectiveProfile。skills_mounts を含む）。",
        scope: McpScope::OrgRead,
        input_schema: schema::<GetArgs>,
        call: get_call,
    }
}

// ---- org_create_node ----

/// ADR-0056 D2: `Profile` の部分集合。`tools` / `permissions` / `review` は**受け取っても無視**する
/// （外から触れない。ADR-0056 D6）。
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileInput {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub knowledge: Vec<task_core::KnowledgeMount>,
    #[serde(default)]
    pub skills_mounts: Vec<String>,
    #[serde(default)]
    pub harnesses: task_core::HarnessPrefs,
    #[serde(default)]
    pub model: task_core::ModelPrefs,
    #[serde(default)]
    pub policy: Vec<String>,
    #[serde(default)]
    pub run: Option<task_core::ProfileRun>,
    /// 受け取っても無視する（ADR-0056 D6）。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 受け取っても無視する（ADR-0056 D6）。
    #[serde(default)]
    pub permissions: serde_json::Value,
    /// 受け取っても無視する（ADR-0056 D6）。
    #[serde(default)]
    pub review: serde_json::Value,
}

impl ProfileInput {
    /// `tools` / `permissions` / `review` を落とした `Profile`（ADR-0056 D2: `org_create_node` の
    /// 「`tools` と `permissions` は外からは触れない」）。
    fn into_profile(self) -> Profile {
        Profile {
            budget: Default::default(),
            skills: self.skills,
            knowledge: self.knowledge,
            skills_mounts: self.skills_mounts,
            harnesses: self.harnesses,
            tools: Vec::new(),
            deny_tools: Vec::new(),
            run: self.run,
            model: self.model,
            policy: self.policy,
            review: Default::default(),
            permissions: Default::default(),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateNodeArgs {
    pub parent_id: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub profile: Option<ProfileInput>,
}

async fn create_node_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: CreateNodeArgs =
        serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let profile = args
        .profile
        .map(ProfileInput::into_profile)
        .unwrap_or_default();
    let node = state
        .blocking(move |store| -> Result<OrgNode, ToolError> {
            let nodes = store
                .org_list()
                .map_err(|e| ToolError::internal(e.to_string()))?;
            let Some(parent) = nodes.iter().find(|n| n.id == args.parent_id) else {
                return Err(ToolError::invalid_params(format!(
                    "parent {:?} does not exist",
                    args.parent_id
                )));
            };
            let kind = match parent.kind {
                OrgKind::Secretary => OrgKind::Department,
                OrgKind::Department => OrgKind::Section,
                OrgKind::Section => {
                    return Err(ToolError::invalid_params(
                        "a section cannot have child nodes",
                    ));
                }
            };
            if nodes.iter().any(|n| n.id == args.id) {
                return Err(ToolError::invalid_params(format!(
                    "org node {:?} already exists",
                    args.id
                )));
            }
            let now = OffsetDateTime::now_utc();
            let node = OrgNode {
                id: args.id,
                parent_id: Some(args.parent_id),
                name: args.name,
                kind,
                genre: None,
                brief: String::new(),
                profile,
                position: 0,
                created_at: now,
                updated_at: now,
            };
            task_core::validate_upsert(&nodes, &node)
                .map_err(|e| ToolError::invalid_params(e.to_string()))?;
            task_core::validate_profile(&node.profile, &[])
                .map_err(|e| ToolError::invalid_params(e.to_string()))?;
            store
                .org_upsert(&node)
                .map_err(|e| ToolError::internal(e.to_string()))
        })
        .await?;
    ToolOutput::from_serialize(&node)
}

fn create_node_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(create_node_impl(state, client, args))
}

pub fn create_node_def() -> ToolDef {
    ToolDef {
        name: "org_create_node",
        description: "子ノードを作る（profile は skills / harnesses / model / policy / knowledge / skills_mounts のみ。tools と permissions は外からは触れない）。",
        scope: McpScope::OrgWrite,
        input_schema: schema::<CreateNodeArgs>,
        call: create_node_call,
    }
}

// ---- org_mount_skill / org_unmount_skill ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MountArgs {
    pub node_id: String,
    pub skill: String,
}

async fn mount_common(
    state: &Arc<McpState>,
    args: serde_json::Value,
    mount: bool,
) -> Result<ToolOutput, ToolError> {
    let args: MountArgs =
        serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    if !task_core::knowledge::is_valid_skill_name(&args.skill) {
        return Err(ToolError::invalid_params(format!(
            "{:?} must match [a-z0-9-] (1..=64 chars)",
            args.skill
        )));
    }
    let node = state
        .blocking(move |store| -> Result<OrgNode, ToolError> {
            let Some(mut node) = store
                .org_get(&args.node_id)
                .map_err(|e| ToolError::internal(e.to_string()))?
            else {
                return Err(ToolError::not_found(format!(
                    "org node {:?} was not found",
                    args.node_id
                )));
            };
            // Phase 82（ADR-0056 D3 続き）: task-api の `POST/DELETE /org/{id}/skills…` と**同じ**
            // task-ops 関数を呼ぶ（挙動が食い違わないようにするため）。名前は呼び出し元で検証済みなので
            // ここで失敗することは無いが、`?` で素直に伝える。
            task_ops::knowledge::set_skill_mount(
                &mut node.profile.skills_mounts,
                &args.skill,
                mount,
            )
            .map_err(|e| ToolError::invalid_params(e.to_string()))?;
            node.updated_at = OffsetDateTime::now_utc();
            store
                .org_upsert(&node)
                .map_err(|e| ToolError::internal(e.to_string()))
        })
        .await?;
    ToolOutput::from_serialize(&node)
}

fn mount_skill_impl<'a>(
    state: &'a Arc<McpState>,
    _client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(mount_common(state, args, true))
}

pub fn mount_skill_def() -> ToolDef {
    ToolDef {
        name: "org_mount_skill",
        description: "そのノードの profile に skill mount を足す。",
        scope: McpScope::OrgWrite,
        input_schema: schema::<MountArgs>,
        call: mount_skill_impl,
    }
}

fn unmount_skill_impl<'a>(
    state: &'a Arc<McpState>,
    _client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(mount_common(state, args, false))
}

pub fn unmount_skill_def() -> ToolDef {
    ToolDef {
        name: "org_unmount_skill",
        description: "そのノードの profile から skill mount を外す。",
        scope: McpScope::OrgWrite,
        input_schema: schema::<MountArgs>,
        call: unmount_skill_impl,
    }
}
