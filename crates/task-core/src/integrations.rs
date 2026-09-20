//! 変更の取り込みの記録（ADR-0043 D5。`task_integrations` の 1 行）。
//!
//! タスクが作ったブランチを人または承認済みdeliveryがどう扱ったか（`merge` / `pr` / `discard`）と、その行方
//! （PR が開いている・merge された・閉じた、rebase が衝突した、git / gh が失敗した）を残す。
//! ADR-0051では部署レビュアーの合格後、制御プレーンも取り込み結果を書く。
//!
//! ここは純粋なデータ定義だけ（I/O も LLM も無い。ADR-0001 D2 / DESIGN 原則 1）。
//! git / gh を起こすのは `task_ops::changes`。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::model::TaskId;
use crate::repos::RepoId;

/// 取り込みの記録の一意識別子（ULID）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct IntegrationId(#[schemars(with = "String")] pub Ulid);

impl IntegrationId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for IntegrationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for IntegrationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for IntegrationId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 取り込みの方法（ADR-0043 D5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationMethod {
    /// default_branch に rebase して fast-forward（push はしない）。
    Merge,
    /// `git push -u origin <branch>` → `gh pr create`。
    Pr,
    /// worktree とブランチを消す（確認付き）。
    Discard,
}

impl IntegrationMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            IntegrationMethod::Merge => "merge",
            IntegrationMethod::Pr => "pr",
            IntegrationMethod::Discard => "discard",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "merge" => Some(IntegrationMethod::Merge),
            "pr" => Some(IntegrationMethod::Pr),
            "discard" => Some(IntegrationMethod::Discard),
            _ => None,
        }
    }
}

/// 取り込みの行方（ADR-0043 D5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationState {
    /// `merge` / `discard` が終わった（worktree とブランチは消えている）。
    Done,
    /// PR が開いている。
    Open,
    /// PR が merge された。
    Merged,
    /// PR が merge されずに閉じた。
    Closed,
    /// rebase が衝突した（「衝突の解消」タスクを作った。worktree は残す）。
    Conflict,
    /// git / gh が失敗した（`detail` に理由）。
    Failed,
}

impl IntegrationState {
    pub fn as_str(self) -> &'static str {
        match self {
            IntegrationState::Done => "done",
            IntegrationState::Open => "open",
            IntegrationState::Merged => "merged",
            IntegrationState::Closed => "closed",
            IntegrationState::Conflict => "conflict",
            IntegrationState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "done" => Some(IntegrationState::Done),
            "open" => Some(IntegrationState::Open),
            "merged" => Some(IntegrationState::Merged),
            "closed" => Some(IntegrationState::Closed),
            "conflict" => Some(IntegrationState::Conflict),
            "failed" => Some(IntegrationState::Failed),
            _ => None,
        }
    }

    /// PR の同期（`gh pr view`）がまだ要る状態。
    pub fn needs_refresh(self) -> bool {
        matches!(self, IntegrationState::Open)
    }
}

/// `task_integrations` の 1 行（ADR-0043 D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskIntegration {
    pub id: IntegrationId,
    pub task_id: TaskId,
    /// `project_repos.id`。Phase 49 の 1 リポジトリのタスク（案件のリポジトリの行を持たない）は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<RepoId>,
    /// タスクの中でのリポジトリの名前（`repos/<name>/`）。API の URL もこれで引く。
    pub repo: String,
    pub method: IntegrationMethod,
    pub state: IntegrationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[schemars(with = "Option<String>")]
    pub merged_at: Option<OffsetDateTime>,
    /// 人に見せる 1 行（409 の理由、衝突したファイル、gh の失敗など）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub updated_at: OffsetDateTime,
}

impl TaskIntegration {
    /// 新しい記録を 1 件作る（`id` は採番、`created_at` = `updated_at` = `now`）。
    pub fn new(
        task_id: TaskId,
        repo_id: Option<RepoId>,
        repo: impl Into<String>,
        method: IntegrationMethod,
        state: IntegrationState,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            id: IntegrationId::new(),
            task_id,
            repo_id,
            repo: repo.into(),
            method,
            state,
            pr_number: None,
            pr_url: None,
            merged_at: None,
            detail: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = if detail.trim().is_empty() {
            None
        } else {
            Some(detail)
        };
        self
    }

    /// PR の状態の文字列（`gh pr view --json state` の `OPEN` / `MERGED` / `CLOSED`）を写す。
    /// 知らない値は `None`（そのときは記録を触らない）。
    pub fn pr_state(raw: &str) -> Option<IntegrationState> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "OPEN" => Some(IntegrationState::Open),
            "MERGED" => Some(IntegrationState::Merged),
            "CLOSED" => Some(IntegrationState::Closed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_and_states_round_trip_through_their_column_strings() {
        for m in [
            IntegrationMethod::Merge,
            IntegrationMethod::Pr,
            IntegrationMethod::Discard,
        ] {
            assert_eq!(IntegrationMethod::parse(m.as_str()), Some(m));
        }
        assert_eq!(IntegrationMethod::parse("nope"), None);
        for s in [
            IntegrationState::Done,
            IntegrationState::Open,
            IntegrationState::Merged,
            IntegrationState::Closed,
            IntegrationState::Conflict,
            IntegrationState::Failed,
        ] {
            assert_eq!(IntegrationState::parse(s.as_str()), Some(s));
        }
        assert_eq!(IntegrationState::parse("nope"), None);
        assert!(IntegrationState::Open.needs_refresh());
        assert!(!IntegrationState::Merged.needs_refresh());
    }

    /// ADR-0043 D5: `gh pr view --json state` の値を `open` / `merged` / `closed` に写す。
    #[test]
    fn the_github_pr_state_maps_onto_the_integration_state() {
        assert_eq!(
            TaskIntegration::pr_state("OPEN"),
            Some(IntegrationState::Open)
        );
        assert_eq!(
            TaskIntegration::pr_state("merged"),
            Some(IntegrationState::Merged)
        );
        assert_eq!(
            TaskIntegration::pr_state(" CLOSED "),
            Some(IntegrationState::Closed)
        );
        assert_eq!(TaskIntegration::pr_state("DRAFT"), None);
        assert_eq!(TaskIntegration::pr_state(""), None);
    }
}
