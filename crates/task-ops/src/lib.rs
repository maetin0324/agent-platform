//! `task-ops`: `taskctl` の approve/reject/answer/cancel/add/plan/replay の判断と検証、および
//! イベントからの派生ビュー（質問文・answers・prior_review・連続 requeue・バックオフ・Human check の
//! Approval 子の対応など）を 1 か所にまとめる（ADR-0013 D7）。`task-api`（Phase 9b）からも同じ関数を
//! 呼べるようにするための下ごしらえ。
//!
//! 依存は `task-core` のみ（`task-worker` / `task-dispatch` / `tokio` には依存しない）。
//! ワーカープロトコルの型（`task_worker::{PriorReview, Answer}`）への写像は呼び出し側で行う。

pub mod add;
pub mod derive;
pub mod error;
pub mod gate;
pub mod plan;
pub mod replay;

pub use error::OpsError;
