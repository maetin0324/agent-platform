//! ADR-0048 D3（Phase 60b）: CoS の結果ファイルが宣言する `actions` の**形**だけを持つ。
//!
//! 読む（`task_worker::result_report`。ファイル I/O）のも実行する（`task_ops::actions`。ストアの
//! 読み書き）のも別の crate なので、両方が依存できる `task-core` にこの形だけを置く
//! （ADR-0034 D7 / ADR-0038 D1 の宣言的フィールドと同じ流儀）。判断も I/O もここには無い。
//!
//! ```json
//! {"summary": "…", "actions": [
//!   {"type": "create_task", "title": "…", "objective": "…", "acceptance": [...], "harness": "coding",
//!    "skills": ["rust"], "mode": "prototype", "repos": ["agent-platform"], "project": "<id or null>",
//!    "milestone": "<id or null>", "assignee": null},
//!   {"type": "propose_project", "title": "…", "request": "…", "repos": [...]},
//!   {"type": "add_milestone", "project": "<id>", "title": "…", "description": "…"},
//!   {"type": "ask_human", "text": "…"}
//! ]}
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 1 件の action（ADR-0048 D3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleAction {
    CreateTask {
        title: String,
        objective: String,
        #[serde(default)]
        acceptance: Vec<String>,
        #[serde(default)]
        harness: Option<String>,
        #[serde(default)]
        skills: Vec<String>,
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        repos: Vec<String>,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        milestone: Option<String>,
        #[serde(default)]
        assignee: Option<String>,
    },
    ProposeProject {
        title: String,
        request: String,
        #[serde(default)]
        repos: Vec<String>,
    },
    AddMilestone {
        project: String,
        title: String,
        #[serde(default)]
        description: String,
    },
    AskHuman {
        text: String,
    },
}

impl ConsoleAction {
    /// 人が読む種類の名前（`Message.metadata` / ログに使う）。
    pub fn kind(&self) -> &'static str {
        match self {
            ConsoleAction::CreateTask { .. } => "create_task",
            ConsoleAction::ProposeProject { .. } => "propose_project",
            ConsoleAction::AddMilestone { .. } => "add_milestone",
            ConsoleAction::AskHuman { .. } => "ask_human",
        }
    }
}
