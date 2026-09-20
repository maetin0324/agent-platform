//! `fake` アダプタ（ADR-0005 D2）。設定されたコマンドをサブプロセスとして起動する。

use std::sync::Arc;

use async_trait::async_trait;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, WorkerAdapter};
use crate::protocol::RunRequest;
use crate::subprocess::{SubprocessSpec, run_subprocess};

#[derive(Debug, Clone)]
pub struct FakeAdapter {
    spec: SubprocessSpec,
}

impl FakeAdapter {
    pub const ID: &'static str = "fake";

    /// 既定: `progress` 1 行と `done{evidence:[]}` を返す `sh` スクリプト。
    ///
    /// ADR-0048 D2（Phase 60a）: `progress` は `kind = "status"`（節目）を名乗る。`fake` は
    /// 道具も発話も持たないので、正規化できるのは節目だけである。`msg` は従来どおり `fake worker`。
    pub fn default_command() -> Vec<String> {
        vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; \
             echo '{\"type\":\"progress\",\"msg\":\"fake worker\",\"kind\":\"status\",\"summary\":\"fake worker\"}'; \
             echo '{\"type\":\"done\",\"summary\":\"fake\",\"evidence\":[]}'"
                .into(),
        ]
    }

    /// `command` が空なら `default_command()`。
    pub fn new(command: Vec<String>) -> Self {
        let mut command = if command.is_empty() {
            Self::default_command()
        } else {
            command
        };
        let program = command.remove(0);
        Self {
            spec: SubprocessSpec {
                program,
                args: command,
                env: vec![],
                container: None,
            },
        }
    }

    pub fn spec(&self) -> &SubprocessSpec {
        &self.spec
    }

    /// サブプロセスに渡す追加の環境変数。
    pub fn set_env(&mut self, env: Vec<(String, String)>) {
        self.spec.env = env;
    }
}

impl Default for FakeAdapter {
    fn default() -> Self {
        Self::new(vec![])
    }
}

#[async_trait]
impl WorkerAdapter for FakeAdapter {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(
        &self,
        req: RunRequest,
        run_id: &str,
        limits: RunLimits,
        sink: &dyn EventSink,
    ) -> Result<RunOutcome, AdapterError> {
        run_subprocess(&self.spec, &req, run_id, &limits, sink).await
    }

    /// ADR-0043 D3（Phase 56）: コンテナの中で起こす複製（差し込み点は `subprocess::run_subprocess`）。
    fn with_container(&self, plan: crate::container::SharedPlan) -> Option<Arc<dyn WorkerAdapter>> {
        let mut next = self.clone();
        next.spec.container = Some(plan);
        Some(Arc::new(next))
    }
}
