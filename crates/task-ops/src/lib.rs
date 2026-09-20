//! `task-ops`: `celerisctl` の approve/reject/answer/cancel/add/plan/replay の判断と検証、および
//! イベントからの派生ビュー（質問文・answers・prior_review・連続 requeue・バックオフ・Human check の
//! Approval 子の対応など）を 1 か所にまとめる（ADR-0013 D7）。`task-api`（Phase 9b）からも同じ関数を
//! 呼べるようにするための下ごしらえ。
//!
//! 依存は `task-core` のみ（`task-worker` / `task-dispatch` / `tokio` には依存しない）。
//! ワーカープロトコルの型（`task_worker::{PriorReview, Answer}`）への写像は呼び出し側で行う。

pub mod actions;
pub mod add;
pub mod approval;
/// ADR-0043 D5（Phase 54）: 変更の取り込み（差分・merge・PR・衝突タスク）の足回り。
pub mod changes;
pub mod comment;
/// ADR-0048 D1/D2（Phase 60a）: Console の一本の流れを組み立てる決定的な部品。
pub mod console;
pub mod conversation;
pub mod daemon;
pub mod delegate;
pub mod derive;
/// ADR-0044 D7（Phase 57）: 文書（git が正本）の足回り。
pub mod docs;
pub mod edit;
pub mod error;
pub mod gate;
pub mod graph;
pub mod inbox;
/// ADR-0047（Phase 61）: 知識ベース（`~/knowledge` の Markdown が正本。git・索引・検索・`_inbox`）。
pub mod knowledge;
/// ADR-0044 D6（Phase 55）: 案件・途中目標の中止・一時停止・アーカイブ。
pub mod lifecycle;
/// ADR-0046 D5（Phase 59）: 担当の決定的な選び方（capability matching）。
pub mod matching;
pub mod memory;
pub mod milestone_review;
pub mod plan;
pub mod project_plan;
pub mod replay;
pub mod retry;
pub mod view;
pub mod workspace;

pub use error::OpsError;
