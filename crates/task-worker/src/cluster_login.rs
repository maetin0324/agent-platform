//! クラスタへの接続を GUI から張る（ADR-0032）。
//!
//! `SshWorkspace`（`ssh.rs`）は「人が張った ControlMaster を借りる」だけだった（ADR-0018 D2）。
//! ここではその借り先を celeris 自身が用意する: `ssh -M -N` の子プロセスを celeris が**保持し続ける**ことで
//! master を張る（`-f` は使わない。`ControlPersist` に依存しないため。ADR-0032 D2）。
//!
//! - `auth = "publickey"`（`interactive = false`）: `BatchMode=yes` で鍵だけの接続を試みる。
//! - `auth = "totp"`（`interactive = true`）: `SSH_ASKPASS` 経由でプロンプトと検証コードを GUI と中継する
//!   （ADR-0032 D4）。コードはメモリと FIFO（カーネルのパイプバッファ）だけを通り、ディスクには残らない。

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::claude_account::{READER_JOIN_TIMEOUT, join_with_timeout, pump_reader, truncate_detail};
use crate::ssh::control_master_alive_blocking;
use crate::subprocess::send_signal_to_group;

/// `-O check` をポーリングする間隔（ADR-0032 §1「実機で確かめた事実」: `-O check` は即座に返る）。
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// celeris が保持する ssh master。Drop でプロセスグループごと落とす。
pub struct ClusterMaster {
    child: Child,
}

impl std::fmt::Debug for ClusterMaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterMaster").field("pid", &self.child.id()).finish()
    }
}

impl Drop for ClusterMaster {
    fn drop(&mut self) {
        send_signal_to_group(&self.child, Signal::SIGKILL);
    }
}

/// `start_connect` の結果。
#[derive(Debug)]
pub enum ClusterConnectStart {
    /// コード無しで接続できた。人が張った master を見つけた場合は `None`。
    Connected(Option<ClusterMaster>),
    /// ssh がプロンプトを出した。`prompt` をそのまま GUI に見せる。
    NeedsCode { prompt: String, session: ClusterConnectSession },
}

/// 進行中の TOTP 接続セッション（ADR-0032 D4）。一時ディレクトリ・FIFO・askpass の子プロセスを保持する。
pub struct ClusterConnectSession {
    child: Option<Child>,
    ssh_command: Vec<String>,
    host: String,
    code_fifo: PathBuf,
    /// 0700 の一時ディレクトリ（FIFO と askpass スクリプトを置く）。Drop で消す。
    dir: PathBuf,
    started_at: Instant,
    stderr_buf: Arc<Mutex<Vec<u8>>>,
    err_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ClusterConnectSession {
    /// プロンプト文字列はここには保持していないが、慣習として `<redacted>` を出す
    /// （`claude_account.rs::LoginSession` の `Debug` と同じ規律。ADR-0032 D4）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterConnectSession")
            .field("host", &self.host)
            .field("started_at", &self.started_at)
            .field("prompt", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Drop for ClusterConnectSession {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            send_signal_to_group(&child, Signal::SIGKILL);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl ClusterConnectSession {
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// コードを FIFO へ渡し、`wait` 以内の `-O check` の成功を待つ（ADR-0032 D4）。
    ///
    /// N6 相当: `trim` して空、または制御文字を含むコードは ssh に渡さずに `InvalidCode` を返す
    /// （`claude_account.rs::LoginSession::submit_code` と同じ注入防止）。この場合 `self` はここで
    /// 消費され、`Drop` がプロセスグループを kill し一時ディレクトリを消す（ssh には何も渡らない）。
    /// 戻り値が `None` なのは失敗ではない。ssh が `ControlPersist` で自分を切り離したため
    /// **保持すべき子プロセスが無い**という意味（接続自体は成立している。上の実機の注記を見よ）。
    pub async fn submit_code(
        mut self,
        code: &str,
        wait: Duration,
    ) -> Result<Option<ClusterMaster>, ClusterConnectError> {
        let trimmed = code.trim();
        if trimmed.is_empty() || trimmed.chars().any(|c| c.is_control()) {
            return Err(ClusterConnectError::InvalidCode);
        }

        let code_fifo = self.code_fifo.clone();
        let payload = trimmed.to_string();
        let write_ok = matches!(
            tokio::task::spawn_blocking(move || write_code_fifo(&code_fifo, &payload)).await,
            Ok(Ok(()))
        );
        if !write_ok {
            let detail = self.stderr_detail();
            self.cancel().await;
            return Err(ClusterConnectError::Failed(detail));
        }

        let ssh_command = self.ssh_command.clone();
        let host = self.host.clone();
        let Some(child) = self.child.as_mut() else {
            let detail = self.stderr_detail();
            self.cancel().await;
            return Err(ClusterConnectError::Failed(detail));
        };
        let outcome = poll_until_connected_or_timeout(&ssh_command, &host, child, wait).await;
        match outcome {
            PollOutcome::Connected => Ok(self.into_master()),
            PollOutcome::TimedOut | PollOutcome::ChildExited => {
                let detail = self.stderr_detail();
                self.cancel().await;
                Err(ClusterConnectError::Failed(detail))
            }
        }
    }

    /// 進行中の接続を取り消す（ADR-0032 D5: `DELETE /clusters/{id}/connect`）。
    /// プロセスグループを落とし、一時ディレクトリを消す。
    pub async fn cancel(mut self) {
        if let Some(mut child) = self.child.take() {
            send_signal_to_group(&child, Signal::SIGKILL);
            let _ = child.wait().await;
        }
        if let Some(t) = self.err_task.take() {
            join_with_timeout(t, READER_JOIN_TIMEOUT).await;
        }
        let dir = self.dir.clone();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir)).await;
    }

    /// 認証済みの child を `ClusterMaster` として取り出す。一時ディレクトリは（`self` の残りとともに）
    /// この関数を抜けたときの `Drop` で消える（child は既に取り出しているので kill はされない）。
    /// 生きている子だけを `ClusterMaster` にする。ssh が切り離した後の抜け殻を掴むと、
    /// `Drop` が既に終了したプロセスグループへ signal を送るだけの無意味な保持になる。
    fn into_master(mut self) -> Option<ClusterMaster> {
        self.child.take().and_then(master_if_alive)
    }

    fn stderr_detail(&self) -> String {
        let buf = self.stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        truncate_detail(&String::from_utf8_lossy(&buf))
    }
}

/// `start_connect` の失敗。
#[derive(Debug)]
pub enum ClusterConnectError {
    Spawn(String),
    Timeout,
    Failed(String),
    InvalidCode,
}

impl std::fmt::Display for ClusterConnectError {
    /// ADR-0032 D4: **検証コードは決してここに現れない**（`InvalidCode` は値を持たず、`Failed` に入れるのは
    /// ssh の stderr だけ）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(detail) => write!(f, "could not start ssh: {detail}"),
            Self::Timeout => write!(f, "timed out waiting for ssh"),
            Self::Failed(detail) => write!(f, "ssh did not connect: {detail}"),
            Self::InvalidCode => write!(f, "the verification code was empty or contained control characters"),
        }
    }
}

impl std::error::Error for ClusterConnectError {}

/// 接続を開始する（ADR-0032 D2/D3/D4）。
///
/// 1. 既に人（または以前の celeris）が張った master が生きていれば、何もせず `Connected(None)`。
/// 2. `interactive == false`（`auth = "publickey"`）: `BatchMode=yes` で `ssh -M -N` を張り、
///    `connect_timeout` 以内に `-O check` が通れば `Connected(Some(master))`。
/// 3. `interactive == true`（`auth = "totp"`）: `SSH_ASKPASS` を使って `ssh -M -N` を張り、
///    プロンプトが出れば `NeedsCode`。`prompt_timeout` 内に出なければ、鍵だけで入れたか確かめる。
pub async fn start_connect(
    ssh_command: &[String],
    host: &str,
    interactive: bool,
    prompt_timeout: Duration,
    connect_timeout: Duration,
) -> Result<ClusterConnectStart, ClusterConnectError> {
    if check_master(ssh_command, host).await {
        return Ok(ClusterConnectStart::Connected(None));
    }
    if interactive {
        start_totp(ssh_command, host, prompt_timeout).await
    } else {
        start_publickey(ssh_command, host, connect_timeout).await
    }
}

/// 接続を切る。`ssh -O exit <host>` を `BatchMode=yes` で呼ぶ（ADR-0032 D5: `DELETE /clusters/{id}/connect`）。
pub async fn disconnect(ssh_command: &[String], host: &str) -> Result<(), ClusterConnectError> {
    let (program, rest) =
        ssh_command.split_first().ok_or_else(|| ClusterConnectError::Spawn("empty ssh_command".to_string()))?;
    let mut args: Vec<String> = rest.to_vec();
    args.push("-o".into());
    args.push("BatchMode=yes".into());
    args.push("-O".into());
    args.push("exit".into());
    args.push(host.to_string());
    let output = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| ClusterConnectError::Spawn(e.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ClusterConnectError::Failed(truncate_detail(&String::from_utf8_lossy(&output.stderr))))
    }
}

/// ADR-0032 D3: `auth = "publickey"`。`BatchMode=yes` で askpass を使わずに張る。
async fn start_publickey(
    ssh_command: &[String],
    host: &str,
    connect_timeout: Duration,
) -> Result<ClusterConnectStart, ClusterConnectError> {
    let (program, rest) =
        ssh_command.split_first().ok_or_else(|| ClusterConnectError::Spawn("empty ssh_command".to_string()))?;
    let mut args: Vec<String> = rest.to_vec();
    args.push("-o".into());
    args.push("BatchMode=yes".into());
    args.push("-M".into());
    args.push("-N".into());
    args.push(host.to_string());

    let (mut child, stderr_buf, err_task) = spawn_master(program, &args, &[])?;
    match poll_until_connected_or_timeout(ssh_command, host, &mut child, connect_timeout).await {
        PollOutcome::Connected => {
            // 接続できたので、stderr の中継タスクは master の寿命の間そのまま走らせておく
            // （もう監視する必要はない。参照を手放すだけで abort はしない）。
            drop(err_task);
            // ssh が自分を切り離した（`ControlPersist` あり）場合、この子はもう終了している。
            // そのときは持つべき master が無い（接続は切り離された側が持っている）ので `None` を返す。
            // 切るときは `ssh -O exit` を使う（`disconnect`）。
            Ok(ClusterConnectStart::Connected(master_if_alive(child)))
        }
        PollOutcome::TimedOut | PollOutcome::ChildExited => {
            let detail = finish_stderr(&mut child, stderr_buf, err_task).await;
            Err(ClusterConnectError::Failed(detail))
        }
    }
}

/// ADR-0032 D4: `auth = "totp"`。`SSH_ASKPASS` でプロンプトとコードを中継する。
async fn start_totp(
    ssh_command: &[String],
    host: &str,
    prompt_timeout: Duration,
) -> Result<ClusterConnectStart, ClusterConnectError> {
    let dir = make_secure_tempdir().map_err(|e| ClusterConnectError::Spawn(format!("tempdir: {e}")))?;
    let prompt_fifo = dir.join("prompt");
    let code_fifo = dir.join("code");
    if let Err(e) = make_fifo(&prompt_fifo).and_then(|()| make_fifo(&code_fifo)) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    let askpass = match write_askpass_script(&dir) {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };

    let (program, rest) = match ssh_command.split_first() {
        Some(v) => v,
        None => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(ClusterConnectError::Spawn("empty ssh_command".to_string()));
        }
    };
    let mut args: Vec<String> = rest.to_vec();
    args.push("-M".into());
    args.push("-N".into());
    args.push(host.to_string());
    // BatchMode=yes は付けない（付けると askpass が呼ばれない。ADR-0032 §1「実機で確かめた事実」）。
    let envs = vec![
        ("SSH_ASKPASS".to_string(), askpass.to_string_lossy().into_owned()),
        ("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string()),
        ("DISPLAY".to_string(), String::new()),
        ("CELERIS_PROMPT_FIFO".to_string(), prompt_fifo.to_string_lossy().into_owned()),
        ("CELERIS_CODE_FIFO".to_string(), code_fifo.to_string_lossy().into_owned()),
    ];

    let (child, stderr_buf, err_task) = match spawn_master(program, &args, &envs) {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };

    let session = ClusterConnectSession {
        child: Some(child),
        ssh_command: ssh_command.to_vec(),
        host: host.to_string(),
        code_fifo,
        dir,
        started_at: Instant::now(),
        stderr_buf,
        err_task: Some(err_task),
    };

    // N: `read_prompt_blocking` は自前の `deadline` でポーリングして戻ってくる（`prompt_timeout` を
    // 超えて OS スレッドがブロックされたまま残ることがない）。`tokio::select!` で外側から競わせて
    // 見捨てる方式は、tokio のブロッキングスレッドが（呼び手が待つのをやめても）決して終わらないままに
    // なりうる（`ssh` が二度と askpass を呼ばない場合。実際にテストで確認した）ので採らない。
    let deadline = Instant::now() + prompt_timeout;
    let read_prompt_path = prompt_fifo;
    let read_result = tokio::task::spawn_blocking(move || read_prompt_blocking(&read_prompt_path, deadline)).await;

    match read_result {
        Ok(Ok(Some(prompt))) => Ok(ClusterConnectStart::NeedsCode { prompt, session }),
        Ok(Ok(None)) => {
            // プロンプトが所定の時間来なかった: 鍵だけで入れたかもしれないので `-O check` で判定する
            // （ADR-0032 D4 手順 3）。ここでは 1 回だけ見る（ポーリングはしない）。
            if check_master(&session.ssh_command, &session.host).await {
                match session.into_master() {
                    Some(master) => Ok(ClusterConnectStart::Connected(Some(master))),
                    None => Err(ClusterConnectError::Failed(
                        "cluster connect session lost its child process".to_string(),
                    )),
                }
            } else {
                session.cancel().await;
                Err(ClusterConnectError::Timeout)
            }
        }
        _ => {
            // FIFO を開けなかった／読めなかった: 接続の試み自体を失敗として扱う。
            let detail = session.stderr_detail();
            session.cancel().await;
            Err(ClusterConnectError::Failed(detail))
        }
    }
}

/// `spawn_master` の戻り値: 子プロセス・stderr の蓄積バッファ・それを汲み出すタスクのハンドル。
type MasterSpawn = (Child, Arc<Mutex<Vec<u8>>>, JoinHandle<()>);

/// `ssh -M -N` を起動し、stderr を非同期に汲み出す（`stdout` は使わないので捨てる）。
fn spawn_master(program: &str, args: &[String], envs: &[(String, String)]) -> Result<MasterSpawn, ClusterConnectError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(envs.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|e| ClusterConnectError::Spawn(e.to_string()))?;
    let stderr = child.stderr.take();
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let task_buf = buf.clone();
    let err_task = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            pump_reader(stderr, task_buf).await;
        }
    });
    Ok((child, buf, err_task))
}

/// 失敗として終える: プロセスグループを落とし、stderr の読み取りタスクを（上限付きで）待ってから
/// 人が読める一行の手がかりにする（ADR-0032 D4: コードは含めない。ssh の stderr にはそもそも入らない）。
async fn finish_stderr(child: &mut Child, stderr_buf: Arc<Mutex<Vec<u8>>>, err_task: JoinHandle<()>) -> String {
    send_signal_to_group(child, Signal::SIGKILL);
    let _ = child.wait().await;
    join_with_timeout(err_task, READER_JOIN_TIMEOUT).await;
    let buf = stderr_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
    truncate_detail(&String::from_utf8_lossy(&buf))
}

enum PollOutcome {
    Connected,
    TimedOut,
    ChildExited,
}

/// `-O check` を `CHECK_POLL_INTERVAL` 間隔でポーリングしつつ、子の終了も同時に見る（ADR-0032 D2/D4）。
/// 子がまだ生きていれば `ClusterMaster` として保持する。既に終了していれば `None`
/// （ssh が `ControlPersist` で master を切り離した後。接続は生きているが、こちらに持ち物は無い）。
fn master_if_alive(mut child: Child) -> Option<ClusterMaster> {
    match child.try_wait() {
        Ok(Some(_)) => None,
        _ => Some(ClusterMaster { child }),
    }
}

async fn poll_until_connected_or_timeout(
    ssh_command: &[String],
    host: &str,
    child: &mut Child,
    timeout: Duration,
) -> PollOutcome {
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = status;
            // **実機で判明（2026-09-17、sirius）**: `~/.ssh/config` に `ControlPersist` があると、
            // ssh は認証が済んだ時点で**自分をバックグラウンドへ切り離す**（master は setsid して PPID 1 になり、
            // こちらが持っていた子プロセスは終了する）。つまり「子が終了した」は失敗とは限らない。
            // 必ずもう一度 `-O check` を見てから判定する（これを見ないと、接続できているのに失敗を返す）。
            return if check_master(ssh_command, host).await {
                PollOutcome::Connected
            } else {
                PollOutcome::ChildExited
            };
        }
        if check_master(ssh_command, host).await {
            return PollOutcome::Connected;
        }
        if start.elapsed() >= timeout {
            return PollOutcome::TimedOut;
        }
        tokio::time::sleep(CHECK_POLL_INTERVAL).await;
    }
}

/// `ssh.rs::control_master_alive_blocking` をブロッキングスレッドで呼ぶ（新しい async 版は書かない。
/// 判定は一本化する。ADR-0032）。
async fn check_master(ssh_command: &[String], host: &str) -> bool {
    let ssh_command = ssh_command.to_vec();
    let host = host.to_string();
    tokio::task::spawn_blocking(move || control_master_alive_blocking(&ssh_command, &host)).await.unwrap_or(false)
}

/// 0700 の一時ディレクトリを作る（ADR-0032 D4）。
fn make_secure_tempdir() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("celeris-cluster-connect-{}", task_core::TaskId::new()));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}

/// FIFO を作る。`mkfifo(3)` のモードは umask に削られるので、作った後に `chmod 0600` で確定させる。
fn make_fifo(path: &Path) -> Result<(), ClusterConnectError> {
    nix::unistd::mkfifo(path, nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR)
        .map_err(|e| ClusterConnectError::Spawn(format!("mkfifo {}: {e}", path.display())))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| ClusterConnectError::Spawn(format!("chmod {}: {e}", path.display())))
}

/// askpass スクリプト（ADR-0032 §1「実機で確かめた事実」で検証済みの中身と等価）。
fn write_askpass_script(dir: &Path) -> Result<PathBuf, ClusterConnectError> {
    let path = dir.join("askpass.sh");
    let script = "#!/bin/sh\nprintf '%s' \"$1\" > \"$CELERIS_PROMPT_FIFO\"\ncat \"$CELERIS_CODE_FIFO\"\n";
    std::fs::write(&path, script).map_err(|e| ClusterConnectError::Spawn(format!("askpass script: {e}")))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| ClusterConnectError::Spawn(format!("askpass permissions: {e}")))?;
    Ok(path)
}

/// プロンプト FIFO を読む。**実装上の判断**: 素朴に `File::open` + ブロッキング `read` で書くと、
/// askpass が一度も呼ばれないケース（`-O check` だけで判定がついてしまう等）で「誰も書き込まない FIFO の
/// open」が永遠にブロックし、その `spawn_blocking` のスレッドを誰も回収できなくなる
/// （tokio の runtime は drop 時にブロッキングタスクの終了を待つため、テストがハングした）。
/// そこで FIFO を `O_RDWR | O_NONBLOCK` で開く（Linux 拡張: `O_RDWR` は書き手が居なくても即座に返る。
/// 自分自身も書き手として保持することで、askpass がまだ繋がっていない間の `read` が
/// 「書き手が居ない＝EOF」と誤認されない。POSIX: 書き手が 1 つでもあれば、データが無いときの
/// `read` は `EAGAIN`）。`deadline` まで `EAGAIN` をポーリングし、来なければ `Ok(None)` で戻る
/// （呼び出し側は `-O check` で判定する）。プロンプトは askpass の 1 回の `printf` で書かれる想定
/// （ADR-0032 §1）なので、最初に読めたところで確定する。
fn read_prompt_blocking(path: &Path, deadline: Instant) -> std::io::Result<Option<String>> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().read(true).write(true).custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits()).open(path)?;
    let mut chunk = [0u8; 4096];
    loop {
        match file.read(&mut chunk) {
            Ok(n) => return Ok(Some(String::from_utf8_lossy(&chunk[..n]).into_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

/// コード FIFO に書いて、即座に unlink する（ADR-0032 D4: コードをディスクに残さない）。
fn write_code_fifo(path: &Path, code: &str) -> std::io::Result<()> {
    {
        let mut file = std::fs::OpenOptions::new().write(true).open(path)?;
        file.write_all(code.as_bytes())?;
        file.flush()?;
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::write_executable;

    async fn wait_until_process_gone(pid: u32) {
        for _ in 0..150 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("process {pid} is still alive");
    }

    fn fake_ssh(dir: &Path, name: &str, script: &str) -> Vec<String> {
        let path = dir.join(name);
        write_executable(&path, script);
        vec![path.to_string_lossy().into_owned()]
    }

    /// 偽 ssh の共通の骨組み: 引数を 1 つずつ見て `-M`/`-N`/`check`/`exit` の有無を判定してから分岐する
    /// （`case " $* " in *" -M "*" -N "*)` のような部分文字列マッチだと、`-M -N` の間の空白 1 個を
    /// 2 つの `*"..."` パターンで取り合えず必ず失敗する。トークンごとに見る方が確実）。
    fn preamble() -> &'static str {
        "is_master=0\n\
         is_check=0\n\
         is_exit=0\n\
         for a in \"$@\"; do\n  \
           case \"$a\" in\n    \
             -M) is_master=$((is_master + 1)) ;;\n    \
             -N) is_master=$((is_master + 1)) ;;\n    \
             check) is_check=1 ;;\n    \
             exit) is_exit=1 ;;\n  \
           esac\n\
         done\n"
    }

    /// `-O check` に一致するかどうかだけを見る（ADR-0018 の判定＝`control_master_alive_blocking` の
    /// 引数と同じなので、テストの偽 ssh もその形に合わせる）。
    fn always_ok_script() -> String {
        format!("#!/bin/sh\n{}if [ \"$is_check\" = 1 ]; then exit 0; fi\nexit 1\n", preamble())
    }

    /// `-M -N` で master を張り続け（ブロックし続け）、`-O check` は常に失敗する偽 ssh。
    /// master の pid を `$STATE/pid` に書く（結果が公開型に出てこないテストで使う）。
    fn never_authenticates_script(state: &Path) -> String {
        format!(
            "#!/bin/sh\nSTATE={state:?}\n{preamble}\
             if [ \"$is_master\" -ge 2 ]; then\n  \
               echo $$ > \"$STATE/pid\"\n  \
               while true; do sleep 3600; done\nfi\n\
             if [ \"$is_check\" = 1 ]; then exit 1; fi\n\
             exit 1\n",
            preamble = preamble(),
        )
    }

    /// `-M -N` で master を張り、張ってから `{delay_ms}` ms 経つと `-O check` が通るようになる偽 ssh
    /// （`auth = \"publickey\"` の接続がしばらくしてから確立する場合を模す）。
    fn delayed_success_script(state: &Path, delay_ms: u64) -> String {
        format!(
            "#!/bin/sh\nSTATE={state:?}\n{preamble}\
             if [ \"$is_master\" -ge 2 ]; then\n  \
               echo $$ > \"$STATE/pid\"\n  \
               date +%s%N > \"$STATE/started\"\n  \
               while true; do sleep 3600; done\nfi\n\
             if [ \"$is_check\" = 1 ]; then\n  \
               if [ ! -f \"$STATE/started\" ]; then exit 1; fi\n  \
               started=$(cat \"$STATE/started\")\n  \
               now=$(date +%s%N)\n  \
               elapsed_ms=$(( (now - started) / 1000000 ))\n  \
               if [ \"$elapsed_ms\" -ge {delay_ms} ]; then exit 0; else exit 1; fi\nfi\n\
             exit 1\n",
            preamble = preamble(),
        )
    }

    /// `SSH_ASKPASS` を呼び、返ってきたコードが `{expected_code}` と一致すれば以後 `-O check` が通る偽 ssh
    /// （`auth = \"totp\"` を模す）。
    fn askpass_script(state: &Path, prompt: &str, expected_code: &str) -> String {
        format!(
            "#!/bin/sh\nSTATE={state:?}\n{preamble}\
             if [ \"$is_master\" -ge 2 ]; then\n  \
               code=$(\"$SSH_ASKPASS\" \"{prompt}\")\n  \
               if [ \"$code\" = \"{expected_code}\" ]; then echo ok > \"$STATE/authed\"; fi\n  \
               while true; do sleep 3600; done\nfi\n\
             if [ \"$is_check\" = 1 ]; then\n  \
               if [ -f \"$STATE/authed\" ]; then exit 0; else exit 1; fi\nfi\n\
             exit 1\n",
            preamble = preamble(),
        )
    }

    // ---- 1. 生きている master をそのまま借りる ----

    #[tokio::test]
    async fn connected_without_spawning_when_master_already_alive() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), "ssh", &always_ok_script());
        let result =
            start_connect(&ssh, "cluster-host", false, Duration::from_millis(200), Duration::from_secs(2)).await;
        match result {
            Ok(ClusterConnectStart::Connected(master)) => assert!(master.is_none(), "master を借りるだけ"),
            other => panic!("expected Connected(None), got {other:?}"),
        }
    }

    // ---- 2. publickey、少し待ってから繋がる ----

    #[tokio::test]
    async fn publickey_connects_after_a_delay() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), "ssh", &delayed_success_script(state.path(), 300));
        let result =
            start_connect(&ssh, "cluster-host", false, Duration::from_millis(200), Duration::from_secs(5)).await;
        match result {
            Ok(ClusterConnectStart::Connected(Some(master))) => {
                let pid = master.child.id();
                drop(master);
                if let Some(pid) = pid {
                    wait_until_process_gone(pid).await;
                }
            }
            other => panic!("expected Connected(Some(_)), got {other:?}"),
        }
    }

    // ---- 3. publickey、connect_timeout で Failed、子が残らない ----

    #[tokio::test]
    async fn publickey_times_out_and_leaves_no_child() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), "ssh", &never_authenticates_script(state.path()));
        let result =
            start_connect(&ssh, "cluster-host", false, Duration::from_millis(300), Duration::from_secs(5)).await;
        match result {
            Err(ClusterConnectError::Failed(_)) => {}
            other => panic!("expected Failed, got {other:?}"),
        }
        let pid_file = state.path().join("pid");
        assert!(pid_file.is_file(), "master は一度は起動した");
        let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        wait_until_process_gone(pid).await;
    }

    // ---- 4. totp、プロンプトが出る ----

    #[tokio::test]
    async fn totp_needs_code_reports_the_exact_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let prompt = "(rmaeda@130.158.241.2) Verification code: ";
        let ssh = fake_ssh(dir.path(), "ssh", &askpass_script(state.path(), prompt, "123456"));
        let result =
            start_connect(&ssh, "cluster-host", true, Duration::from_secs(5), Duration::from_secs(5)).await.unwrap();
        match result {
            ClusterConnectStart::NeedsCode { prompt: got, session } => {
                assert_eq!(got, prompt);
                session.cancel().await;
            }
            other => panic!("expected NeedsCode, got {other:?}"),
        }
    }

    // ---- 5. totp、正しいコードで繋がる ----

    /// 偽 ssh が「認証が済んだら**自分を切り離して終了する**」（`ControlPersist` があるときの本物の挙動）。
    ///
    /// `-O check` は**前面の子が消えてから**しか成功しないようにしてある。こうしないと
    /// 「認証済みファイルを書いてから exit するまでの隙間」で普通の成功経路を通ってしまい、
    /// 肝心の「子の終了を観測した後」の分岐を踏まないテストになる（実際に一度そうなった）。
    fn daemonizing_askpass_script(state: &Path, prompt: &str, expected_code: &str) -> String {
        format!(
            "#!/bin/sh\nSTATE={state:?}\n{preamble}\
             if [ \"$is_master\" -ge 2 ]; then\n  \
               echo $$ > \"$STATE/masterpid\"\n  \
               code=$(\"$SSH_ASKPASS\" \"{prompt}\")\n  \
               if [ \"$code\" = \"{expected_code}\" ]; then echo ok > \"$STATE/authed\"; fi\n  \
               exit 0\nfi\n\
             if [ \"$is_check\" = 1 ]; then\n  \
               [ -f \"$STATE/authed\" ] || exit 1\n  \
               if [ -f \"$STATE/masterpid\" ] && kill -0 \"$(cat \"$STATE/masterpid\")\" 2>/dev/null; then exit 1; fi\n  \
               exit 0\nfi\n\
             exit 1\n",
            preamble = preamble(),
        )
    }

    /// **実機の回帰（2026-09-17、sirius）**: `~/.ssh/config` に `ControlPersist` があると、ssh は認証が済んだ
    /// 時点で自分をバックグラウンドへ切り離す（master は PPID 1 になり、こちらの子は終了する）。
    /// 「子が終了した＝失敗」と決めつけていたため、**接続できているのに失敗を返していた**
    /// （人間の報告「一回接続に失敗しましたという表記が出てから接続に成功しています」。
    /// 実際には 1 回目で繋がっていて、失敗表示だけが誤りだった）。子の終了後に必ず `-O check` を見る。
    #[tokio::test]
    async fn totp_succeeds_when_ssh_backgrounds_itself_after_authenticating() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let prompt = "(rmaeda@130.158.241.2) Verification code: ";
        let ssh = fake_ssh(dir.path(), "ssh", &daemonizing_askpass_script(state.path(), prompt, "123456"));
        let result =
            start_connect(&ssh, "cluster-host", true, Duration::from_secs(5), Duration::from_secs(5)).await.unwrap();
        let ClusterConnectStart::NeedsCode { session, .. } = result else {
            panic!("expected NeedsCode");
        };
        match session.submit_code("123456", Duration::from_secs(5)).await {
            // 保持する子は無い（ssh が切り離した）が、**接続は成功している**。
            Ok(master) => assert!(master.is_none(), "the child exited, so there is nothing to hold"),
            Err(e) => panic!("expected success even though the child exited, got {e:?}"),
        }
    }

    /// 切り離されても**間違ったコードなら失敗のまま**（上の修正で失敗を握りつぶしていないこと）。
    #[tokio::test]
    async fn totp_still_fails_when_the_code_is_wrong_and_ssh_exits() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let prompt = "(rmaeda@130.158.241.2) Verification code: ";
        let ssh = fake_ssh(dir.path(), "ssh", &daemonizing_askpass_script(state.path(), prompt, "123456"));
        let result =
            start_connect(&ssh, "cluster-host", true, Duration::from_secs(5), Duration::from_secs(5)).await.unwrap();
        let ClusterConnectStart::NeedsCode { session, .. } = result else {
            panic!("expected NeedsCode");
        };
        assert!(session.submit_code("000000", Duration::from_secs(2)).await.is_err());
    }

    #[tokio::test]
    async fn totp_submit_code_connects_with_the_right_code() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let prompt = "(rmaeda@130.158.241.2) Verification code: ";
        let ssh = fake_ssh(dir.path(), "ssh", &askpass_script(state.path(), prompt, "123456"));
        let result =
            start_connect(&ssh, "cluster-host", true, Duration::from_secs(5), Duration::from_secs(5)).await.unwrap();
        let ClusterConnectStart::NeedsCode { session, .. } = result else {
            panic!("expected NeedsCode");
        };
        let master = session.submit_code("123456", Duration::from_secs(5)).await;
        match master {
            Ok(_master) => {}
            Err(e) => panic!("expected Ok(ClusterMaster), got {e:?}"),
        }
    }

    // ---- 6. 不正なコードは ssh に渡らない ----

    #[tokio::test]
    async fn totp_rejects_invalid_codes_without_delivering_them() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let prompt = "(rmaeda@130.158.241.2) Verification code: ";

        for bad in ["", "12\n34"] {
            let ssh = fake_ssh(dir.path(), "ssh", &askpass_script(state.path(), prompt, "123456"));
            let result = start_connect(&ssh, "cluster-host", true, Duration::from_secs(5), Duration::from_secs(5))
                .await
                .unwrap();
            let ClusterConnectStart::NeedsCode { session, .. } = result else {
                panic!("expected NeedsCode");
            };
            let outcome = session.submit_code(bad, Duration::from_secs(1)).await;
            assert!(matches!(outcome, Err(ClusterConnectError::InvalidCode)), "{outcome:?}");
        }
        // 何もコードが渡っていないので、askpass のスクリプトは一度も `authed` を書いていない。
        assert!(!state.path().join("authed").exists(), "ssh に何も渡らない");
    }

    // ---- 7. cancel で子プロセスと一時ディレクトリが消える ----

    #[tokio::test]
    async fn cancel_kills_the_child_and_removes_the_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let prompt = "(rmaeda@130.158.241.2) Verification code: ";
        let ssh = fake_ssh(dir.path(), "ssh", &askpass_script(state.path(), prompt, "123456"));
        let result =
            start_connect(&ssh, "cluster-host", true, Duration::from_secs(5), Duration::from_secs(5)).await.unwrap();
        let ClusterConnectStart::NeedsCode { session, .. } = result else {
            panic!("expected NeedsCode");
        };
        let pid = session.child.as_ref().and_then(|c| c.id());
        let session_dir = session.dir.clone();
        assert!(session_dir.is_dir());
        session.cancel().await;
        assert!(!session_dir.exists(), "一時ディレクトリも消える");
        if let Some(pid) = pid {
            wait_until_process_gone(pid).await;
        }
    }

    // ---- 8. totp、プロンプトが来ないまま prompt_timeout ----

    #[tokio::test]
    async fn totp_times_out_when_no_prompt_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), "ssh", &never_authenticates_script(state.path()));
        let result =
            start_connect(&ssh, "cluster-host", true, Duration::from_millis(300), Duration::from_secs(5)).await;
        assert!(matches!(result, Err(ClusterConnectError::Timeout)), "{result:?}");
        let pid_file = state.path().join("pid");
        assert!(pid_file.is_file(), "master は一度は起動した");
        let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        wait_until_process_gone(pid).await;
    }

    // ---- disconnect ----

    #[tokio::test]
    async fn disconnect_runs_o_exit_batch_mode() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(dir.path(), "ssh", &format!("#!/bin/sh\n{}if [ \"$is_exit\" = 1 ]; then exit 0; fi\nexit 1\n", preamble()));
        disconnect(&ssh, "cluster-host").await.expect("disconnect ok");
    }

    #[tokio::test]
    async fn disconnect_surfaces_stderr_without_a_code() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = fake_ssh(
            dir.path(),
            "ssh",
            "#!/bin/sh\necho 'no such control socket' >&2\nexit 1\n",
        );
        let err = disconnect(&ssh, "cluster-host").await.expect_err("disconnect fails");
        match err {
            ClusterConnectError::Failed(detail) => assert!(detail.contains("control socket"), "{detail}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
