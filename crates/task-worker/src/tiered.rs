//! Resolve an explicit tier binding before starting a CLI. Never substitute another model.
use crate::{AdapterError, EventSink, RunLimits, RunOutcome, RunRequest, WorkerAdapter};
use async_trait::async_trait;
use std::sync::Arc;
use task_core::{
    Tier,
    model_routing::{TierModels, resolve},
};

pub struct TieredAdapter {
    pub base: Arc<dyn WorkerAdapter>,
    pub models: TierModels,
    pub account_id: Option<String>,
    pub credential_error: Option<String>,
}
#[async_trait]
impl WorkerAdapter for TieredAdapter {
    fn id(&self) -> &str {
        self.base.id()
    }
    fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }
    fn model_for_tier(&self, tier: Tier) -> Result<Option<String>, String> {
        if let Some(reason) = &self.credential_error {
            return Err(reason.clone());
        }
        resolve(&self.models, tier)
    }
    fn reasoning_effort_for_tier(&self, tier: Tier) -> Option<String> {
        task_core::model_routing::reasoning_effort(&self.models, tier)
    }
    /// ADR-0069 Phase 118 D1: 実際に CLI へ effort を渡せるかは基盤アダプタ次第（codex は対応、
    /// claude-code は既定のまま非対応）。
    fn supports_reasoning_effort(&self) -> bool {
        self.base.supports_reasoning_effort()
    }
    async fn run(
        &self,
        req: RunRequest,
        run_id: &str,
        limits: RunLimits,
        sink: &dyn EventSink,
    ) -> Result<RunOutcome, AdapterError> {
        let tier = req.task.worker_hint.tier;
        let model = self.model_for_tier(tier).map_err(AdapterError::Other)?;
        let mut base = match model {
            Some(model) => self
                .base
                .with_model(&model)
                .ok_or_else(|| AdapterError::Other("adapter cannot apply tier model".into()))?,
            None => self.base.clone(),
        };
        // ADR-0069 Phase 118 D1: 対応するアダプタ（codex）にだけ、実際に reasoning effort を渡す。
        // 対応しないアダプタ（claude-code）では黙って素通しする（監査記録は別の層で「渡らなかった」
        // ことを区別する）。
        if let Some(effort) = self.reasoning_effort_for_tier(tier)
            && let Some(wrapped) = base.with_reasoning_effort(&effort)
        {
            base = wrapped;
        }
        base.run(req, run_id, limits, sink).await
    }
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        Some(Arc::new(Self {
            base: self.base.with_env(extra)?,
            models: self.models.clone(),
            account_id: self.account_id.clone(),
            credential_error: self.credential_error.clone(),
        }))
    }
    fn with_container(&self, plan: crate::container::SharedPlan) -> Option<Arc<dyn WorkerAdapter>> {
        Some(Arc::new(Self {
            base: self.base.with_container(plan)?,
            models: self.models.clone(),
            account_id: self.account_id.clone(),
            credential_error: self.credential_error.clone(),
        }))
    }
    /// ADR-0072 D14（Phase E4b 項目3）: `self.adapters` に登録された実体は `TieredAdapter` で
    /// 包まれている（`with_model` と同じ理由）ので、基盤アダプタへそのまま中継する。
    fn with_permission_mode(&self, mode: &str) -> Option<Arc<dyn WorkerAdapter>> {
        Some(Arc::new(Self {
            base: self.base.with_permission_mode(mode)?,
            models: self.models.clone(),
            account_id: self.account_id.clone(),
            credential_error: self.credential_error.clone(),
        }))
    }
}
