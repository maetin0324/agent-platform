//! `Workspace`（DESIGN §5.8）。本プロジェクトは `LocalWorkspace` のみ実装し、
//! `RemoteWorkspace` は接続層プロジェクトが実装するまで骨組みのみ（ADR-0005 D3）。
//!
//! 1 インスタンス = 1 タスクの作業ディレクトリ。ディスパッチャが run ごとに
//! `LocalWorkspace::new(dir)` を作る。

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use task_core::{ArtifactRef, Task};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("input artifact not found: {0}")]
    InputNotFound(String),
    #[error("remote workspace is not implemented (cluster={0})")]
    RemoteUnsupported(String),
    /// ADR-0018 D5: ssh / rsync 自体の失敗（接続が無い・認証を求められた・宛先に届かない）。供給側失敗として扱う。
    #[error("cluster is unreachable: {0}")]
    Unreachable(String),
    /// リモート側の準備に失敗した（ディレクトリが作れない・rsync が異常終了した）。
    #[error("remote error: {0}")]
    Remote(String),
}

/// `Workspace::exec` の結果。`exit` は signal 終了・タイムアウト時 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub exit: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub timed_out: bool,
}

/// DESIGN §5.8 の trait。
#[async_trait]
pub trait Workspace: Send + Sync {
    /// 作業ディレクトリと `artifacts/`, `inputs/`, `runs/` を作り、`task.inputs` を `inputs/<name>` に配置する。
    /// 既存ファイルは消さない。戻り値は作業ディレクトリの絶対パス。
    async fn prepare(&self, task: &Task) -> Result<PathBuf, WorkspaceError>;
    /// `sh -c <cmd>` を作業ディレクトリで実行する。`timeout` 超過は kill して `timed_out=true`。
    /// stdout/stderr は末尾 4 KiB を返す。
    async fn exec(&self, cmd: &str, timeout: Duration) -> Result<ExecResult, WorkspaceError>;
    /// `artifacts/` 配下のファイルを再帰的に列挙し、sha256 付きの `ArtifactRef` を返す
    /// （`path` は作業ディレクトリ相対、`name` はファイル名、`kind` は拡張子）。
    async fn collect(&self, task: &Task) -> Result<Vec<ArtifactRef>, WorkspaceError>;
}

/// ローカルファイルシステム上の作業ディレクトリ。
#[derive(Debug, Clone)]
pub struct LocalWorkspace {
    dir: PathBuf,
}

impl LocalWorkspace {
    /// `dir` はそのタスクの作業ディレクトリ（ADR-0005 D3）。
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// 接続層プロジェクトが実装する。型のみ。
#[derive(Debug, Clone)]
pub struct RemoteWorkspace {
    pub cluster: String,
    pub path: PathBuf,
}

// ---- 以下は implementer（unit B）が実装する ----

const TAIL_BYTES: usize = 4096;

pub(crate) fn tail_utf8_lossy(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(TAIL_BYTES);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[async_trait]
impl Workspace for LocalWorkspace {
    async fn prepare(&self, task: &Task) -> Result<PathBuf, WorkspaceError> {
        tokio::fs::create_dir_all(&self.dir).await?;
        tokio::fs::create_dir_all(self.dir.join("artifacts")).await?;
        tokio::fs::create_dir_all(self.dir.join("inputs")).await?;
        tokio::fs::create_dir_all(self.dir.join("runs")).await?;

        for input in &task.inputs {
            let src = Path::new(&input.path);
            if src.is_absolute() {
                let dest = self.dir.join("inputs").join(&input.name);
                tokio::fs::copy(src, &dest).await.map_err(|_| {
                    WorkspaceError::InputNotFound(input.name.clone())
                })?;
            } else if self.dir.join(src).exists() {
                // すでに配置済み。何もしない。
            } else {
                return Err(WorkspaceError::InputNotFound(input.name.clone()));
            }
        }

        let canon = self.dir.canonicalize()?;
        Ok(canon)
    }

    async fn exec(&self, cmd: &str, timeout: Duration) -> Result<ExecResult, WorkspaceError> {
        use std::process::Stdio;

        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(cmd);
        command.current_dir(&self.dir);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);
        #[cfg(unix)]
        {
            command.process_group(0);
        }

        let mut child = command.spawn()?;
        let mut stdout = child.stdout.take().ok_or_else(|| {
            WorkspaceError::Io(std::io::Error::other("child stdout not captured"))
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| {
            WorkspaceError::Io(std::io::Error::other("child stderr not captured"))
        })?;

        let read_stdout = async {
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut buf).await?;
            Ok::<Vec<u8>, std::io::Error>(buf)
        };
        let read_stderr = async {
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf).await?;
            Ok::<Vec<u8>, std::io::Error>(buf)
        };

        let wait_fut = child.wait();
        let combined = async {
            let (out, err, status) = tokio::join!(read_stdout, read_stderr, wait_fut);
            (out, err, status)
        };

        match tokio::time::timeout(timeout, combined).await {
            Ok((out, err, status)) => {
                let out = out?;
                let err = err?;
                let status = status?;
                Ok(ExecResult {
                    exit: status.code(),
                    stdout_tail: tail_utf8_lossy(&out),
                    stderr_tail: tail_utf8_lossy(&err),
                    timed_out: false,
                })
            }
            Err(_elapsed) => {
                if let Some(pid) = child.id() {
                    #[cfg(unix)]
                    {
                        let _ = nix::sys::signal::killpg(
                            nix::unistd::Pid::from_raw(pid as i32),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                }
                let _ = child.wait().await;
                Ok(ExecResult { exit: None, stdout_tail: String::new(), stderr_tail: String::new(), timed_out: true })
            }
        }
    }

    async fn collect(&self, _task: &Task) -> Result<Vec<ArtifactRef>, WorkspaceError> {
        let artifacts_dir = self.dir.join("artifacts");
        if !artifacts_dir.exists() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        collect_files_recursive(&artifacts_dir, &mut files)?;

        let mut refs = Vec::with_capacity(files.len());
        for file in &files {
            let rel = file
                .strip_prefix(&self.dir)
                .map_err(|_| WorkspaceError::Io(std::io::Error::other("artifact path outside workspace")))?;
            let rel_str = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let kind = file
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            let sha256 = crate::artifact::sha256_file(file)?;
            refs.push(ArtifactRef { name, path: rel_str, sha256, kind });
        }

        refs.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(refs)
    }
}

fn collect_files_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), WorkspaceError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_recursive(&path, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

#[async_trait]
impl Workspace for RemoteWorkspace {
    async fn prepare(&self, _task: &Task) -> Result<PathBuf, WorkspaceError> {
        unimplemented!("RemoteWorkspace is implemented by the connection layer project")
    }

    async fn exec(&self, _cmd: &str, _timeout: Duration) -> Result<ExecResult, WorkspaceError> {
        unimplemented!("RemoteWorkspace is implemented by the connection layer project")
    }

    async fn collect(&self, _task: &Task) -> Result<Vec<ArtifactRef>, WorkspaceError> {
        unimplemented!("RemoteWorkspace is implemented by the connection layer project")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use task_core::WorkspaceSpec;

    // `crate::protocol::tests::sample_task` は `mod tests` 自体が private で
    // 到達不可のため（`sample_task` は `pub(crate)` だがモジュールに `pub` が無い）、
    // ここでは同等のタスクをローカルに組み立てる。`protocol.rs` は編集禁止のため。
    fn sample_task() -> Task {
        use task_core::*;
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Running,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: PathBuf::from("/tmp/ws") },
            budget: Budget { max_turns: 10, max_wall_secs: 60, max_retries: 1 },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
        }
    }

    #[tokio::test]
    async fn prepare_creates_subdirs_keeps_existing_and_copies_absolute_inputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("inputs")).expect("mkdir inputs");
        std::fs::write(dir.path().join("inputs/marker"), b"keep-me").expect("write marker");

        let input_src = tempfile::tempdir().expect("tempdir");
        let abs_input_path = input_src.path().join("data.txt");
        std::fs::write(&abs_input_path, b"hello").expect("write input");

        let ws = LocalWorkspace::new(dir.path());
        let mut task = sample_task();
        task.workspace = WorkspaceSpec::Local { path: dir.path().to_path_buf() };
        task.inputs = vec![task_core::ArtifactRef {
            name: "data.txt".into(),
            path: abs_input_path.to_string_lossy().into_owned(),
            sha256: String::new(),
            kind: "txt".into(),
        }];

        let prepared = ws.prepare(&task).await.expect("prepare");
        assert_eq!(prepared, dir.path().canonicalize().expect("canonicalize"));
        assert!(dir.path().join("artifacts").is_dir());
        assert!(dir.path().join("inputs").is_dir());
        assert!(dir.path().join("runs").is_dir());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("inputs/marker")).expect("read marker"),
            "keep-me"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("inputs/data.txt")).expect("read copied input"),
            "hello"
        );
    }

    #[tokio::test]
    async fn prepare_uses_existing_relative_input_without_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("scripts")).expect("mkdir scripts");
        std::fs::write(dir.path().join("scripts/run.sh"), b"#!/bin/sh\n").expect("write script");

        let ws = LocalWorkspace::new(dir.path());
        let mut task = sample_task();
        task.workspace = WorkspaceSpec::Local { path: dir.path().to_path_buf() };
        task.inputs = vec![task_core::ArtifactRef {
            name: "run.sh".into(),
            path: "scripts/run.sh".into(),
            sha256: String::new(),
            kind: "sh".into(),
        }];

        ws.prepare(&task).await.expect("prepare");
    }

    #[tokio::test]
    async fn prepare_errors_on_missing_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = LocalWorkspace::new(dir.path());
        let mut task = sample_task();
        task.workspace = WorkspaceSpec::Local { path: dir.path().to_path_buf() };
        task.inputs = vec![task_core::ArtifactRef {
            name: "missing.txt".into(),
            path: "does/not/exist.txt".into(),
            sha256: String::new(),
            kind: "txt".into(),
        }];

        let err = ws.prepare(&task).await.expect_err("expected InputNotFound");
        assert!(matches!(err, WorkspaceError::InputNotFound(name) if name == "missing.txt"));
    }

    #[tokio::test]
    async fn exec_captures_exit_code_and_stream_tails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = LocalWorkspace::new(dir.path());
        let result = ws
            .exec("echo out; echo err 1>&2; exit 3", Duration::from_secs(5))
            .await
            .expect("exec");
        assert_eq!(result.exit, Some(3));
        assert!(result.stdout_tail.contains("out"));
        assert!(result.stderr_tail.contains("err"));
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn exec_times_out_and_kills_process_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = LocalWorkspace::new(dir.path());
        let start = Instant::now();
        let result = ws.exec("sleep 30", Duration::from_millis(300)).await.expect("exec");
        assert!(result.timed_out);
        assert_eq!(result.exit, None);
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn exec_runs_with_workspace_dir_as_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = LocalWorkspace::new(dir.path());
        let result = ws.exec("pwd", Duration::from_secs(5)).await.expect("exec");
        let printed = result.stdout_tail.trim();
        let printed_canon = std::path::Path::new(printed).canonicalize().expect("canonicalize pwd output");
        let expected = dir.path().canonicalize().expect("canonicalize dir");
        assert_eq!(printed_canon, expected);
    }

    #[tokio::test]
    async fn collect_returns_sorted_nested_artifacts_with_sha256() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("artifacts/sub")).expect("mkdir");
        std::fs::write(dir.path().join("artifacts/b.txt"), b"bbb").expect("write b");
        std::fs::write(dir.path().join("artifacts/sub/a.json"), b"{}").expect("write a");

        let ws = LocalWorkspace::new(dir.path());
        let task = sample_task();
        let refs = ws.collect(&task).await.expect("collect");

        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].path, "artifacts/b.txt");
        assert_eq!(refs[0].name, "b.txt");
        assert_eq!(refs[0].kind, "txt");
        assert_eq!(refs[1].path, "artifacts/sub/a.json");
        assert_eq!(refs[1].name, "a.json");
        assert_eq!(refs[1].kind, "json");
        assert_eq!(refs[1].sha256, crate::artifact::sha256_file(&dir.path().join("artifacts/sub/a.json")).expect("sha"));
    }

    #[tokio::test]
    async fn collect_returns_empty_when_no_artifacts_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = LocalWorkspace::new(dir.path());
        let task = sample_task();
        let refs = ws.collect(&task).await.expect("collect");
        assert!(refs.is_empty());
    }
}
