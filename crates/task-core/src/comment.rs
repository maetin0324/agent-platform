//! ADR-0044 D2（Phase 53）: タスク単位のコメント。
//!
//! `progress`（`Event::WorkerProgress`）と違って**残る**記録で、人・組織の「人」・taskd 自身の 3 者が書く。
//! 純粋なデータ定義だけを置く（表の SQL は `store.rs`、効き方の判断は `task-ops::comment`）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::model::TaskId;

/// コメントの一意識別子（ULID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CommentId(#[schemars(with = "String")] pub Ulid);

impl CommentId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for CommentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CommentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for CommentId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 誰が書いたか（`task_comments.author_kind` の CHECK と対）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommentAuthorKind {
    /// 人（GUI の `POST /tasks/{id}/comments`）。担当をすぐ起こす（ADR-0044 D2 の表）。
    Human,
    /// 組織の「人」（ワーカーの `{"type":"comment"}` 行、または秘書・lead が委譲先に書いたもの）。
    Node,
    /// taskd 自身（決定的な記録）。
    System,
}

impl CommentAuthorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CommentAuthorKind::Human => "human",
            CommentAuthorKind::Node => "node",
            CommentAuthorKind::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<CommentAuthorKind> {
        match s {
            "human" => Some(CommentAuthorKind::Human),
            "node" => Some(CommentAuthorKind::Node),
            "system" => Some(CommentAuthorKind::System),
            _ => None,
        }
    }
}

/// `task_comments` の 1 行（ADR-0044 D2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskComment {
    pub id: CommentId,
    pub task_id: TaskId,
    pub author_kind: CommentAuthorKind,
    /// `author_kind = node` のときの `org_nodes.id`（無ければ `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub body: String,
    /// ワーカーが書いたコメントの run（人のコメントには無い）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
}

/// 本文の上限（1 コメント）。長い記録は成果物に書く。
pub const MAX_COMMENT_CHARS: usize = 20_000;

/// 前置きに載せるコメントの件数（ADR-0044 D2「コメントの最新 20 件」）。
pub const PREAMBLE_COMMENTS: usize = 20;

impl TaskComment {
    /// 新しいコメントを組む（id と時刻は呼び出し側が与える値でも、既定でもよい）。
    pub fn new(
        task_id: TaskId,
        author_kind: CommentAuthorKind,
        author: Option<String>,
        body: String,
        run_id: Option<String>,
        created_at: OffsetDateTime,
    ) -> Self {
        Self {
            id: CommentId::new(),
            task_id,
            author_kind,
            author,
            body,
            run_id,
            created_at,
        }
    }

    /// 本文の検証（空白だけは不可、上限あり）。純粋関数。
    pub fn validate_body(body: &str) -> Result<(), String> {
        if body.trim().is_empty() {
            return Err("comment body must not be blank".to_string());
        }
        if body.chars().count() > MAX_COMMENT_CHARS {
            return Err(format!("comment body must be at most {MAX_COMMENT_CHARS} characters"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn author_kind_round_trips_through_its_string_form() {
        for kind in [
            CommentAuthorKind::Human,
            CommentAuthorKind::Node,
            CommentAuthorKind::System,
        ] {
            assert_eq!(CommentAuthorKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(CommentAuthorKind::parse("robot"), None);
    }

    #[test]
    fn a_blank_body_is_rejected_and_a_long_body_is_rejected() {
        assert!(TaskComment::validate_body("  \n ").is_err());
        assert!(TaskComment::validate_body("ok").is_ok());
        let long = "あ".repeat(MAX_COMMENT_CHARS + 1);
        assert!(TaskComment::validate_body(&long).is_err());
    }
}
