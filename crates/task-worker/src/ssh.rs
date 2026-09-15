//! クラスタ上でコマンドを実行する `Workspace`（ADR-0018）。
//!
//! **ワーカー（LLM）は手元で動く。** ここで行うのは「コマンドをクラスタで実行すること」と「ファイルの同期」だけ。
//! 接続は**人が張った `ControlMaster` の多重接続を借りる**（`BatchMode=yes` で、対話的な認証は絶対に行わない）。
//! 接続が無ければ `WorkspaceError::Unreachable` を返し、呼び出し側（ディスパッチャ）が供給側失敗として扱う。

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use task_core::{ArtifactRef, Task};

use crate::workspace::{ExecResult, LocalWorkspace, Workspace, WorkspaceError};

/// ワークスペースの同期方法（ADR-0018 D4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// run の前後で rsync を往復させる（既定）。
    Rsync,
    /// 共有ファイルシステム。何もしない。
    None,
}

/// 1 タスク分のリモート実行の設定。
#[derive(Debug, Clone)]
pub struct SshSettings {
    /// 設定の `[[clusters]] id`（ログとイベントに出す）。
    pub cluster: String,
    /// `~/.ssh/config` の `Host` 名。
    pub host: String,
    /// このタスクのリモート作業ディレクトリ（`remote_workdir` + タスクのディレクトリ名）。
    pub remote_dir: PathBuf,
    /// コマンドの前に流す準備（`module load ...` など）。
    pub setup: Vec<String>,
    /// リモートで `export` する環境変数（値は決定的な順で並べる）。
    pub env: Vec<(String, String)>,
    pub sync: SyncMode,
    /// push（手元 → クラスタ）で、手元に無いファイルをクラスタ側から消すか（ADR-0018 D4）。
    /// **既定は false**。既存プロジェクトを指すタスクでファイルを失わないため。taskd 専用の作業ディレクトリなら true にしてよい。
    pub delete_on_push: bool,
    /// `exec` の前に push、後に pull するか（既定 true）。判定コマンドが手元の編集を見て、
    /// その結果の成果物が手元に戻るようにするため（ADR-0018 D4）。
    pub sync_around_exec: bool,
    /// `rsync` から除外するパターン（`.taskd/` は常に除外する）。
    pub rsync_excludes: Vec<String>,
    /// `ssh` の起動コマンド（テストで差し替える。既定は `["ssh"]`）。
    pub ssh_command: Vec<String>,
    /// `rsync` の起動コマンド（同上）。
    pub rsync_command: Vec<String>,
}

impl SshSettings {
    pub fn new(cluster: impl Into<String>, host: impl Into<String>, remote_dir: impl Into<PathBuf>) -> Self {
        Self {
            cluster: cluster.into(),
            host: host.into(),
            remote_dir: remote_dir.into(),
            setup: Vec::new(),
            env: Vec::new(),
            sync: SyncMode::Rsync,
            delete_on_push: false,
            sync_around_exec: true,
            rsync_excludes: Vec::new(),
            ssh_command: vec!["ssh".to_string()],
            rsync_command: vec!["rsync".to_string()],
        }
    }
}

/// `sh` に渡す 1 引数としての安全な引用（シングルクォートで囲み、中の `'` を `'\''` にする）。
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// ローカルの作業ディレクトリを持ちつつ、コマンドをリモートで実行するワークスペース。
#[derive(Debug, Clone)]
pub struct SshWorkspace {
    local: LocalWorkspace,
    settings: SshSettings,
}

impl SshWorkspace {
    pub fn new(dir: impl Into<PathBuf>, settings: SshSettings) -> Self {
        Self { local: LocalWorkspace::new(dir), settings }
    }

    pub fn dir(&self) -> &Path {
        self.local.dir()
    }

    pub fn settings(&self) -> &SshSettings {
        &self.settings
    }

    /// `ssh` に必ず付ける引数（対話的な認証を禁じる）。
    fn ssh_base(&self) -> Vec<String> {
        let mut args = self.settings.ssh_command.clone();
        args.push("-o".into());
        args.push("BatchMode=yes".into());
        args
    }

    /// 人が張った多重接続があるか（ADR-0018 D2）。無ければ taskd は何もできない。
    pub async fn control_master_alive(&self) -> bool {
        let mut args = self.ssh_base();
        args.push("-O".into());
        args.push("check".into());
        args.push(self.settings.host.clone());
        let Some((program, rest)) = args.split_first() else {
            return false;
        };
        match tokio::process::Command::new(program)
            .args(rest)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
        {
            Ok(status) => status.success(),
            Err(_) => false,
        }
    }

    /// リモートで走らせるスクリプト（作業ディレクトリへ移動 → env → setup → コマンド）。
    fn remote_script(&self, cmd: &str, timeout: Duration) -> String {
        let mut script = String::new();
        script.push_str(&format!("cd {} && ", shq(&self.settings.remote_dir.to_string_lossy())));
        for (k, v) in &self.settings.env {
            script.push_str(&format!("export {k}={} && ", shq(v)));
        }
        for line in &self.settings.setup {
            script.push_str(&format!("{{ {line}; }} && "));
        }
        // ローカル側の timeout に加えて、リモートでも kill する（二重の安全弁。ADR-0018 D5）。
        let secs = timeout.as_secs().max(1);
        script.push_str(&format!("timeout -k 5 {secs} sh -c {}", shq(cmd)));
        script
    }

    /// リモートの作業ディレクトリを作る。
    async fn ensure_remote_dir(&self) -> Result<(), WorkspaceError> {
        let dir = self.settings.remote_dir.to_string_lossy().to_string();
        let out = self.run_ssh(&format!("mkdir -p {}", shq(&dir)), Duration::from_secs(60)).await?;
        if out.exit != Some(0) {
            return Err(WorkspaceError::Remote(format!(
                "cannot create the remote directory {dir} on {}: {}",
                self.settings.cluster,
                out.stderr_tail.trim()
            )));
        }
        Ok(())
    }

    /// `ssh` で 1 コマンド。終了コード 255 は ssh 自身の失敗（= 接続の問題）として `Unreachable` にする。
    async fn run_ssh(&self, script: &str, timeout: Duration) -> Result<ExecResult, WorkspaceError> {
        let mut args = self.ssh_base();
        args.push(self.settings.host.clone());
        args.push(script.to_string());
        let result = run_command(&args, timeout).await?;
        if result.exit == Some(255) {
            return Err(WorkspaceError::Unreachable(format!(
                "ssh to {} ({}) failed: {}",
                self.settings.cluster,
                self.settings.host,
                result.stderr_tail.trim()
            )));
        }
        Ok(result)
    }

    /// 手元の写し → クラスタ（作業のあと、判定の前。ADR-0018 D4）。
    /// 既定では `--delete` を付けない（既存プロジェクトのファイルを消さない）。
    pub async fn push(&self) -> Result<(), WorkspaceError> {
        if self.settings.sync == SyncMode::None {
            return Ok(());
        }
        self.ensure_remote_dir().await?;
        let mut args = self.settings.rsync_command.clone();
        args.push("-a".into());
        if self.settings.delete_on_push {
            args.push("--delete".into());
        }
        args.push("-e".into());
        args.push(self.ssh_base().join(" "));
        args.push("--exclude".into());
        args.push(".taskd/".into());
        for pattern in &self.settings.rsync_excludes {
            args.push("--exclude".into());
            args.push(pattern.clone());
        }
        args.push(format!("{}/", self.local.dir().to_string_lossy()));
        args.push(format!("{}:{}/", self.settings.host, self.settings.remote_dir.to_string_lossy()));
        self.run_rsync(&args, "push").await
    }

    /// クラスタ → 手元の写し（run の前と、判定の後。ADR-0018 D4）。
    /// 写しは taskd が作り直してよいので、こちらは `--delete` してよい。
    pub async fn pull(&self) -> Result<(), WorkspaceError> {
        if self.settings.sync == SyncMode::None {
            return Ok(());
        }
        self.ensure_remote_dir().await?;
        let mut args = self.settings.rsync_command.clone();
        args.extend(["-a".into(), "--delete".into()]);
        args.push("-e".into());
        args.push(self.ssh_base().join(" "));
        args.push("--exclude".into());
        args.push(".taskd/".into());
        for pattern in &self.settings.rsync_excludes {
            args.push("--exclude".into());
            args.push(pattern.clone());
        }
        args.push(format!("{}:{}/", self.settings.host, self.settings.remote_dir.to_string_lossy()));
        args.push(format!("{}/", self.local.dir().to_string_lossy()));
        self.run_rsync(&args, "pull").await
    }

    async fn run_rsync(&self, args: &[String], direction: &str) -> Result<(), WorkspaceError> {
        let result = run_command(args, Duration::from_secs(3600)).await?;
        match result.exit {
            Some(0) => Ok(()),
            // rsync の 255 / 12 は ssh の失敗（接続の問題）。
            Some(255) | Some(12) => Err(WorkspaceError::Unreachable(format!(
                "rsync {direction} to {} failed: {}",
                self.settings.cluster,
                result.stderr_tail.trim()
            ))),
            other => Err(WorkspaceError::Remote(format!(
                "rsync {direction} to {} exited with {other:?}: {}",
                self.settings.cluster,
                result.stderr_tail.trim()
            ))),
        }
    }

    /// ワーカーがクラスタでコマンドを実行するためのラッパ `.taskd/remote-exec`（ADR-0018 D3）。
    pub async fn write_remote_exec_helper(&self) -> Result<PathBuf, WorkspaceError> {
        let dir = self.local.dir().join(".taskd");
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join("remote-exec");
        let ssh = self.ssh_base().join(" ");
        let remote = self.settings.remote_dir.to_string_lossy().to_string();
        let mut prefix = String::new();
        for (k, v) in &self.settings.env {
            prefix.push_str(&format!("export {k}={} && ", shq(v)));
        }
        for line in &self.settings.setup {
            prefix.push_str(&format!("{{ {line}; }} && "));
        }
        let script = format!(
            "#!/bin/sh\n\
             # taskd が run ごとに作るラッパ（ADR-0018 D3）。クラスタ {cluster} でコマンドを実行する。\n\
             # 使い方: .taskd/remote-exec <コマンド ...>\n\
             set -u\n\
             if [ $# -eq 0 ]; then echo \"usage: $0 <command...>\" >&2; exit 2; fi\n\
             cmd=\"$*\"\n\
             exec {ssh} {host} \"cd {remote_q} && {prefix}sh -c \\\"$cmd\\\"\"\n",
            cluster = self.settings.cluster,
            ssh = ssh,
            host = self.settings.host,
            remote_q = remote,
            prefix = prefix,
        );
        tokio::fs::write(&path, script).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = tokio::fs::metadata(&path).await?.permissions();
            perms.set_mode(0o755);
            tokio::fs::set_permissions(&path, perms).await?;
        }
        Ok(path)
    }
}

/// tick（同期の文脈）から呼ぶ、多重接続の有無の確認（ADR-0018 D2）。`ssh -O check` は unix ソケットを見るだけで即座に返る。
pub fn control_master_alive_blocking(ssh_command: &[String], host: &str) -> bool {
    let Some((program, rest)) = ssh_command.split_first() else {
        return false;
    };
    std::process::Command::new(program)
        .args(rest)
        .args(["-o", "BatchMode=yes", "-O", "check", host])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 外部コマンドを 1 つ動かし、末尾の出力と終了コードを返す（`LocalWorkspace::exec` と同じ流儀）。
async fn run_command(args: &[String], timeout: Duration) -> Result<ExecResult, WorkspaceError> {
    let Some((program, rest)) = args.split_first() else {
        return Err(WorkspaceError::Remote("empty command".to_string()));
    };
    let mut command = tokio::process::Command::new(program);
    command.args(rest);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);
    let child = command.output();
    match tokio::time::timeout(timeout, child).await {
        Ok(out) => {
            let out = out?;
            Ok(ExecResult {
                exit: out.status.code(),
                stdout_tail: crate::workspace::tail_utf8_lossy(&out.stdout),
                stderr_tail: crate::workspace::tail_utf8_lossy(&out.stderr),
                timed_out: false,
            })
        }
        Err(_) => Ok(ExecResult {
            exit: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            timed_out: true,
        }),
    }
}

#[async_trait]
impl Workspace for SshWorkspace {
    /// **クラスタ側が正**（ADR-0018 D1）。手元の写しを用意し、クラスタの内容を取り込んでから、
    /// run に必要なディレクトリを作る。既存プロジェクトを指していても壊さない。
    async fn prepare(&self, task: &Task) -> Result<PathBuf, WorkspaceError> {
        tokio::fs::create_dir_all(self.local.dir()).await?;
        self.pull().await?;
        self.local.prepare(task).await
    }

    /// コマンドはクラスタで実行する（ADR-0018 D1）。前後に同期して、手元の編集が反映され、
    /// リモートで生まれたファイルが手元に戻るようにする。
    async fn exec(&self, cmd: &str, timeout: Duration) -> Result<ExecResult, WorkspaceError> {
        if self.settings.sync_around_exec {
            self.push().await?;
        }
        let script = self.remote_script(cmd, timeout);
        // ssh 自体のタイムアウトは、リモートの timeout より少し長くする。
        let result = self.run_ssh(&script, timeout + Duration::from_secs(30)).await?;
        if self.settings.sync_around_exec {
            self.pull().await?;
        }
        Ok(result)
    }

    /// リモートの結果を取り込んでから、ローカルで sha256 を計算する（真実はローカル。ADR-0018 D4）。
    async fn collect(&self, task: &Task) -> Result<Vec<ArtifactRef>, WorkspaceError> {
        self.pull().await?;
        self.local.collect(task).await
    }
}
