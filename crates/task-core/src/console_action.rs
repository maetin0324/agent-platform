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
//!    "milestone": "<id or null>", "assignee": null,
//!    "workspace": {"kind": "remote", "cluster": "<id>", "path": "<remote dir or ~>"}},
//!   {"type": "propose_project", "title": "…", "request": "…", "repos": [...]},
//!   {"type": "add_milestone", "project": "<id>", "title": "…", "description": "…"},
//!   {"type": "ask_human", "text": "…"}
//! ]}
//! ```
//!
//! Phase 98（ADR-0018、実機障害 2026-09-22）: `create_task.workspace` は `WorkspaceSpec` そのもの
//! （`{"kind":"local","path":"…"}` または `{"kind":"remote","cluster":"…","path":"…"}`）。CoS がクラスタ作業
//! （pegasus / sirius / fern03 でのコマンド実行）を`cluster:<id>` を持つノードへ流すときに使う。実行側
//! （`task_ops::actions::create_task_action`）が `cluster` を `[[clusters]]` に照らして検証する。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 1 件の action（ADR-0048 D3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleAction {
    CreateTask {
        #[serde(default)]
        tier: Option<crate::Tier>,
        title: String,
        objective: String,
        #[serde(default, deserialize_with = "lenient_acceptance")]
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
        /// Phase 98（ADR-0018）: クラスタで動く仕事を指すときの作業場所。`{"kind":"remote",
        /// "cluster":"<id>","path":"<クラスタ側のパスか ~>"}` か `{"kind":"local","path":"…"}`。
        /// `cluster` は `[[clusters]]` に存在すること（実行側が検証する）。`Box` は
        /// `clippy::large_enum_variant`（`WorkspaceSpec` を直に持つと `CreateTask` だけ他の variant
        /// より大きく膨らむ）を避けるためだけで、意味は変わらない。
        #[serde(default)]
        workspace: Option<Box<crate::model::WorkspaceSpec>>,
        /// ADR-0069 D3（Phase 114）: 仕事の性質の記述（lane policy の `TaskFeatures` の上書き）。
        /// モデルの選択ではない。書いた軸だけが効く。
        #[serde(default)]
        features: Option<crate::model_policy::TaskFeatureHints>,
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

/// 実機 2026-09-20: CoS は `acceptance` をタスクの実際の形（`{"text": "…", "check": {…}}`）で書くことがあり、
/// 文字列しか受けない型だと action 全体が「invalid type: map, expected a string」で捨てられた（タスクが作られない）。
/// 文字列でも、`text` を持つオブジェクトでも受け、どちらも受け入れ条件の**文**として扱う（判定の方法は実行側が決める）。
fn lenient_acceptance<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Vec<serde_json::Value> = Deserialize::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        match item {
            serde_json::Value::String(text) => out.push(text),
            serde_json::Value::Object(map) => match map.get("text").and_then(|t| t.as_str()) {
                Some(text) => out.push(text.to_string()),
                None => {
                    return Err(serde::de::Error::custom(
                        "acceptance の各要素は文字列か、`text` を持つオブジェクト",
                    ));
                }
            },
            _ => {
                return Err(serde::de::Error::custom(
                    "acceptance の各要素は文字列か、`text` を持つオブジェクト",
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod lenient_tests {
    use super::*;

    #[test]
    fn acceptance_accepts_strings_and_criterion_objects() {
        let json = r#"{"type":"create_task","title":"t","objective":"o",
            "acceptance":["文だけ", {"text":"オブジェクト","check":{"type":"reviewer"}}]}"#;
        let action: ConsoleAction = serde_json::from_str(json).expect("parse");
        let ConsoleAction::CreateTask { acceptance, .. } = action else {
            panic!("create_task")
        };
        assert_eq!(
            acceptance,
            vec!["文だけ".to_string(), "オブジェクト".to_string()]
        );
        let bad =
            r#"{"type":"create_task","title":"t","objective":"o","acceptance":[{"check":{}}]}"#;
        assert!(serde_json::from_str::<ConsoleAction>(bad).is_err());
    }
}
