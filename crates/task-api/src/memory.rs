//! `GET /org/{id}/memory?project=`（読み取り。ADR-0033 D6 の記憶を人が確認するための窓口。
//! GUI 監査対応 Phase 29 / H3）。
//!
//! ノードの長期記憶（`notes.md`）と、指定した案件の引き出し（`projects/<project_id>.md`）を
//! **全文**で返す（前置き用の 8,000 字カットとは別。人が読む画面なので上限は切らない）。
//! **書き込み API は無い**: 記憶はワーカーが run の後に書く（ADR-0033 D6）。人が直したければファイルを
//! 直接編集する。この応答がパスを返すのはそのため。`[memory]` が未設定なら 409 `memory_unavailable`。
//! 知らないノードは 404。

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::TaskStore;

use crate::handlers::{ApiResult, Params, json_response};
use crate::problem::{ApiProblem, store_problem};
use crate::query::QueryParams;
use crate::state::ApiState;

/// `GET /org/{id}/memory` の応答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryView {
    /// `notes.md` の全文（無ければ空文字列）。
    pub notes: String,
    /// `?project=` を指定したときだけ `Some`（その引き出し `projects/<project_id>.md` の全文、無ければ
    /// 空文字列）。指定しなければ `null`。
    pub project: Option<String>,
    /// `notes.md` の絶対パス（人がファイルを直接編集するため）。
    pub notes_path: String,
    /// `?project=` を指定したときだけ `Some`。指定しなければ `null`。
    pub project_path: Option<String>,
}

pub(crate) async fn get_memory(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    // `project` はメモリファイルの置き場所を選ぶ生の文字列（`projects` 表の存在確認はしない。記憶の
    // 引き出しはファイルが正で、`projects` 行が消えても読めてよいため）。空文字列は「指定なし」扱い。
    let query = QueryParams::parse(raw.as_deref(), &["project"])?;
    let project_id = query
        .single("project")?
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let Some(dir) = state.inner.memory_dir.clone() else {
        return Err(ApiProblem::memory_unavailable());
    };
    let view = state
        .blocking(move |store| {
            if store.org_get(&id).map_err(store_problem)?.is_none() {
                return Err(ApiProblem::org_node_not_found(&id));
            }
            Ok(task_ops::memory::read_memory(
                &dir,
                &id,
                project_id.as_deref(),
            ))
        })
        .await?;
    Ok(json_response(
        StatusCode::OK,
        &MemoryView {
            notes: view.notes,
            project: view.project,
            notes_path: view.notes_path.to_string_lossy().into_owned(),
            project_path: view.project_path.map(|p| p.to_string_lossy().into_owned()),
        },
    ))
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new().route("/api/v1/org/{id}/memory", axum::routing::get(get_memory))
}
