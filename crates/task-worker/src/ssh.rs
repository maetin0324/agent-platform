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

/// ワークスペースの同期方法（ADR-0018 D4、ADR-0019 D1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    /// run の前後で rsync を往復させる。git 管理外の小さなディレクトリ向け。
    Rsync,
    /// 共有ファイルシステム。何もしない。
    None,
    /// クラスタ側で `git worktree` を切り、その中だけを rsync する（ADR-0019）。
    /// 未追跡の巨大データを持ち込まない。git 管理下のプロジェクトの既定の選び方。
    Worktree,
}

/// ADR-0019 D1: worktree の設定。`SyncMode::Worktree` のときだけ使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSettings {
    /// worktree を置く親ディレクトリ（既定は `<project>/.celeris-worktrees`）。
    pub root: Option<PathBuf>,
    /// 切り出す元（既定 `HEAD`）。
    pub base: String,
    /// sparse-checkout で残すパス（空なら全追跡ファイル）。
    pub paths: Vec<String>,
    /// ブランチ名の接頭辞（既定 `celeris/`）。
    pub branch_prefix: String,
}

impl Default for WorktreeSettings {
    fn default() -> Self {
        Self {
            root: None,
            base: "HEAD".to_string(),
            paths: Vec::new(),
            branch_prefix: "celeris/".to_string(),
        }
    }
}

/// 同期の両方向で常に除外するもの（P-46）。celeris が写しに作る管理用のディレクトリで、
/// クラスタ側の既存プロジェクトに持ち込まないし、`--delete` 付きの pull で手元から消してもいけない。
/// `artifacts/` は**除外しない**（成果物はクラスタで作られることがあり、受け入れ条件の照合に要る）。
/// ADR-0059 D5: `.taskd/` はプラットフォーム名が taskd だった頃の名残（`.celeris/` に改名した）。
/// 後方互換のため両方を除外に残す（古い写し・クラスタ側の残骸を同期に巻き込まない）。
pub const SYNC_ALWAYS_EXCLUDED: [&str; 4] = [".taskd/", ".celeris/", "runs/", "inputs/"];

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
    /// ADR-0019: `sync = "worktree"` のときの設定。
    pub worktree: WorktreeSettings,
    /// タスク ID（worktree のディレクトリ名とブランチ名に使う。ADR-0019 D2）。
    pub task_id: String,
    /// push（手元 → クラスタ）で、手元に無いファイルをクラスタ側から消すか（ADR-0018 D4）。
    /// **既定は false**。既存プロジェクトを指すタスクでファイルを失わないため。celeris 専用の作業ディレクトリなら true にしてよい。
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
    pub fn new(
        cluster: impl Into<String>,
        host: impl Into<String>,
        remote_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cluster: cluster.into(),
            host: host.into(),
            remote_dir: remote_dir.into(),
            setup: Vec::new(),
            env: Vec::new(),
            sync: SyncMode::Rsync,
            worktree: WorktreeSettings::default(),
            task_id: String::new(),
            delete_on_push: false,
            sync_around_exec: true,
            rsync_excludes: Vec::new(),
            ssh_command: vec!["ssh".to_string()],
            rsync_command: vec!["rsync".to_string()],
        }
    }

    /// ADR-0019 D1: 同期とコマンド実行の対象。`worktree` のときは worktree のパス、それ以外は `remote_dir`。
    pub fn effective_remote_dir(&self) -> PathBuf {
        match self.sync {
            SyncMode::Worktree => self.worktree_dir(),
            _ => self.remote_dir.clone(),
        }
    }

    /// worktree のパス（`worktree_root`/`<task_id>`。既定の root は `<project>/.celeris-worktrees`）。
    pub fn worktree_dir(&self) -> PathBuf {
        let root = self
            .worktree
            .root
            .clone()
            .unwrap_or_else(|| self.remote_dir.join(".celeris-worktrees"));
        root.join(&self.task_id)
    }

    /// worktree のブランチ名（ADR-0019 D2: celeris は commit しない。人が見てから扱う）。
    pub fn worktree_branch(&self) -> String {
        format!("{}{}", self.worktree.branch_prefix, self.task_id)
    }
}

/// `sh` に渡す 1 引数としての安全な引用（シングルクォートで囲み、中の `'` を `'\''` にする）。
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// ADR-0059 D2: リモート側のパスをシェルの引数として安全に書く。先頭の `~`（単独）または `~/…` だけを
/// `"$HOME"`（クォート済みのシェル変数）に展開する（`~user` は展開しない。ADR-0039 D5 の
/// `expand_home` と同じ方針: celeris は他ユーザの home を知らない）。それ以外はこれまでどおり `shq`
/// でシングルクォート引用する。worktree 準備・`mkdir`・`cd`・`.celeris/remote-exec` の `cd` など、
/// クラスタ側のパスをスクリプトに埋め込むすべての箇所がこれを通る。
fn shell_remote_path(raw: &str) -> String {
    if raw == "~" {
        return "\"$HOME\"".to_string();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return format!("\"$HOME\"{}", shq(&format!("/{rest}")));
    }
    shq(raw)
}

/// ADR-0059 D6: `path`（タスクの workspace）と実効 `work_dir`（呼び出し側が DB 上書き > 設定 > 無し の
/// 順で決めたもの）から、クラスタで使うディレクトリを決める（純粋関数。celeris はこれ以上パスを
/// 書き換えない）。絶対パス（`/` 始まり）・`~`/`~/…` はそのまま使う（展開は上の `shell_remote_path`
/// が実行時に行う）。空、またはそれ以外の相対パスは `work_dir` からの相対にする。`work_dir` が無ければ
/// `None`（呼び出し側は `path` をそのまま返し、`remote_dir_is_resolved` で「解決できなかった」ことを
/// 検出する）。
pub fn resolve_remote_dir(path: &Path, work_dir: Option<&Path>) -> Option<PathBuf> {
    let raw = path.to_string_lossy();
    if raw.starts_with('/') || raw.starts_with('~') {
        return Some(path.to_path_buf());
    }
    let base = work_dir?;
    if raw.is_empty() {
        Some(base.to_path_buf())
    } else {
        Some(base.join(path))
    }
}

/// ADR-0059 D6: `resolve_remote_dir` が解決できたかどうかを、結果の `PathBuf` から判定する
/// （`work_dir` が無いまま `path` をそのまま返した場合は絶対でも `~` 始まりでもない）。
pub fn remote_dir_is_resolved(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.starts_with('/') || raw.starts_with('~')
}

/// ローカルの作業ディレクトリを持ちつつ、コマンドをリモートで実行するワークスペース。
#[derive(Debug, Clone)]
pub struct SshWorkspace {
    local: LocalWorkspace,
    settings: SshSettings,
}

impl SshWorkspace {
    pub fn new(dir: impl Into<PathBuf>, settings: SshSettings) -> Self {
        Self {
            local: LocalWorkspace::new(dir),
            settings,
        }
    }

    pub fn dir(&self) -> &Path {
        self.local.dir()
    }

    pub fn settings(&self) -> &SshSettings {
        &self.settings
    }

    /// ADR-0019 D1: 同期とコマンド実行の対象。`worktree` のときは worktree のパス、それ以外は `remote_dir`。
    pub fn effective_remote_dir(&self) -> PathBuf {
        self.settings.effective_remote_dir()
    }

    /// worktree のパス（`worktree_root`/`<task_id>`。既定の root は `<project>/.celeris-worktrees`）。
    fn worktree_dir(&self) -> PathBuf {
        self.settings.worktree_dir()
    }

    /// worktree のブランチ名（ADR-0019 D2: celeris は commit しない。人が見てから扱う）。
    pub fn worktree_branch(&self) -> String {
        self.settings.worktree_branch()
    }

    /// ADR-0019 D1: クラスタ側に worktree を用意する（既にあれば再利用）。sparse-checkout の指定があれば絞る。
    async fn ensure_worktree(&self) -> Result<(), WorkspaceError> {
        if self.settings.task_id.is_empty() {
            return Err(WorkspaceError::Remote(
                "sync = \"worktree\" needs the task id (the worktree directory and branch are named after it)".to_string(),
            ));
        }
        let project = self.settings.remote_dir.to_string_lossy().to_string();
        let wt = self.worktree_dir().to_string_lossy().to_string();
        let branch = self.worktree_branch();
        let base = &self.settings.worktree.base;
        let mut inner = format!(
            "if [ ! -e {wt}/.git ]; then git worktree add -B {branch} {wt} {base} >/dev/null; fi\n",
            wt = shell_remote_path(&wt),
            branch = shq(&branch),
            base = shq(base),
        );
        if !self.settings.worktree.paths.is_empty() {
            let paths: Vec<String> = self
                .settings
                .worktree
                .paths
                .iter()
                .map(|p| shq(p))
                .collect();
            inner.push_str(&format!(
                "git -C {wt} sparse-checkout set --cone {paths} >/dev/null\n",
                wt = shell_remote_path(&wt),
                paths = paths.join(" ")
            ));
        }
        // 同じリポジトリに対して複数のタスクが同時に worktree を作ることがある（クラスタの並列度 > 1）。
        // git の worktree 管理は共有なので、あれば `flock` で直列化する（無ければそのまま実行する）。
        let script = format!(
            "set -e\n\
             cd {project} 2>/dev/null || {{ echo \"no such directory: {project_display}\" >&2; exit 66; }}\n\
             gitdir=$(git rev-parse --git-common-dir 2>/dev/null) || {{ echo \"not a git repository\" >&2; exit 65; }}\n\
             if command -v flock >/dev/null 2>&1; then\n\
               exec 9>\"$gitdir/celeris-worktree.lock\"\n\
               flock 9\n\
             fi\n\
             {inner}",
            project = shell_remote_path(&project),
            project_display = project,
            inner = inner,
        );
        let out = self.run_ssh(&script, Duration::from_secs(600)).await?;
        match out.exit {
            Some(0) => Ok(()),
            // ADR-0059 D3: 専用のバリアントにする（呼び出し側が「格下げしてよいか」を型で判定できるように。
            // 文字列のパースはしない）。
            Some(65) => Err(WorkspaceError::NotAGitRepository(format!(
                "{project} on {} is not a git repository; use sync = \"rsync\" for this cluster (ADR-0019 D3), \
                 or leave `mode` unset with no `repos` on a command-only task to run it as `shared` (ADR-0059 D3)",
                self.settings.cluster
            ))),
            other => Err(WorkspaceError::Remote(format!(
                "cannot prepare the git worktree on {} (exit {other:?}): {}",
                self.settings.cluster,
                out.stderr_tail.trim()
            ))),
        }
    }

    /// `ssh` に必ず付ける引数（対話的な認証を禁じる）。
    fn ssh_base(&self) -> Vec<String> {
        let mut args = self.settings.ssh_command.clone();
        args.push("-o".into());
        args.push("BatchMode=yes".into());
        args
    }

    /// 人が張った多重接続があるか（ADR-0018 D2）。無ければ celeris は何もできない。
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
        script.push_str(&format!(
            "cd {} && ",
            shell_remote_path(&self.effective_remote_dir().to_string_lossy())
        ));
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
        // worktree のときは `git worktree add` が作るので、ここでは作らない。
        if self.settings.sync == SyncMode::Worktree {
            return self.ensure_worktree().await;
        }
        let dir = self.effective_remote_dir().to_string_lossy().to_string();
        let out = self
            .run_ssh(
                &format!("mkdir -p {}", shell_remote_path(&dir)),
                Duration::from_secs(60),
            )
            .await?;
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
        // P-46: celeris の管理用ディレクトリ（ラッパ・run のログ・入力）はクラスタへ送らない。
        for pattern in SYNC_ALWAYS_EXCLUDED {
            args.push("--exclude".into());
            args.push(pattern.into());
        }
        for pattern in &self.settings.rsync_excludes {
            args.push("--exclude".into());
            args.push(pattern.clone());
        }
        args.push(format!("{}/", self.local.dir().to_string_lossy()));
        args.push(format!(
            "{}:{}/",
            self.settings.host,
            self.effective_remote_dir().to_string_lossy()
        ));
        self.run_rsync(&args, "push").await
    }

    /// クラスタ → 手元の写し（run の前と、判定の後。ADR-0018 D4）。
    /// 写しは celeris が作り直してよいので、こちらは `--delete` してよい。
    pub async fn pull(&self) -> Result<(), WorkspaceError> {
        if self.settings.sync == SyncMode::None {
            return Ok(());
        }
        self.ensure_remote_dir().await?;
        let mut args = self.settings.rsync_command.clone();
        args.extend(["-a".into(), "--delete".into()]);
        args.push("-e".into());
        args.push(self.ssh_base().join(" "));
        // P-46: `--delete` で手元の run のログ・入力を消さない（クラスタ側には無いため）。
        for pattern in SYNC_ALWAYS_EXCLUDED {
            args.push("--exclude".into());
            args.push(pattern.into());
        }
        for pattern in &self.settings.rsync_excludes {
            args.push("--exclude".into());
            args.push(pattern.clone());
        }
        args.push(format!(
            "{}:{}/",
            self.settings.host,
            self.effective_remote_dir().to_string_lossy()
        ));
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

    /// ワーカーがクラスタでコマンドを実行するためのラッパ `.celeris/remote-exec`（ADR-0018 D3、
    /// ADR-0059 D5 で `.taskd/remote-exec` から改名）。
    pub async fn write_remote_exec_helper(&self) -> Result<PathBuf, WorkspaceError> {
        // ADR-0059 D5: 改名前の古いラッパが写しに残っていたら消す（新しいものと混同しないため。
        // `.taskd/artifacts/<task_id>/` は ADR-0036 D1 の別の規約なので触らない）。
        let old = self.local.dir().join(".taskd").join("remote-exec");
        let _ = tokio::fs::remove_file(&old).await;
        let dir = self.local.dir().join(".celeris");
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join("remote-exec");
        let ssh = self.ssh_base().join(" ");
        let remote = self.effective_remote_dir().to_string_lossy().to_string();
        let mut prefix = String::new();
        for (k, v) in &self.settings.env {
            prefix.push_str(&format!("export {k}={} && ", shq(v)));
        }
        for line in &self.settings.setup {
            prefix.push_str(&format!("{{ {line}; }} && "));
        }
        let script = format!(
            "#!/bin/sh\n\
             # celeris が run ごとに作るラッパ（ADR-0018 D3）。クラスタ {cluster} でコマンドを実行する。\n\
             # 使い方: .celeris/remote-exec <コマンド ...>\n\
             set -u\n\
             if [ $# -eq 0 ]; then echo \"usage: $0 <command...>\" >&2; exit 2; fi\n\
             cmd=\"$*\"\n\
             exec {ssh} {host} \"cd {remote_q} && {prefix}sh -c \\\"$cmd\\\"\"\n",
            cluster = self.settings.cluster,
            ssh = ssh,
            host = self.settings.host,
            remote_q = shell_remote_path(&remote),
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

/// ワーカーへ渡す指示文（ADR-0018 D3）。`RunRequest.task.objective` の末尾に足し、`.celeris/remote-exec`
/// の存在と使い方を伝える（ADR-0059 D5 で `.taskd/remote-exec` から改名）。
/// ワーカーが従うかは保証しない（受け入れ条件はクラスタ側で判定されるので、手元だけで済ませた仕事は条件で落ちる）。
pub fn remote_exec_instructions(settings: &SshSettings) -> String {
    let base = format!(
        "\n\n[celeris] このタスクの正はクラスタ `{cluster}`（ssh host `{host}`）の `{dir}` です。手元の作業ディレクトリはその写しで、\
         run の後にクラスタへ同期され、受け入れ条件のコマンドはクラスタ側で実行されます。\
         重い処理・クラスタ上のデータやモジュールを使う処理は `.celeris/remote-exec <コマンド ...>` で実行してください\
         （クラスタの作業ディレクトリで実行され、終了コードと出力がそのまま返ります）。",
        cluster = settings.cluster,
        host = settings.host,
        dir = settings.effective_remote_dir().to_string_lossy(),
    );
    // ADR-0019 D1/D3: worktree では、手元に来ているのは追跡ファイルだけで、元のリポジトリは触らない。
    if settings.sync != SyncMode::Worktree {
        return base;
    }
    format!(
        "{base}\n\
         これは `{project}` から切り出した git worktree（ブランチ `{branch}`）です。追跡ファイルだけが入っているので、\
         手元に見えないファイル（未追跡の巨大データなど）はクラスタ側にあります。元のリポジトリの作業ツリーは触らないでください。",
        project = settings.remote_dir.to_string_lossy(),
        branch = settings.worktree_branch(),
    )
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

/// ADR-0062 A（Phase 107）: master 越しの実通信で生存を確定させる（`ssh -o BatchMode=yes <host> -- true`）。
/// `control_master_alive_blocking`（`-O check`）は unix ソケットを見るだけなので、NAT / ファイアウォールの
/// idle timeout で TCP が黙って死んでいても「Master running」を返し続ける。こちらは実際にリモートへ
/// コマンドを 1 つ流すので確定できる。`timeout` を超えたら子を kill して `false`（`std::process::Command`
/// を使う同期の実装。tick ループからは `run_cluster_hooks_off_async` 経由で OS スレッドに逃がして呼ぶ）。
pub fn control_master_command_probe_blocking(
    ssh_command: &[String],
    host: &str,
    timeout: Duration,
) -> bool {
    let Some((program, rest)) = ssh_command.split_first() else {
        return false;
    };
    let mut child = match std::process::Command::new(program)
        .args(rest)
        .args(["-o", "BatchMode=yes", host, "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
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
        } else if self.settings.sync == SyncMode::Worktree {
            // push を挟まない設定でも worktree だけは用意する（無ければ `cd` で落ちる）。
            self.ensure_remote_dir().await?;
        }
        let script = self.remote_script(cmd, timeout);
        // ssh 自体のタイムアウトは、リモートの timeout より少し長くする。
        let result = self
            .run_ssh(&script, timeout + Duration::from_secs(30))
            .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ADR-0059 D2: `~` の展開（純粋関数） ----

    #[test]
    fn shell_remote_path_expands_only_a_leading_tilde() {
        assert_eq!(shell_remote_path("~"), "\"$HOME\"");
        assert_eq!(shell_remote_path("~/work/project"), "\"$HOME\"'/work/project'");
        // `~user` は展開しない（celeris は他ユーザの home を知らない。ADR-0039 D5 と同じ方針）。
        assert_eq!(shell_remote_path("~user/x"), "'~user/x'");
        // それ以外は従来どおり `shq`。
        assert_eq!(shell_remote_path("/work/x"), "'/work/x'");
        assert_eq!(shell_remote_path("relative/x"), "'relative/x'");
        assert_eq!(shell_remote_path("it's/quoted"), "'it'\\''s/quoted'");
    }

    // ---- ADR-0059 D6: `work_dir` からの相対解決（純粋関数） ----

    #[test]
    fn resolve_remote_dir_keeps_absolute_and_tilde_paths_untouched() {
        let work_dir = Some(Path::new("/work/NBB/rmaeda"));
        assert_eq!(
            resolve_remote_dir(Path::new("/scratch/x"), work_dir),
            Some(PathBuf::from("/scratch/x"))
        );
        assert_eq!(
            resolve_remote_dir(Path::new("~"), work_dir),
            Some(PathBuf::from("~"))
        );
        assert_eq!(
            resolve_remote_dir(Path::new("~/work"), work_dir),
            Some(PathBuf::from("~/work"))
        );
        // `work_dir` が無くても絶対・`~` はそのまま解決できる。
        assert_eq!(
            resolve_remote_dir(Path::new("/scratch/x"), None),
            Some(PathBuf::from("/scratch/x"))
        );
    }

    #[test]
    fn resolve_remote_dir_resolves_empty_and_relative_paths_against_work_dir() {
        let work_dir = Some(Path::new("/work/NBB/rmaeda"));
        assert_eq!(
            resolve_remote_dir(Path::new(""), work_dir),
            Some(PathBuf::from("/work/NBB/rmaeda"))
        );
        assert_eq!(
            resolve_remote_dir(Path::new("benchfs"), work_dir),
            Some(PathBuf::from("/work/NBB/rmaeda/benchfs"))
        );
    }

    #[test]
    fn resolve_remote_dir_is_none_when_there_is_no_work_dir_to_resolve_against() {
        assert_eq!(resolve_remote_dir(Path::new(""), None), None);
        assert_eq!(resolve_remote_dir(Path::new("benchfs"), None), None);
    }

    #[test]
    fn remote_dir_is_resolved_matches_absolute_and_tilde_only() {
        assert!(remote_dir_is_resolved(Path::new("/work/x")));
        assert!(remote_dir_is_resolved(Path::new("~")));
        assert!(remote_dir_is_resolved(Path::new("~/work")));
        assert!(!remote_dir_is_resolved(Path::new("")));
        assert!(!remote_dir_is_resolved(Path::new("relative")));
    }

    // ---- ADR-0018 D5 / ADR-0059: `SyncMode::None` は同期を一切しない ----

    #[tokio::test]
    async fn sync_none_push_and_pull_never_run_rsync_or_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = SshSettings::new("c", "h", PathBuf::from("/work/x"));
        settings.sync = SyncMode::None;
        // 呼ばれたら即座にエラーで分かるように、実在しないプログラムを指す。
        settings.ssh_command = vec!["/nonexistent/ssh-should-not-run".into()];
        settings.rsync_command = vec!["/nonexistent/rsync-should-not-run".into()];
        let ws = SshWorkspace::new(dir.path(), settings);
        ws.push().await.expect("push is a no-op under SyncMode::None");
        ws.pull().await.expect("pull is a no-op under SyncMode::None");
    }

    // ---- ADR-0059 D3: worktree 準備の exit 65 を型で区別する ----

    #[tokio::test]
    async fn ensure_worktree_maps_exit_65_to_not_a_git_repository_and_exit_66_to_remote() {
        let dir = tempfile::tempdir().unwrap();
        for (exit_code, matches_not_a_git_repo) in [(65, true), (66, false), (1, false)] {
            let stub = dir.path().join(format!("stub-{exit_code}.sh"));
            std::fs::write(&stub, format!("#!/bin/sh\nexit {exit_code}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&stub).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&stub, perms).unwrap();
            }
            let mut settings = SshSettings::new("c", "h", PathBuf::from("/work/proj"));
            settings.sync = SyncMode::Worktree;
            settings.task_id = "01TESTTASK".into();
            settings.ssh_command = vec![stub.to_string_lossy().into_owned()];
            let mirror = dir.path().join(format!("mirror-{exit_code}"));
            let ws = SshWorkspace::new(&mirror, settings);
            let err = ws.ensure_worktree().await.expect_err("stub always fails");
            assert_eq!(
                matches!(err, WorkspaceError::NotAGitRepository(_)),
                matches_not_a_git_repo,
                "exit {exit_code}: {err:?}"
            );
        }
    }

    // ---- ADR-0059 D5: `.taskd/remote-exec` -> `.celeris/remote-exec` ----

    #[tokio::test]
    async fn write_remote_exec_helper_writes_celeris_and_cleans_up_the_old_taskd_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let mirror = dir.path().join("mirror");
        tokio::fs::create_dir_all(mirror.join(".taskd")).await.unwrap();
        tokio::fs::write(mirror.join(".taskd").join("remote-exec"), "old wrapper")
            .await
            .unwrap();

        let mut settings = SshSettings::new("pegasus", "pegasus", PathBuf::from("~/work/proj"));
        settings.sync = SyncMode::None;
        let ws = SshWorkspace::new(&mirror, settings);
        let path = ws.write_remote_exec_helper().await.expect("write helper");
        assert_eq!(path, mirror.join(".celeris").join("remote-exec"));
        assert!(path.is_file());
        assert!(
            !mirror.join(".taskd").join("remote-exec").exists(),
            "the old wrapper must be removed"
        );

        // ADR-0059 D2: `~/work/proj` は `.celeris/remote-exec` の生成スクリプトの中で `"$HOME"` に展開される。
        let script = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(script.contains("\"$HOME\"'/work/proj'"), "{script}");
    }

    // ---- ADR-0018 D3 / ADR-0059 D5: `.celeris/` は同期から常に除外し、`.taskd/` も後方互換で残す ----

    #[test]
    fn sync_always_excluded_keeps_the_legacy_taskd_alongside_celeris() {
        assert!(SYNC_ALWAYS_EXCLUDED.contains(&".taskd/"));
        assert!(SYNC_ALWAYS_EXCLUDED.contains(&".celeris/"));
    }

    // ---- ADR-0062 A（Phase 107）: 実通信 probe（`ssh -o BatchMode=yes <host> -- true`） ----

    fn fake_ssh_probe(dir: &Path, script: &str) -> Vec<String> {
        let path = dir.join("ssh");
        crate::test_support::write_executable(&path, script);
        vec![path.to_string_lossy().into_owned()]
    }

    #[test]
    fn command_probe_succeeds_when_the_remote_command_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh_probe(dir.path(), "#!/bin/sh\nexit 0\n");
        assert!(control_master_command_probe_blocking(
            &ssh,
            "cluster-host",
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn command_probe_fails_when_the_remote_command_exits_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh_probe(dir.path(), "#!/bin/sh\nexit 255\n");
        assert!(!control_master_command_probe_blocking(
            &ssh,
            "cluster-host",
            Duration::from_secs(2)
        ));
    }

    /// NAT の idle timeout で TCP が黙って死んだ状況を模す: ssh が応答せずハングし続ける。
    /// `timeout` を超えたら kill されて `false` になる（ハングしたまま残らない）。
    #[test]
    fn command_probe_times_out_and_kills_a_hanging_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let script = format!(
            "#!/bin/sh\necho $$ > {state:?}/pid\nwhile true; do sleep 3600; done\n",
            state = state.path()
        );
        let ssh = fake_ssh_probe(dir.path(), &script);
        let start = std::time::Instant::now();
        assert!(!control_master_command_probe_blocking(
            &ssh,
            "cluster-host",
            Duration::from_millis(300)
        ));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "probe must not block past the timeout"
        );
        let pid: u32 = std::fs::read_to_string(state.path().join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..150 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("hanging ssh process {pid} was not killed");
    }
}
