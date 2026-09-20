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
    async fn run(
        &self,
        req: RunRequest,
        run_id: &str,
        limits: RunLimits,
        sink: &dyn EventSink,
    ) -> Result<RunOutcome, AdapterError> {
        let model = self
            .model_for_tier(req.task.worker_hint.tier)
            .map_err(AdapterError::Other)?;
        let base = match model {
            Some(model) => self
                .base
                .with_model(&model)
                .ok_or_else(|| AdapterError::Other("adapter cannot apply tier model".into()))?,
            None => self.base.clone(),
        };
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
}
