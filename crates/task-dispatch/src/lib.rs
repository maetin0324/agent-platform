//! task-dispatch: 決定的ディスパッチャ、リース管理、リトライ、並列度制御（DESIGN §5.2）、
//! `ProviderPolicy`（§5.5）、Reviewer（§5.7。`Command`/`ArtifactExists`/`Plan` 検証は決定的、`Reviewer` はアダプタ経由の別 run）。
//! **LLM 呼び出しはここに書かない。**

pub mod dispatcher;
pub mod policy;
pub mod review;

pub use dispatcher::{DispatchConfig, DispatchError, Dispatcher, TickReport};
pub use policy::{AdapterId, ProviderId, ProviderOutcome, ProviderPolicy, ProviderSpec, StaticPolicy};
pub use review::{
    PLAN_FILE, PlanCheck, REVIEW_FILE, ReviewExtras, ReviewOutcome, ReviewSubject, ReviewerRun, Verdict, needs_reviewer_run,
    review_task, reviewer_hint,
};
