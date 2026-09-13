//! `fake` アダプタ（ADR-0005 D2）。設定されたコマンドをサブプロセスとして起動する。

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
    pub fn default_command() -> Vec<String> {
        vec![
            "sh".into(),
            "-c".into(),
            "cat >/dev/null; \
             echo '{\"type\":\"progress\",\"msg\":\"fake worker\"}'; \
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
}
