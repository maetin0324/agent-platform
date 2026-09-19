//! 対話（ADR-0033 D4 / Phase 24）。人が組織のノード（＝「人」）に話しかけ、そのノードが返事をする。
//! 純粋なデータ定義と決定的な小さい関数だけを置く（I/O・LLM 呼び出しはしない。ADR-0001 D2 / DESIGN 原則 1）。
//!
//! 「話しかける」は**新しいプロトコルを作らない**: 既存の `tasks` に `kind = execute` の対話用タスクを
//! 1 件作り、その run の `summary` を返事にする。対話由来であることは `Task.conversation`（`json` 列の中の
//! 任意フィールド）で表す（`tasks` の列は増やしていない。ADR-0033 D4 / Phase 24 の指示）。
//! Phase 27（GUI からの依頼 R4）で `messages.task_id` だけを migration 0007 で足し、1 往復とその run を
//! GUI から 1 段で辿れるようにした。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::model::{Task, TaskId};
use crate::org::ProjectId;

/// 対話の 1 行の識別子（ULID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MessageId(#[schemars(with = "String")] pub Ulid);

impl MessageId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for MessageId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 誰が言ったか（ADR-0033 D4）。`user` = 人、`node` = 組織のノード（その run の返事）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Node,
}

impl MessageRole {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Node => "node",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(MessageRole::User),
            "node" => Some(MessageRole::Node),
            _ => None,
        }
    }
}

/// 対話の 1 行（`messages` テーブル。ADR-0033 D4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub id: MessageId,
    /// 話し相手（`org_nodes.id`）。
    pub node_id: String,
    /// 案件（案件に紐づかない雑談なら `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub role: MessageRole,
    pub text: String,
    /// `role = node` のとき、その返事を作った run（`Event::WorkerStarted.run_id`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// この 1 往復を起こした対話用タスク（GUI からの依頼 R4 / Phase 24 の P-74。migration 0007）。
    /// `role = user` の行にも `role = node` の行にも**同じ id** が入る。導入前の行は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
}

/// 分野を持たないノードが対話するときに使う分野（ADR-0033 D4 の「秘書の対話用分野」）。
/// `[[genres]] id = "secretary"` を指す。分野が設定に無ければ、分野なしで run するだけ（決定的）。
pub const CONVERSATION_GENRE: &str = "secretary";

/// 対話用タスクの `title` に使う本文の先頭の字数（ADR-0033 D4 / Phase 24）。
pub const CONVERSATION_TITLE_CHARS: usize = 40;

/// 対話用タスクの `title`（`"対話: <本文の先頭 40 字>"`）。改行と前後の空白は畳んで 1 行にする。
pub fn conversation_title(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let head: String = flat.chars().take(CONVERSATION_TITLE_CHARS).collect();
    format!("対話: {head}")
}

/// そのタスクが対話由来なら、きっかけになった人の発言の id を返す（`Task.conversation`）。
pub fn conversation_origin(task: &Task) -> Option<MessageId> {
    task.conversation
}

/// そのタスクが対話用タスクか。
pub fn is_conversation(task: &Task) -> bool {
    conversation_origin(task).is_some()
}

/// Phase 41（ADR-0038 D1）: 途中目標レビューの対話タスクか。印は**対話の印 + `milestone_id`** の 2 つだけで、
/// 新しい列も新しいフィールドも増やさない（人が話しかける対話 `task_ops::conversation::start` は
/// `milestone_id` を付けないので、この 2 つで決定的に見分けられる）。裏方なので
/// `crate::support_kind` は `"milestone_review"` を返す。
pub fn is_milestone_review(task: &Task) -> bool {
    is_conversation(task) && task.milestone_id.is_some()
}

/// Phase 41（ADR-0038 D1）: レビュー対象の途中目標（レビューの対話タスクでなければ `None`）。
pub fn milestone_review_of(task: &Task) -> Option<crate::org::MilestoneId> {
    if is_milestone_review(task) { task.milestone_id } else { None }
}

/// run が `Error` に終わったときの返事の文面（ADR-0033 D4 / Phase 24）。
pub fn failure_reply(reason: &str) -> String {
    format!("返事できませんでした: {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with(conversation: Option<MessageId>) -> Task {
        use crate::model::{Budget, Status, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec};
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Draft,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: std::path::PathBuf::from("ws"), mode: None },
            budget: Budget { max_turns: 1, max_wall_secs: 1, max_retries: 0 },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation,
        }
    }

    #[test]
    fn the_title_is_the_first_40_characters_on_one_line() {
        let text = "Pluvio を基盤に用いた新たな研究テーマの模索、検証をしたい。\n まずは関連研究を洗ってほしい。";
        let title = conversation_title(text);
        assert!(title.starts_with("対話: "));
        assert!(!title.contains('\n'));
        assert_eq!(title.chars().count(), "対話: ".chars().count() + CONVERSATION_TITLE_CHARS);
        // 40 字に満たない本文はそのまま。
        assert_eq!(conversation_title("短い用件"), "対話: 短い用件");
        assert_eq!(conversation_title("  空白  を   畳む "), "対話: 空白 を 畳む");
    }

    /// 対話由来の印は `Task.conversation`（DB の列は増やさない）。導入前の JSON もそのまま読める。
    #[test]
    fn the_conversation_marker_round_trips_and_old_tasks_have_none() {
        let id = MessageId::new();
        let task = task_with(Some(id));
        assert!(is_conversation(&task));
        assert_eq!(conversation_origin(&task), Some(id));
        assert!(!is_conversation(&task_with(None)));

        let json = serde_json::to_string(&task).expect("serialize");
        assert!(json.contains(&id.to_string()));
        let back: Task = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.conversation, Some(id));
        // 導入前のタスク（`conversation` が無い JSON）は `None` として読める。
        let old = serde_json::to_string(&task_with(None)).expect("serialize");
        assert!(!old.contains("conversation"));
        assert_eq!(serde_json::from_str::<Task>(&old).expect("deserialize").conversation, None);
    }

    #[test]
    fn the_role_and_the_failure_reply_have_fixed_spellings() {
        assert_eq!(MessageRole::User.as_str(), "user");
        assert_eq!(MessageRole::parse("node"), Some(MessageRole::Node));
        assert_eq!(MessageRole::parse("bogus"), None);
        assert_eq!(failure_reply("タイムアウト"), "返事できませんでした: タイムアウト");
    }
}
