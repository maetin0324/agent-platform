//! `task-ops`: `taskctl` の approve/reject/answer/cancel/add/plan/replay の判断と検証、および
//! イベントからの派生ビュー（質問文・answers・prior_review・連続 requeue・バックオフ・Human check の
//! Approval 子の対応など）を 1 か所にまとめる（ADR-0013 D7）。`task-api`（Phase 9b）からも同じ関数を
//! 呼べるようにするための下ごしらえ。
//!
//! 依存は `task-core` のみ（`task-worker` / `task-dispatch` / `tokio` には依存しない）。
//! ワーカープロトコルの型（`task_worker::{PriorReview, Answer}`）への写像は呼び出し側で行う。

pub mod add;
pub mod approval;
pub mod comment;
pub mod conversation;
pub mod daemon;
pub mod delegate;
pub mod derive;
pub mod edit;
pub mod error;
pub mod gate;
pub mod graph;
pub mod inbox;
pub mod memory;
pub mod milestone_review;
pub mod plan;
pub mod project_plan;
pub mod replay;
pub mod retry;
pub mod view;
pub mod workspace;

pub use error::OpsError;
