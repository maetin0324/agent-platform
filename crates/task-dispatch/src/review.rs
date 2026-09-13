//! Reviewer（DESIGN §5.7, ADR-0005 D5）。Phase 3 は `Command` と `ArtifactExists` のみ判定する。
//! `Command` はワーカーの自己申告を信じず、ワークスペースで実際に再実行する。
//! `Reviewer` / `Human` は Phase 5/6 まで未対応で、pass 扱いにはしない（原則 4）。

use std::path::Path;
use std::time::Duration;

use task_core::{ArtifactRef, Check, Task};
use task_worker::Workspace;
use task_worker::artifact::sha256_file;

/// 条件 1 件の判定結果。`Event::ReviewVerdict` にそのまま写す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub criterion_idx: usize,
    pub pass: bool,
    pub reason: String,
}

const REASON_TAIL: usize = 1024;

fn tail(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// `task.acceptance` を順に判定する。`produced` はその run の `ArtifactProduced`（名前の照合に使う）。
pub async fn review_task(
    task: &Task,
    workspace: &dyn Workspace,
    workspace_dir: &Path,
    produced: &[ArtifactRef],
    command_timeout: Duration,
) -> Vec<Verdict> {
    let mut verdicts = Vec::with_capacity(task.acceptance.len());
    for (idx, criterion) in task.acceptance.iter().enumerate() {
        let (pass, reason) = match &criterion.check {
            Check::Command { cmd, expect_exit } => {
                match workspace.exec(cmd, command_timeout).await {
                    Err(e) => (false, format!("exec failed: {e}")),
                    Ok(r) if r.timed_out => (
                        false,
                        format!("command timed out after {}s: {cmd}", command_timeout.as_secs()),
                    ),
                    Ok(r) => {
                        let pass = r.exit == Some(*expect_exit);
                        (
                            pass,
                            format!(
                                "cmd={cmd:?} exit={:?} expected={expect_exit} stdout_tail={:?} stderr_tail={:?}",
                                r.exit,
                                tail(&r.stdout_tail, REASON_TAIL),
                                tail(&r.stderr_tail, REASON_TAIL)
                            ),
                        )
                    }
                }
            }
            Check::ArtifactExists { name } => {
                let rel = produced
                    .iter()
                    .rev()
                    .find(|a| &a.name == name)
                    .map(|a| a.path.clone())
                    .unwrap_or_else(|| format!("artifacts/{name}"));
                let full = workspace_dir.join(&rel);
                if full.is_file() {
                    match sha256_file(&full) {
                        Ok(sha) => (true, format!("path={rel} sha256={sha}")),
                        Err(e) => (false, format!("path={rel} unreadable: {e}")),
                    }
                } else {
                    (false, format!("artifact {name:?} not found at {rel}"))
                }
            }
            Check::Reviewer => (
                false,
                "check kind 'reviewer' is not supported until Phase 5".to_string(),
            ),
            Check::Human => (
                false,
                "check kind 'human' is not supported until Phase 6".to_string(),
            ),
        };
        verdicts.push(Verdict {
            criterion_idx: idx,
            pass,
            reason,
        });
    }
    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use task_core::*;
    use task_worker::LocalWorkspace;

    fn task_with(checks: Vec<Check>, dir: &Path) -> Task {
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: checks
                .into_iter()
                .map(|check| Criterion {
                    text: "c".into(),
                    check,
                })
                .collect(),
            inputs: vec![],
            depends_on: vec![],
            status: Status::Reviewing,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from(dir),
            },
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 10,
                max_retries: 0,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn command_checks_are_re_executed_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("present.txt"), "x").unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let task = task_with(
            vec![
                Check::Command {
                    cmd: "test -f present.txt".into(),
                    expect_exit: 0,
                },
                Check::Command {
                    cmd: "test -f absent.txt".into(),
                    expect_exit: 0,
                },
                Check::Command {
                    cmd: "exit 7".into(),
                    expect_exit: 7,
                },
                Check::Command {
                    cmd: "sleep 30".into(),
                    expect_exit: 0,
                },
            ],
            dir.path(),
        );
        let v = review_task(&task, &ws, dir.path(), &[], Duration::from_millis(300)).await;
        assert_eq!(v.iter().map(|x| x.pass).collect::<Vec<_>>(), vec![true, false, true, false]);
        assert!(v[3].reason.contains("timed out"));
        assert_eq!(v[1].criterion_idx, 1);
    }

    #[tokio::test]
    async fn artifact_exists_uses_produced_path_then_fallback_and_unsupported_kinds_fail() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts/sub")).unwrap();
        std::fs::write(dir.path().join("artifacts/sub/bench.json"), "{}").unwrap();
        std::fs::write(dir.path().join("artifacts/report.md"), "# r").unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let produced = vec![ArtifactRef {
            name: "bench".into(),
            path: "artifacts/sub/bench.json".into(),
            sha256: String::new(),
            kind: "json".into(),
        }];
        let task = task_with(
            vec![
                Check::ArtifactExists { name: "bench".into() },
                Check::ArtifactExists { name: "report.md".into() },
                Check::ArtifactExists { name: "missing".into() },
                Check::Reviewer,
                Check::Human,
            ],
            dir.path(),
        );
        let v = review_task(&task, &ws, dir.path(), &produced, Duration::from_secs(5)).await;
        assert_eq!(v.iter().map(|x| x.pass).collect::<Vec<_>>(), vec![true, true, false, false, false]);
        assert!(v[0].reason.contains("sha256=44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"));
        assert!(v[1].reason.contains("artifacts/report.md"));
        assert!(v[3].reason.contains("Phase 5"));
        assert!(v[4].reason.contains("Phase 6"));
    }
}
