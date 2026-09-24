//! Phase 82（ADR-0056 D3 続き）: skills を GUI から見る・作る・mount する。
//!
//! - `GET  /skills` — KB の `skills/` の一覧（name / description / updated / mounted_by）。読み取り
//! - `GET  /skills/{name}` — `SKILL.md` 本文と付属ファイルの一覧。読み取り
//! - `PUT  /skills/{name}` — **管理系**。`task_ops::knowledge::skills_put` と同じ検証（frontmatter の
//!   `name` / `description` 必須）。celeris-mcp の `skills_put` ツールと**同じ関数**を呼ぶ
//! - `DELETE /skills/{name}` — **管理系**。どこかの組織ノードに mount されていれば 409 `skill_mounted`
//! - `POST /org/{id}/skills` `{skill}` / `DELETE /org/{id}/skills/{skill}` — **管理系**。mount / unmount。
//!   celeris-mcp の `org_mount_skill`/`org_unmount_skill` と**同じ** `task_ops::knowledge::set_skill_mount`
//!   を呼ぶので挙動は同一（ADR-0056 D2/D3）
//!
//! `mounted_by` は「継いだ後」（`EffectiveProfile.skills_mounts`）で判定する: 親ノードで mount すれば
//! 子ノードにもそこに現れる。組織の木を GUI が再計算しない規律（ADR-0046 D1）に合わせ、平坦化は
//! `task_core::resolve_profile` に任せる。

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{OrgNode, TaskStore};
use task_ops::knowledge::{self as ops_kb, SkillError};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, load_org_node, no_query, read_json};
use crate::knowledge::{require_kb, root_of};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, store_problem};
use crate::state::ApiState;
use crate::types::ValidationError;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new()
        .route("/api/v1/skills", get(list_skills))
        .route(
            "/api/v1/skills/{name}",
            get(get_skill).put(put_skill).delete(delete_skill),
        )
        .route("/api/v1/org/{id}/skills", post(mount_skill))
        .route(
            "/api/v1/org/{id}/skills/{skill}",
            axum::routing::delete(unmount_skill),
        )
}

// ---------------------------------------------------------------------------
// 応答の型（`docs/gui/api.md` §3.112〜）
// ---------------------------------------------------------------------------

/// `GET /skills` の 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillSummaryView {
    pub name: String,
    pub description: String,
    /// 最後のコミットの時刻（RFC 3339。無ければ省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// この skill を（継承も含め）mount している組織ノードの id。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounted_by: Vec<String>,
}

/// `GET /skills`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillList {
    /// KB の根（絶対パス）。
    pub root: String,
    /// `celerisctl knowledge init` が済んでいるか。偽なら `items` は空。
    pub initialized: bool,
    pub items: Vec<SkillSummaryView>,
}

/// `GET /skills/{name}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillDetailView {
    pub name: String,
    /// `SKILL.md` の中身（frontmatter を含む）。
    pub skill_md: String,
    /// 同じディレクトリの付属ファイル（相対パス。`SKILL.md` 自身は含まない）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounted_by: Vec<String>,
}

/// `PUT /skills/{name}` の付属ファイル 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillFileBody {
    pub path: String,
    pub content: String,
}

/// `PUT /skills/{name}` の本文。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillPutBody {
    /// `SKILL.md` の中身（frontmatter を含む。`name` / `description` 必須、`name` はこの URL の
    /// `{name}` と一致していること）。
    pub skill_md: String,
    #[serde(default)]
    pub files: Vec<SkillFileBody>,
}

/// `PUT /skills/{name}` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillPutResult {
    /// `skills/<name>/SKILL.md`。
    pub path: String,
}

/// `POST /org/{id}/skills` の本文。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgSkillMountBody {
    pub skill: String,
}

// ---------------------------------------------------------------------------
// 共通
// ---------------------------------------------------------------------------

fn skill_not_found(name: &str) -> ApiProblem {
    ApiProblem::new(
        StatusCode::NOT_FOUND,
        "skill_not_found",
        format!("skill not found: {name}"),
    )
}

fn skill_mounted(name: &str, nodes: &[String]) -> ApiProblem {
    ApiProblem::new(
        StatusCode::CONFLICT,
        "skill_mounted",
        format!(
            "skill {name:?} is mounted by: {}（先に外してから消す）",
            nodes.join(", ")
        ),
    )
}

/// [`SkillError`] → [`ApiProblem`]。`Failed`（I/O 失敗）だけ 500、それ以外は入力の誤りなので 422。
fn skill_error_to_problem(e: SkillError) -> ApiProblem {
    match e {
        SkillError::Failed(detail) => ApiProblem::internal(detail),
        other => ApiProblem::validation(vec![ValidationError {
            field: None,
            message: other.to_string(),
        }]),
    }
}

/// その skill を（継承も含め）mount している組織ノードの id を、全ノード分まとめて 1 度に計算する
/// （`GET /skills` が N 件でも `org_list` は 1 回だけ）。
fn mounted_by_map(nodes: &[OrgNode]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for node in nodes {
        let effective = task_core::resolve_profile(nodes, &node.id);
        for skill in effective.skills_mounts {
            map.entry(skill).or_default().push(node.id.clone());
        }
    }
    map
}

fn skill_updated(root: &std::path::Path, name: &str) -> Option<String> {
    ops_kb::history(root, &format!("skills/{name}/SKILL.md"))
        .into_iter()
        .next()
        .map(|c| c.at)
}

// ---------------------------------------------------------------------------
// GET /skills
// ---------------------------------------------------------------------------

async fn list_skills(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let root = root_of(&state)?;
    let view = state
        .blocking(move |store| {
            let initialized = ops_kb::exists(&root);
            if !initialized {
                return Ok(SkillList {
                    root: root.display().to_string(),
                    initialized,
                    items: Vec::new(),
                });
            }
            let nodes = store.org_list().map_err(store_problem)?;
            let by = mounted_by_map(&nodes);
            let items: Vec<SkillSummaryView> = ops_kb::skills_list(&root)
                .into_iter()
                .map(|s| SkillSummaryView {
                    updated: skill_updated(&root, &s.name),
                    mounted_by: by.get(&s.name).cloned().unwrap_or_default(),
                    name: s.name,
                    description: s.description,
                })
                .collect();
            Ok(SkillList {
                root: root.display().to_string(),
                initialized,
                items,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// GET /skills/{name}
// ---------------------------------------------------------------------------

async fn get_skill(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(name): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let root = root_of(&state)?;
    let view = state
        .blocking(move |store| {
            let detail = ops_kb::skills_get(&root, &name).ok_or_else(|| skill_not_found(&name))?;
            let nodes = store.org_list().map_err(store_problem)?;
            let mounted_by = mounted_by_map(&nodes).remove(&name).unwrap_or_default();
            let updated = skill_updated(&root, &name);
            Ok(SkillDetailView {
                name: detail.name,
                skill_md: detail.skill_md,
                files: detail.files,
                updated,
                mounted_by,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// PUT /skills/{name}（管理系）
// ---------------------------------------------------------------------------

async fn put_skill(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(name): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let request: SkillPutBody = read_json(body, false).await?;
    let root = root_of(&state)?;
    let result = state
        .blocking(move |_| {
            require_kb(&root)?;
            let files: Vec<(String, String)> = request
                .files
                .into_iter()
                .map(|f| (f.path, f.content))
                .collect();
            // GUI からの書き込みは人の操作だが、`source` は「どこから来たか」を frontmatter に残す
            // ADR-0056 D3 の趣旨（MCP は `mcp:<client_id>`）に合わせ、GUI は `gui` を残す（冪等: 既に
            // `source:` があれば `skills_put` が触らない）。
            match ops_kb::skills_put(&root, &name, &request.skill_md, &files, Some("gui")) {
                Ok(path) => {
                    tracing::info!(who = "admin", op = "skill_put", name = %name, "admin: skill written");
                    Ok(SkillPutResult { path })
                }
                Err(e) => Err(skill_error_to_problem(e)),
            }
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

// ---------------------------------------------------------------------------
// DELETE /skills/{name}（管理系）
// ---------------------------------------------------------------------------

async fn delete_skill(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(name): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let root = root_of(&state)?;
    state
        .blocking(move |store| {
            require_kb(&root)?;
            if ops_kb::skills_get(&root, &name).is_none() {
                return Err(skill_not_found(&name));
            }
            let nodes = store.org_list().map_err(store_problem)?;
            let mounted_by = mounted_by_map(&nodes).remove(&name).unwrap_or_default();
            if !mounted_by.is_empty() {
                return Err(skill_mounted(&name, &mounted_by));
            }
            match ops_kb::skills_delete(&root, &name) {
                Ok(_) => {
                    tracing::info!(who = "admin", op = "skill_delete", name = %name, "admin: skill deleted");
                    Ok(())
                }
                Err(e) => Err(skill_error_to_problem(e)),
            }
        })
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// POST /org/{id}/skills、DELETE /org/{id}/skills/{skill}（管理系）
// ---------------------------------------------------------------------------

async fn mount_skill(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let request: OrgSkillMountBody = read_json(body, false).await?;
    let skill = request.skill;
    let skill_for_log = skill.clone();
    let node = state
        .blocking(move |store| -> Result<OrgNode, ApiProblem> {
            let mut node = load_org_node(store, &id)?;
            ops_kb::set_skill_mount(&mut node.profile.skills_mounts, &skill, true)
                .map_err(skill_error_to_problem)?;
            node.updated_at = OffsetDateTime::now_utc();
            store.org_upsert(&node).map_err(store_problem)
        })
        .await?;
    tracing::info!(who = "admin", op = "org_skill_mount", org_id = %node.id, skill = %skill_for_log, "admin: skill mounted");
    Ok(json_response(StatusCode::OK, &node))
}

async fn unmount_skill(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params((id, skill)): Params<(String, String)>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let node = state
        .blocking(move |store| -> Result<OrgNode, ApiProblem> {
            let mut node = load_org_node(store, &id)?;
            ops_kb::set_skill_mount(&mut node.profile.skills_mounts, &skill, false)
                .map_err(skill_error_to_problem)?;
            node.updated_at = OffsetDateTime::now_utc();
            store.org_upsert(&node).map_err(store_problem)
        })
        .await?;
    tracing::info!(who = "admin", op = "org_skill_unmount", org_id = %node.id, "admin: skill unmounted");
    Ok(json_response(StatusCode::OK, &node))
}
