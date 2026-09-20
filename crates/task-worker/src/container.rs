//! コンテナ実行（ADR-0043 D3。Phase 56 = A3）。
//!
//! リポジトリの `run` が `container`（または `auto` で `workspace.toml` の `[run] mode = "container"`）の
//! とき、**そのタスクのワーカー（ハーネスの CLI そのもの）をコンテナの中で起こす**。やることは 1 つだけで、
//! アダプタが組み立てた `(program, args, env, cwd)` を
//!
//! ```text
//! <runtime> run --rm -i --network host {--userns=keep-id | --user <uid>:<gid>} -w <cwd> \
//!   -v <task_dir>:<task_dir> [-v <dir リポジトリの実体>:<同じパス>] [-v <認証情報>:<同じパス>:ro] \
//!   [--env K=V …] [workspace.toml の mounts] --label celeris.task=<task_id> <image> <program> <args…>
//! ```
//!
//! に**包む**（ハーネスの stdio 契約はそのまま使えるので、アダプタごとの変更は無い）。
//!
//! ## 差し込み点（ADR-0043 D3「差し込み点は 1 か所」）
//!
//! 包むのは [`wrap`] の 1 関数だけで、呼ぶ場所は「`Command` を組み立て終えて stdio を付ける直前」である:
//!
//! | ファイル | 関数 | 何を包むか |
//! |---|---|---|
//! | `subprocess.rs` | `run_subprocess` | `fake` など `SubprocessSpec` のワーカー |
//! | `claude_code.rs` | `run_claude_code` | `claude` CLI |
//! | `codex.rs` | `run_codex` | `codex` CLI |
//! | `acp.rs` | `run_acp` | `opencode acp` |
//! | `workspace.rs` | `LocalWorkspace::exec` | `[commands] setup`（ADR-0043 D3「`setup` はその実行環境で走る」） |
//!
//! `paperqa` / `local-deep-research` は**コンテナに入れない**（[`HOST_ONLY_ADAPTERS`]）。道具立てが
//! ホストの venv（`uv` で作った `.venv`、`PAPERQA_*` / LDR の設定）に生えていて、コンテナに持ち込むと
//! 別物になるためである。[`decide`] がこの 2 つを弾く。
//!
//! ## ここに無いもの
//!
//! 判断（どのタスクをコンテナで走らせるか）は [`decide`] という**純粋関数**にあり、それを呼ぶのは
//! ディスパッチャである。LLM 呼び出しはもちろん無い（DESIGN 原則 1）。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use task_core::repos::RepoRun;
use task_core::workspace_config::{RunMode, WorkspaceConfig};

/// `--label celeris.task=<task_id>`（後片付け用。ADR-0043 D3）。
pub const TASK_LABEL: &str = "celeris.task";

/// `[containers] image_default` の既定（`deploy/containers/celeris-worker/Dockerfile`）。
pub const DEFAULT_IMAGE: &str = "celeris-worker:latest";

/// `[container] dockerfile` の既定の場所（リポジトリ相対。ADR-0042 D2）。
pub const DEFAULT_DOCKERFILE: &str = ".config/celeris/Dockerfile";

/// `[containers] build_timeout_secs` の既定（30 分）。
pub const DEFAULT_BUILD_TIMEOUT_SECS: u64 = 1800;

/// `workspace.toml` から作ったイメージのタグの接頭辞（`celeris-ws-<sha12>`）。
pub const BUILT_IMAGE_PREFIX: &str = "celeris-ws-";

/// ビルドの記録（タスクの `<task_dir>/` からの相対）。
pub const BUILD_LOG: &str = "runs/container-build.log";

/// **コンテナに入れないアダプタ**（ADR-0043 Phase 56 追記）。道具立てがホストの venv にあるため。
pub const HOST_ONLY_ADAPTERS: [&str; 2] = ["paperqa", "local-deep-research"];

/// 認証情報の置き場を指しているとみなす環境変数（値のパスを**読み取り専用**で同じ場所にマウントする）。
/// `OPENCODE_CONFIG` だけはファイルを指すので、その**親ディレクトリ**を渡す。
pub const CREDENTIAL_ENV_DIRS: [&str; 3] =
    ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR", "CODEX_HOME"];
/// 値が**ファイル**の認証情報（親ディレクトリをマウントする）。
pub const CREDENTIAL_ENV_FILES: [&str; 1] = ["OPENCODE_CONFIG"];

// ---------------------------------------------------------------------------
// runtime（podman / docker）
// ---------------------------------------------------------------------------

/// 使えるコンテナ runtime（ADR-0043 D3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Runtime {
    Podman,
    Docker,
}

impl Runtime {
    pub fn as_str(self) -> &'static str {
        match self {
            Runtime::Podman => "podman",
            Runtime::Docker => "docker",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "podman" => Some(Runtime::Podman),
            "docker" => Some(Runtime::Docker),
            _ => None,
        }
    }
}

/// `[containers] runtime` の設定値。`auto` は podman を先に試し、駄目なら docker（ADR-0043 D3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimePreference {
    #[default]
    Auto,
    Podman,
    Docker,
}

impl RuntimePreference {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimePreference::Auto => "auto",
            RuntimePreference::Podman => "podman",
            RuntimePreference::Docker => "docker",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(RuntimePreference::Auto),
            "podman" => Some(RuntimePreference::Podman),
            "docker" => Some(RuntimePreference::Docker),
            _ => None,
        }
    }

    /// 試す順番（ADR-0043 D3: podman を優先）。
    pub fn candidates(self) -> Vec<Runtime> {
        match self {
            RuntimePreference::Auto => vec![Runtime::Podman, Runtime::Docker],
            RuntimePreference::Podman => vec![Runtime::Podman],
            RuntimePreference::Docker => vec![Runtime::Docker],
        }
    }
}

/// 起動時の検出結果（**観測値**。`GET /daemon` とログに出す。DB には書かない）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeProbe {
    /// 使える runtime。どれも駄目なら `None`（コンテナが要るタスクは `blocked`）。
    pub runtime: Option<Runtime>,
    /// 設定（`auto` / `podman` / `docker`）。
    pub preference: String,
    /// 試した runtime ごとの `<runtime> info` の結果（`("podman", "ok" | 理由)`。順番は試した順）。
    pub tried: Vec<(String, String)>,
}

impl RuntimeProbe {
    pub fn is_available(&self) -> bool {
        self.runtime.is_some()
    }

    /// 選んだ runtime の実行ファイル名。
    pub fn program(&self) -> Option<&'static str> {
        self.runtime.map(Runtime::as_str)
    }

    /// 人に見せる 1 行（ログと `blocked` の質問文）。
    pub fn summary(&self) -> String {
        match self.runtime {
            Some(rt) => format!("{} が使える", rt.as_str()),
            None if self.tried.is_empty() => "コンテナ runtime を試していない".to_string(),
            None => self
                .tried
                .iter()
                .map(|(name, detail)| format!("{name}: {detail}"))
                .collect::<Vec<_>>()
                .join(" / "),
        }
    }
}

/// `<program> info` を起こして使えるかどうかを見る（能力の確認。ADR-0043 D3）。
/// 成功なら `Ok(())`、失敗なら理由の 1 行。**ネットワークには出ない**（ローカルの runtime に聞くだけ）。
pub fn probe_program(program: &str, timeout: Duration) -> Result<(), String> {
    let mut child = match std::process::Command::new(program)
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(format!("{program} が見つからない")),
        Err(e) => return Err(format!("{program} を起こせない: {e}")),
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("`{program} info` が {} 秒で終わらなかった", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("`{program} info` を待てなかった: {e}")),
        }
    }
    let out = match child.wait_with_output() {
        Ok(out) => out,
        Err(e) => return Err(format!("`{program} info` を待てなかった: {e}")),
    };
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let reason = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("`info` が失敗した")
        .to_string();
    Err(truncate(&reason, 200))
}

/// 検出（判断だけ。`probe` に `<runtime> info` を渡す）。**純粋**なので試験できる。
pub fn detect_with<F>(preference: RuntimePreference, probe: F) -> RuntimeProbe
where
    F: Fn(Runtime) -> Result<(), String>,
{
    let mut out = RuntimeProbe {
        runtime: None,
        preference: preference.as_str().to_string(),
        tried: Vec::new(),
    };
    for candidate in preference.candidates() {
        match probe(candidate) {
            Ok(()) => {
                out.tried.push((candidate.as_str().to_string(), "ok".to_string()));
                out.runtime = Some(candidate);
                break;
            }
            Err(reason) => out.tried.push((candidate.as_str().to_string(), reason)),
        }
    }
    out
}

/// 起動時の検出（`podman info` → `docker info`）。
pub fn detect(preference: RuntimePreference, timeout: Duration) -> RuntimeProbe {
    detect_with(preference, |rt| probe_program(rt.as_str(), timeout))
}

// ---------------------------------------------------------------------------
// 判断（どのタスクをコンテナで走らせるか）
// ---------------------------------------------------------------------------

/// 1 リポジトリ分の判断材料（ADR-0043 D3。`project_repos.run` と `workspace.toml`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRunInput {
    /// 案件の中での名前。
    pub name: String,
    /// `project_repos.run`（`auto` / `host` / `container`）。
    pub run: RepoRun,
    /// 案件の主なリポジトリか（イメージを選ぶ順番は primary が先）。
    pub is_primary: bool,
    /// そのリポジトリの `.config/celeris/workspace.toml`（読めなければ既定）。
    pub config: WorkspaceConfig,
    /// `workspace.toml` を読んだディレクトリ（`dockerfile` と `.config/celeris/` の基準）。
    pub config_dir: PathBuf,
}

/// どのイメージで走るか（ADR-0043 D3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// `[container] image` — **そのまま使う**（ビルドしない）。
    Named(String),
    /// `[container] dockerfile` — 内容 + `.config/celeris/` の sha でタグを付けてビルドする。
    Dockerfile {
        /// Dockerfile と `.config/celeris/` の基準になるディレクトリ。
        repo_dir: PathBuf,
        /// リポジトリ相対の Dockerfile のパス。
        dockerfile: String,
    },
    /// どちらも無い → `[containers] image_default`（`celeris-worker:latest`）。
    Default,
}

/// コンテナで走らせるときの中身（ADR-0043 D3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerChoice {
    /// コンテナを要求した最初のリポジトリの名前（primary → `repos[0]` の順）。
    pub repo: String,
    pub image: ImageSource,
    /// `[container] mounts`（`src:dst[:opts]` をそのまま渡す）。
    pub mounts: Vec<String>,
    /// `[container] env`。
    pub env: Vec<(String, String)>,
}

/// そのリポジトリがコンテナを要求しているか（ADR-0043 D3: `container`、または `auto` + `[run] mode`）。
pub fn wants_container(repo: &RepoRunInput) -> bool {
    match repo.run {
        RepoRun::Container => true,
        RepoRun::Host => false,
        RepoRun::Auto => repo.config.run.mode == RunMode::Container,
    }
}

/// **タスクをコンテナで走らせるか**（純粋関数。ADR-0043 D3）。
///
/// - 1 つでもコンテナを要求したリポジトリがあれば、そのタスクの run はコンテナ（環境は混ぜない）。
/// - イメージ・`mounts`・`env` は **primary → `repos[0]` の順で最初に要求したリポジトリ**のものを使う。
/// - `paperqa` / `local-deep-research` は**常にホスト**（[`HOST_ONLY_ADAPTERS`]）。
pub fn decide(repos: &[RepoRunInput], adapter_id: &str) -> Option<ContainerChoice> {
    if HOST_ONLY_ADAPTERS.contains(&adapter_id) {
        return None;
    }
    let chosen = repos
        .iter()
        .find(|r| r.is_primary && wants_container(r))
        .or_else(|| repos.iter().find(|r| wants_container(r)))?;
    let container = &chosen.config.container;
    let image = match (&container.image, &container.dockerfile) {
        (Some(image), _) if !image.trim().is_empty() => ImageSource::Named(image.trim().to_string()),
        (_, Some(dockerfile)) if !dockerfile.trim().is_empty() => ImageSource::Dockerfile {
            repo_dir: chosen.config_dir.clone(),
            dockerfile: dockerfile.trim().to_string(),
        },
        _ => ImageSource::Default,
    };
    Some(ContainerChoice {
        repo: chosen.name.clone(),
        image,
        mounts: container.mounts.clone(),
        env: container.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    })
}

// ---------------------------------------------------------------------------
// 計画とコマンドの組み立て
// ---------------------------------------------------------------------------

/// 1 タスク分のコンテナの形（決定的。ADR-0043 D3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPlan {
    pub runtime: Runtime,
    /// 実行ファイル（既定は `runtime.as_str()`。試験では偽物の絶対パスを差す）。
    pub program: String,
    pub image: String,
    /// `<workspace_root>/<task_id>`。**同じパス**で読み書きできるようにマウントする。
    pub task_dir: PathBuf,
    /// `dir` リポジトリの実体（シンボリックリンクの先）。同じパスでマウントする。
    pub dir_repos: Vec<PathBuf>,
    /// 追加で読み取り専用にする認証情報（アカウントプールが選んだディレクトリなど）。
    /// これとは別に、環境変数（[`CREDENTIAL_ENV_DIRS`] / [`CREDENTIAL_ENV_FILES`]）からも拾う。
    pub creds: Vec<PathBuf>,
    /// `workspace.toml` の `[container] mounts`（`src:dst[:opts]`）。
    pub extra_mounts: Vec<String>,
    /// ADR-0047 D3（Phase 61）: 知識ベースの根。**同じパスに読み取り専用**でマウントし、`_inbox` だけ
    /// 書き込み可にする（`celerisctl knowledge` がコンテナの中でもそのまま動く）。`None` ならマウントしない。
    pub knowledge_root: Option<PathBuf>,
    /// `workspace.toml` の `[container] env`（アダプタの環境より**後**に置くので勝つ）。
    pub env: Vec<(String, String)>,
    /// `--label celeris.task=<task_id>`。
    pub task_id: String,
    /// docker のときの `--user <uid>:<gid>`（podman は `--userns=keep-id`）。
    pub uid: u32,
    pub gid: u32,
}

impl ContainerPlan {
    /// `--label celeris.task=<task_id>` の値。
    pub fn label(&self) -> String {
        format!("{TASK_LABEL}={}", self.task_id)
    }
}

/// `(program, args, env, cwd)` → `<runtime> run …` の**引数**（`plan.program` は含まない）。純粋関数。
pub fn argv(plan: &ContainerPlan, program: &str, args: &[String], env: &[(String, String)], cwd: &Path) -> Vec<String> {
    let mut out: Vec<String> = vec!["run".into(), "--rm".into(), "-i".into(), "--network".into(), "host".into()];
    match plan.runtime {
        // rootless の podman はホストと同じ uid に見せる（マウントした worktree にそのまま書ける）。
        Runtime::Podman => out.push("--userns=keep-id".into()),
        // docker はデーモンが root なので、書いたファイルがホストで root にならないよう uid を渡す。
        Runtime::Docker => {
            out.push("--user".into());
            out.push(format!("{}:{}", plan.uid, plan.gid));
        }
    }
    out.push("-w".into());
    out.push(cwd.display().to_string());

    // 読み書きのマウント: タスクのディレクトリ（worktree・artifacts・runs）、`dir` リポジトリの実体、
    // そして task_dir の外に出ている cwd（`mode = shared` のタスク）。同じパスに置く。
    let mut rw: Vec<PathBuf> = vec![plan.task_dir.clone()];
    rw.extend(plan.dir_repos.iter().cloned());
    if !is_under(cwd, &rw) {
        rw.push(cwd.to_path_buf());
    }
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for path in &rw {
        if !path.is_absolute() || !seen.insert(path.clone()) {
            continue;
        }
        out.push("-v".into());
        out.push(format!("{0}:{0}", path.display()));
    }

    // 認証情報は**読み取り専用**で、アダプタが env に書いた場所だけ（ADR-0043 D3）。
    let mut ro: Vec<PathBuf> = plan.creds.clone();
    ro.extend(credential_dirs(env));
    let mut seen_ro: BTreeSet<PathBuf> = BTreeSet::new();
    for path in &ro {
        if !path.is_absolute() || is_under(path, &rw) || !seen_ro.insert(path.clone()) {
            continue;
        }
        out.push("-v".into());
        out.push(format!("{0}:{0}:ro", path.display()));
    }

    // 環境変数: HOME の既定（タスクのディレクトリ。ホームは絶対にマウントしない）→ アダプタが渡すもの →
    // `workspace.toml` の `[container] env`。同じキーは後勝ち。
    out.push("--env".into());
    out.push(format!("HOME={}", plan.task_dir.display()));
    for (k, v) in env.iter().chain(plan.env.iter()) {
        out.push("--env".into());
        out.push(format!("{k}={v}"));
    }

    // ADR-0047 D3（Phase 61）: 知識ベース。正本は読み取り専用、候補の置き場（`_inbox`）だけ書き込み可。
    if let Some(kb) = &plan.knowledge_root
        && kb.is_absolute()
        && !is_under(kb, &rw)
    {
        out.push("-v".into());
        out.push(format!("{0}:{0}:ro", kb.display()));
        let inbox = kb.join(task_core::knowledge::INBOX_DIR);
        out.push("-v".into());
        out.push(format!("{0}:{0}", inbox.display()));
    }

    // `workspace.toml` の追加マウント（`/dev/infiniband:/dev/infiniband` など）。
    for mount in &plan.extra_mounts {
        out.push("-v".into());
        out.push(mount.clone());
    }

    out.push("--label".into());
    out.push(plan.label());
    out.push(plan.image.clone());
    out.push(program.to_string());
    out.extend(args.iter().cloned());
    out
}

/// **差し込み点**（ADR-0043 D3）。組み立て終えた `Command` を読み、コンテナの中で走る `Command` に作り直す。
/// `plan` が `None` なら**そのまま返す**（ホスト実行は従来どおり 1 バイトも変わらない）。
///
/// stdio・`kill_on_drop`・`process_group` は呼び出し側がこの後で付けるので、ここでは触らない。
pub fn wrap(command: tokio::process::Command, plan: Option<&ContainerPlan>) -> tokio::process::Command {
    let Some(plan) = plan else {
        return command;
    };
    let std_command = command.as_std();
    let program = std_command.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = std_command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let env: Vec<(String, String)> = std_command
        .get_envs()
        .filter_map(|(k, v)| v.map(|v| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())))
        .collect();
    let cwd = std_command
        .get_current_dir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| plan.task_dir.clone());

    let mut wrapped = tokio::process::Command::new(&plan.program);
    wrapped.args(argv(plan, &program, &args, &env, &cwd));
    // runtime のクライアント自身はホストの環境で動かす（`DOCKER_HOST` / `XDG_RUNTIME_DIR` が要る）。
    // ワーカーに渡す環境は上の `--env` で入る。
    wrapped.current_dir(&plan.task_dir);
    wrapped
}

/// `env` のうち認証情報を指しているものの**ディレクトリ**（ADR-0043 D3）。
fn credential_dirs(env: &[(String, String)]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (k, v) in env {
        if v.is_empty() {
            continue;
        }
        if CREDENTIAL_ENV_DIRS.contains(&k.as_str()) {
            out.push(PathBuf::from(v));
        } else if CREDENTIAL_ENV_FILES.contains(&k.as_str())
            && let Some(parent) = Path::new(v).parent()
            && !parent.as_os_str().is_empty()
        {
            out.push(parent.to_path_buf());
        }
    }
    out
}

/// `path` が `roots` のどれかの下（か同じ）か。
fn is_under(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path == root || path.starts_with(root))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

// ---------------------------------------------------------------------------
// イメージ（タグ・キャッシュ・ビルド）
// ---------------------------------------------------------------------------

/// Dockerfile の内容 + `.config/celeris/` の中身から決まるタグ（ADR-0043 D3。同じ内容なら再ビルドしない）。
pub fn image_tag(dockerfile: &str, context_digest: &str) -> String {
    let mut h = Sha256::new();
    h.update(dockerfile.as_bytes());
    h.update([0u8]);
    h.update(context_digest.as_bytes());
    let digest = h.finalize();
    let hex = digest.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!("{BUILT_IMAGE_PREFIX}{}", &hex[..12])
}

/// `.config/celeris/` の中身を決定的に畳んだ sha256（相対パス昇順。無ければ空ディレクトリの sha）。
pub fn context_digest(dir: &Path) -> String {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(dir, dir, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h = Sha256::new();
    for (rel, path) in &files {
        h.update(rel.as_bytes());
        h.update([0u8]);
        match std::fs::read(path) {
            Ok(bytes) => {
                h.update(bytes.len().to_le_bytes());
                h.update(&bytes);
            }
            Err(_) => h.update(b"<unreadable>"),
        }
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect_files(root, &path, out);
        } else if meta.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push((rel.to_string_lossy().into_owned(), path));
        }
    }
}

/// 使うイメージの名前を決める（ビルドが要るものは `Build`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedImage {
    /// そのまま使う（`[container] image` か `image_default`）。
    Ready(String),
    /// ビルドが要る（`[container] dockerfile`）。
    Build(BuildRequest),
}

impl ResolvedImage {
    pub fn tag(&self) -> &str {
        match self {
            ResolvedImage::Ready(tag) => tag,
            ResolvedImage::Build(req) => &req.tag,
        }
    }
}

/// `[container] dockerfile` のビルド 1 回分（ADR-0043 D3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRequest {
    /// `celeris-ws-<sha12>`。
    pub tag: String,
    /// 元の Dockerfile（絶対パス）。
    pub dockerfile: PathBuf,
    /// 文脈にする `.config/celeris/`（絶対パス。無ければ Dockerfile の親）。
    pub context_src: PathBuf,
    /// ビルドの作業場所 `<build_dir>/<tag>/`。
    pub build_dir: PathBuf,
}

/// `ImageSource` を実際のタグに落とす（I/O は `context_digest` の読み取りだけ）。
pub fn resolve_image(source: &ImageSource, image_default: &str, build_root: &Path) -> Result<ResolvedImage, String> {
    match source {
        ImageSource::Named(image) => Ok(ResolvedImage::Ready(image.clone())),
        ImageSource::Default => Ok(ResolvedImage::Ready(image_default.to_string())),
        ImageSource::Dockerfile { repo_dir, dockerfile } => {
            let path = repo_dir.join(dockerfile);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("Dockerfile `{}` が読めない: {e}", path.display()))?;
            let context_src = repo_dir.join(".config/celeris");
            let digest = if context_src.is_dir() {
                context_digest(&context_src)
            } else {
                String::new()
            };
            let tag = image_tag(&text, &digest);
            let build_dir = build_root.join(&tag);
            Ok(ResolvedImage::Build(BuildRequest {
                tag,
                dockerfile: path,
                context_src,
                build_dir,
            }))
        }
    }
}

/// そのイメージが手元にあるか（`<program> image inspect <tag>`）。
pub fn image_exists(program: &str, tag: &str) -> bool {
    std::process::Command::new(program)
        .args(["image", "inspect", tag])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// イメージを用意する（あれば**ビルドしない**。ADR-0043 D3「同じ内容なら再ビルドしない」）。
///
/// - `exists` は試験のために差し替えられる（キャッシュに当たったかどうかを見る）
/// - ビルドの記録は `log_path`（`<task_dir>/runs/container-build.log`）
/// - `timeout` を超えたら kill して失敗（呼び出し側はタスクを `blocked` にして人に聞く）
pub async fn ensure_image(
    program: &str,
    request: &BuildRequest,
    timeout: Duration,
    log_path: &Path,
    exists: &(dyn Fn(&str) -> bool + Sync),
) -> Result<(), String> {
    if exists(&request.tag) {
        return Ok(());
    }
    // 文脈は `<build_dir>/context/`（`.config/celeris/` の写し）。リポジトリ全体は送らない。
    let context = request.build_dir.join("context");
    if let Err(e) = tokio::fs::create_dir_all(&context).await {
        return Err(format!("ビルドの作業場所 `{}` を作れない: {e}", context.display()));
    }
    if request.context_src.is_dir()
        && let Err(e) = copy_tree(&request.context_src, &context)
    {
        return Err(format!("`{}` を写せない: {e}", request.context_src.display()));
    }
    // Dockerfile が `.config/celeris/` の外にある場合も、写しの中に 1 つ置く。
    let dockerfile_in_context = match request.dockerfile.strip_prefix(&request.context_src) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => {
            let dest = context.join("Dockerfile");
            if let Err(e) = std::fs::copy(&request.dockerfile, &dest) {
                return Err(format!("Dockerfile を写せない: {e}"));
            }
            PathBuf::from("Dockerfile")
        }
    };

    let mut command = tokio::process::Command::new(program);
    command
        .arg("build")
        // run と同じネットワーク（ADR-0043 D3）。加えて、入れ子のコンテナ（この LXC）では
        // ビルド中の `RUN` がブリッジを張れず `OCI runtime create failed: recvfrom(PF_NETLINK)` で
        // 落ちるので、ホストのネットワークで動かす（Phase 56 の実機確認）。
        .arg("--network")
        .arg("host")
        .arg("-t")
        .arg(&request.tag)
        .arg("-f")
        .arg(context.join(&dockerfile_in_context))
        .arg(".")
        .current_dir(&context)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let started = format!(
        "$ {program} build --network host -t {} -f {} .\n",
        request.tag,
        dockerfile_in_context.display()
    );
    let output = match tokio::time::timeout(timeout, command.output()).await {
        Err(_) => {
            append_log(
                log_path,
                &format!("{started}=> 失敗: {} 秒で終わらなかった\n", timeout.as_secs()),
            )
            .await;
            return Err(format!(
                "イメージ `{}` のビルドが {} 秒で終わらなかった",
                request.tag,
                timeout.as_secs()
            ));
        }
        Ok(Err(e)) => {
            append_log(log_path, &format!("{started}=> 失敗: {e}\n")).await;
            return Err(format!("`{program} build` を起こせない: {e}"));
        }
        Ok(Ok(output)) => output,
    };
    let mut log = started;
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        log.push_str("[stderr] ");
        log.push_str(&stderr);
        if !stderr.ends_with('\n') {
            log.push('\n');
        }
    }
    if output.status.success() {
        log.push_str("=> exit 0\n");
        append_log(log_path, &log).await;
        return Ok(());
    }
    log.push_str(&format!("=> 失敗: exit {:?}\n", output.status.code()));
    append_log(log_path, &log).await;
    let tail = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("理由は記録を見よ");
    Err(format!(
        "イメージ `{}` のビルドが失敗した（exit {:?}）: {}",
        request.tag,
        output.status.code(),
        truncate(tail, 200)
    ))
}

async fn append_log(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    use tokio::io::AsyncWriteExt as _;
    match tokio::fs::OpenOptions::new().create(true).append(true).open(path).await {
        Ok(mut file) => {
            let _ = file.write_all(text.as_bytes()).await;
        }
        Err(e) => tracing::warn!(path = %path.display(), error = %e, "could not write the container build log"),
    }
}

fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let meta = entry.metadata()?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 後片付け（kill の経路から呼ぶ）
// ---------------------------------------------------------------------------

/// kill の経路から呼ぶ後始末の口（ADR-0044 B2 のプロセスグループ kill helper からも呼べるようにしておく）。
///
/// `--rm` を付けているので `<runtime> run` のクライアントに SIGTERM が届けばコンテナも止まるが、
/// **クライアントの pid への `killpg` はコンテナの中には届かない**（別の PID 名前空間。ADR-0044 P55-4）。
/// そこで「ラベルで止める」道を 2 段に分けて用意する:
///
/// 1. [`ContainerStopper::terminate_blocking`] — 中の PID 1 へ SIGTERM（`<runtime> kill --signal TERM`）。
///    ホストのプロセスグループへ送る SIGTERM と**同じ瞬間**に送る。片付けの猶予を与えるのが目的。
/// 2. [`ContainerStopper::stop_blocking`] — `grace` 後の `rm -f`（ホストの SIGKILL と同じ瞬間）。
pub trait ContainerStopper: Send + Sync {
    /// `--label celeris.task=<task_id>` の付いたコンテナの中へ SIGTERM を送る（同期）。
    fn terminate_blocking(&self);
    /// `--label celeris.task=<task_id>` の付いたコンテナを消す（同期。`grace` の後に呼ぶ）。
    fn stop_blocking(&self);
}

impl ContainerStopper for ContainerPlan {
    fn terminate_blocking(&self) {
        terminate_by_label(&self.program, &self.task_id);
    }

    fn stop_blocking(&self) {
        stop_by_label(&self.program, &self.task_id);
    }
}

/// ラベルでコンテナを止めるのに要る最小限（runtime の実行ファイルとタスク id）。
///
/// `ContainerPlan` そのものを kill の経路へ持ち回すと、イメージやマウントまで抱えることになる。
/// ディスパッチャが `RunEntry` に置くのはこれだけでよい（ADR-0044 §5 / ADR-0043 P56-7）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerStop {
    pub program: String,
    pub task_id: String,
}

impl ContainerStop {
    /// 計画から作る（`program` と `task_id` だけを借りる）。
    pub fn of(plan: &ContainerPlan) -> Self {
        Self {
            program: plan.program.clone(),
            task_id: plan.task_id.clone(),
        }
    }
}

impl ContainerStopper for ContainerStop {
    fn terminate_blocking(&self) {
        terminate_by_label(&self.program, &self.task_id);
    }

    fn stop_blocking(&self) {
        stop_by_label(&self.program, &self.task_id);
    }
}

/// `--label celeris.task=<task_id>` の付いたコンテナの id（`ps -aq --filter label=…`）。
/// runtime が起動できない・落ちたなら空（kill の経路では黙って諦める）。
fn ids_by_label(program: &str, task_id: &str) -> Vec<String> {
    let filter = format!("label={TASK_LABEL}={task_id}");
    let listed = std::process::Command::new(program)
        .args(["ps", "-aq", "--filter", &filter])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(listed) = listed else {
        return Vec::new();
    };
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `--label celeris.task=<task_id>` の付いたコンテナの中の PID 1 へ SIGTERM を送る
/// （`ps -aq --filter` → `kill --signal TERM`）。ホストのプロセスグループへの `killpg` は
/// 別の PID 名前空間には届かないので、コンテナで走る run にはこちらが要る（ADR-0044 P55-4）。
/// 失敗しても warn するだけ（run の結果には影響させない）。
pub fn terminate_by_label(program: &str, task_id: &str) {
    let ids = ids_by_label(program, task_id);
    if ids.is_empty() {
        return;
    }
    let mut command = std::process::Command::new(program);
    command.arg("kill").arg("--signal").arg("TERM").args(&ids);
    if let Err(e) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        tracing::warn!(%task_id, error = %e, "could not signal the task container");
    }
}

/// `--label celeris.task=<task_id>` の付いたコンテナを強制的に消す（`ps -aq --filter` → `rm -f`）。
/// 失敗しても warn するだけ（run の結果には影響させない）。
pub fn stop_by_label(program: &str, task_id: &str) {
    let ids = ids_by_label(program, task_id);
    if ids.is_empty() {
        return;
    }
    let mut command = std::process::Command::new(program);
    command.arg("rm").arg("-f").args(&ids);
    if let Err(e) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        tracing::warn!(%task_id, error = %e, "could not remove the task container");
    }
}

/// 人に見せる質問文（ADR-0043 D3「runtime が使えません」）。`setup` の失敗と同じ経路で `blocked` になる。
pub fn unavailable_question(probe: &RuntimeProbe, repo: &str) -> String {
    format!(
        "コンテナ runtime が使えません（`{repo}` が `run = container` を要求しています）: {}。\
         `[containers] runtime` の設定か、podman / docker の用意を見てください。\
         ホストで走らせてよければ、そのリポジトリの `run` を `host` にしてください。",
        probe.summary()
    )
}

/// ビルドやイメージの失敗を人に聞く文面（同じく `blocked`）。
pub fn image_question(repo: &str, reason: &str, log_path: &Path) -> String {
    format!(
        "コンテナのイメージを用意できませんでした（`{repo}`）: {reason}。記録は `{}` にあります。\
         直し方を教えてください（Dockerfile を直す／`[container] image` を指す／`run = host` にする）。",
        log_path.display()
    )
}

/// ホストの uid / gid（docker の `--user` に渡す。podman は `--userns=keep-id` なので要らないが、
/// どちらでも同じ計画を作れるように常に取る）。
pub fn host_ids() -> (u32, u32) {
    (nix::unistd::getuid().as_raw(), nix::unistd::getgid().as_raw())
}

/// `ContainerPlan` を `Arc` で持ち回すときの別名（アダプタの設定に入る）。
pub type SharedPlan = Arc<ContainerPlan>;

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::workspace_config::ContainerSection;

    fn plan(runtime: Runtime) -> ContainerPlan {
        ContainerPlan {
            runtime,
            program: runtime.as_str().to_string(),
            image: "celeris-worker:latest".to_string(),
            task_dir: PathBuf::from("/home/u/.local/celeris/workspaces/01TASK"),
            dir_repos: vec![PathBuf::from("/data/benchfs-runs")],
            creds: vec![],
            extra_mounts: vec![],
            knowledge_root: None,
            env: vec![],
            task_id: "01TASK".to_string(),
            uid: 1001,
            gid: 1001,
        }
    }

    fn joined(args: &[String]) -> String {
        args.join(" ")
    }

    /// ADR-0043 D3: podman は `--userns=keep-id`、docker は `--user <uid>:<gid>`。
    /// タスクのディレクトリと `dir` リポジトリの実体は**同じパス**、cwd は `-w`。
    #[test]
    fn podman_and_docker_differ_only_in_how_they_keep_the_uid() {
        let cwd = PathBuf::from("/home/u/.local/celeris/workspaces/01TASK/repos/benchfs");
        let args = vec!["-c".to_string(), "echo hi".to_string()];
        let podman = argv(&plan(Runtime::Podman), "sh", &args, &[], &cwd);
        let docker = argv(&plan(Runtime::Docker), "sh", &args, &[], &cwd);

        for got in [&podman, &docker] {
            let line = joined(got);
            assert!(line.starts_with("run --rm -i --network host"), "{line}");
            assert!(line.contains("-w /home/u/.local/celeris/workspaces/01TASK/repos/benchfs"), "{line}");
            assert!(
                line.contains(
                    "-v /home/u/.local/celeris/workspaces/01TASK:/home/u/.local/celeris/workspaces/01TASK"
                ),
                "{line}"
            );
            assert!(line.contains("-v /data/benchfs-runs:/data/benchfs-runs"), "{line}");
            assert!(line.contains("--label celeris.task=01TASK"), "{line}");
            assert!(line.ends_with("celeris-worker:latest sh -c echo hi"), "{line}");
            // cwd は task_dir の下なので、重ねてマウントしない。
            assert_eq!(got.iter().filter(|a| *a == "-v").count(), 2, "{line}");
        }
        assert!(joined(&podman).contains("--userns=keep-id"), "{}", joined(&podman));
        assert!(!joined(&podman).contains("--user 1001:1001"));
        assert!(joined(&docker).contains("--user 1001:1001"), "{}", joined(&docker));
        assert!(!joined(&docker).contains("keep-id"));
    }

    /// ADR-0043 D3: 認証情報は**アダプタが env に書いた場所**だけ、読み取り専用で。
    /// `OPENCODE_CONFIG` はファイルなので親ディレクトリ。`~/.local/celeris` を丸ごと見せない。
    #[test]
    fn credentials_from_the_adapter_env_are_mounted_read_only() {
        let cwd = PathBuf::from("/home/u/.local/celeris/workspaces/01TASK/repos/benchfs");
        let env = vec![
            ("CLAUDE_CONFIG_DIR".to_string(), "/home/u/.local/celeris/accounts/claude/a1".to_string()),
            ("CODEX_HOME".to_string(), "/home/u/.local/celeris/accounts/codex/c1".to_string()),
            ("OPENCODE_CONFIG".to_string(), "/home/u/qwen/opencode.json".to_string()),
            ("ANTHROPIC_MODEL".to_string(), "sonnet".to_string()),
        ];
        let got = argv(&plan(Runtime::Podman), "claude", &[], &env, &cwd);
        let line = joined(&got);
        assert!(line.contains("-v /home/u/.local/celeris/accounts/claude/a1:/home/u/.local/celeris/accounts/claude/a1:ro"), "{line}");
        assert!(line.contains("-v /home/u/.local/celeris/accounts/codex/c1:/home/u/.local/celeris/accounts/codex/c1:ro"), "{line}");
        assert!(line.contains("-v /home/u/qwen:/home/u/qwen:ro"), "{line}");
        assert!(!line.contains("-v /home/u/.local/celeris:/"), "celeris の根を丸ごと見せない: {line}");
        assert!(!line.contains("-v /home/u:/home/u"), "ホームを丸ごと見せない: {line}");
        // env はそのまま渡る（値も含めて）。
        assert!(line.contains("--env ANTHROPIC_MODEL=sonnet"), "{line}");
        assert!(line.contains("--env CLAUDE_CONFIG_DIR=/home/u/.local/celeris/accounts/claude/a1"), "{line}");
        // HOME はタスクのディレクトリ（ホストのホームは見せない）。
        assert!(line.contains("--env HOME=/home/u/.local/celeris/workspaces/01TASK"), "{line}");
    }

    /// ADR-0047 D3（Phase 61）: 知識ベースは**同じパスに読み取り専用**、`_inbox` だけ書き込み可。
    /// 設定していなければ 1 バイトも変わらない。
    #[test]
    fn the_knowledge_base_is_mounted_read_only_with_a_writable_inbox() {
        let cwd = PathBuf::from("/home/u/.local/celeris/workspaces/01TASK/repos/benchfs");
        let without = joined(&argv(&plan(Runtime::Podman), "sh", &[], &[], &cwd));
        assert!(!without.contains("knowledge"), "{without}");

        let mut p = plan(Runtime::Podman);
        p.knowledge_root = Some(PathBuf::from("/home/u/knowledge"));
        let line = joined(&argv(&p, "sh", &[], &[], &cwd));
        assert!(line.contains("-v /home/u/knowledge:/home/u/knowledge:ro"), "{line}");
        assert!(line.contains("-v /home/u/knowledge/_inbox:/home/u/knowledge/_inbox"), "{line}");
        // `_inbox` の方は `:ro` が付かない（候補を書けないと `record` が使えない）。
        assert!(!line.contains("/home/u/knowledge/_inbox:ro"), "{line}");
        // 相対パスは無視する（ホストのどこを指すか分からないものはマウントしない）。
        let mut relative = plan(Runtime::Podman);
        relative.knowledge_root = Some(PathBuf::from("knowledge"));
        assert!(!joined(&argv(&relative, "sh", &[], &[], &cwd)).contains("knowledge"));
    }

    /// `[container] env` はアダプタの環境より**後**（同じキーなら勝つ）。`mounts` はそのまま渡る。
    #[test]
    fn workspace_toml_env_comes_last_and_extra_mounts_are_passed_through() {
        let cwd = PathBuf::from("/home/u/.local/celeris/workspaces/01TASK/repos/benchfs");
        let mut p = plan(Runtime::Podman);
        p.extra_mounts = vec!["/dev/infiniband:/dev/infiniband".to_string()];
        p.env = vec![("CARGO_TARGET_DIR".to_string(), "/w/.cargo-target".to_string())];
        let env = vec![("CARGO_TARGET_DIR".to_string(), "/host".to_string())];
        let got = argv(&p, "sh", &[], &env, &cwd);
        let line = joined(&got);
        assert!(line.contains("-v /dev/infiniband:/dev/infiniband"), "{line}");
        let host_at = line.find("--env CARGO_TARGET_DIR=/host").expect(&line);
        let toml_at = line.find("--env CARGO_TARGET_DIR=/w/.cargo-target").expect(&line);
        assert!(host_at < toml_at, "workspace.toml が後（後勝ち）: {line}");
    }

    /// `mode = shared` のタスク（cwd が task_dir の外）では cwd も同じパスでマウントする。
    #[test]
    fn a_cwd_outside_the_task_directory_is_mounted_too() {
        let cwd = PathBuf::from("/home/u/workspace/agent-platform");
        let got = argv(&plan(Runtime::Docker), "sh", &[], &[], &cwd);
        let line = joined(&got);
        assert!(line.contains("-v /home/u/workspace/agent-platform:/home/u/workspace/agent-platform"), "{line}");
    }

    /// ADR-0043 D3: 1 つでも `container` を要求したら、そのタスクはコンテナ。
    /// イメージは **primary → repos[0] の順で最初に要求したリポジトリ**のもの。
    #[test]
    fn the_decision_follows_the_repos_and_the_workspace_toml() {
        let host = RepoRunInput {
            name: "data".into(),
            run: RepoRun::Host,
            is_primary: false,
            config: WorkspaceConfig::default(),
            config_dir: PathBuf::from("/data"),
        };
        // `auto` + `workspace.toml` の `[run] mode = "container"`
        let mut auto_cfg = WorkspaceConfig::default();
        auto_cfg.run.mode = RunMode::Container;
        auto_cfg.container = ContainerSection {
            image: Some("ghcr.io/x/rust-dev:1.90".into()),
            ..ContainerSection::default()
        };
        let auto = RepoRunInput {
            name: "benchfs".into(),
            run: RepoRun::Auto,
            is_primary: false,
            config: auto_cfg,
            config_dir: PathBuf::from("/src/benchfs"),
        };
        // 明示の `run = container`（`workspace.toml` は何も言っていない）→ 既定のイメージ
        let explicit = RepoRunInput {
            name: "paper".into(),
            run: RepoRun::Container,
            is_primary: true,
            config: WorkspaceConfig::default(),
            config_dir: PathBuf::from("/src/paper"),
        };

        // ホストだけ → None
        assert_eq!(decide(std::slice::from_ref(&host), "claude-code"), None);
        // auto + toml → そのイメージ
        let got = decide(&[host.clone(), auto.clone()], "claude-code").expect("container");
        assert_eq!(got.repo, "benchfs");
        assert_eq!(got.image, ImageSource::Named("ghcr.io/x/rust-dev:1.90".into()));
        // 混在: primary が先（`repos[0]` の順より primary が勝つ）
        let got = decide(&[auto.clone(), explicit.clone()], "claude-code").expect("container");
        assert_eq!(got.repo, "paper");
        assert_eq!(got.image, ImageSource::Default);
        // `auto` で `mode = host`（既定）のリポジトリはコンテナを要求しない
        let plain_auto = RepoRunInput { run: RepoRun::Auto, ..host.clone() };
        assert_eq!(decide(&[plain_auto], "claude-code"), None);
        // paperqa / local-deep-research は常にホスト（道具立てがホストの venv にある）
        for adapter in HOST_ONLY_ADAPTERS {
            assert_eq!(decide(&[auto.clone(), explicit.clone()], adapter), None, "{adapter}");
        }
    }

    /// `dockerfile` を書いたリポジトリは `ImageSource::Dockerfile`。
    #[test]
    fn a_dockerfile_in_the_workspace_toml_is_built() {
        let mut cfg = WorkspaceConfig::default();
        cfg.run.mode = RunMode::Container;
        cfg.container.dockerfile = Some(DEFAULT_DOCKERFILE.to_string());
        let repo = RepoRunInput {
            name: "benchfs".into(),
            run: RepoRun::Auto,
            is_primary: true,
            config: cfg,
            config_dir: PathBuf::from("/src/benchfs"),
        };
        let got = decide(&[repo], "claude-code").expect("container");
        assert_eq!(
            got.image,
            ImageSource::Dockerfile {
                repo_dir: PathBuf::from("/src/benchfs"),
                dockerfile: DEFAULT_DOCKERFILE.to_string(),
            }
        );
    }

    /// タグは Dockerfile の内容と `.config/celeris/` の中身で決まる（どちらが変わっても変わる）。
    #[test]
    fn the_tag_changes_when_the_dockerfile_or_the_context_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join(".config/celeris");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::write(config.join("Dockerfile"), b"FROM debian:stable-slim\n").expect("write");

        let source = ImageSource::Dockerfile {
            repo_dir: dir.path().to_path_buf(),
            dockerfile: DEFAULT_DOCKERFILE.to_string(),
        };
        let build_root = dir.path().join("containers");
        let first = resolve_image(&source, DEFAULT_IMAGE, &build_root).expect("resolve");
        let tag_a = first.tag().to_string();
        assert!(tag_a.starts_with(BUILT_IMAGE_PREFIX), "{tag_a}");
        assert_eq!(tag_a.len(), BUILT_IMAGE_PREFIX.len() + 12, "{tag_a}");
        // 同じ内容なら同じタグ。
        assert_eq!(
            resolve_image(&source, DEFAULT_IMAGE, &build_root).expect("resolve").tag(),
            tag_a
        );
        // 文脈（`.config/celeris/` の別のファイル）が変わればタグも変わる。
        std::fs::write(config.join("workspace.toml"), b"[run]\nmode = \"container\"\n").expect("write");
        let tag_b = resolve_image(&source, DEFAULT_IMAGE, &build_root).expect("resolve").tag().to_string();
        assert_ne!(tag_a, tag_b);
        // Dockerfile が変わってもタグは変わる。
        std::fs::write(config.join("Dockerfile"), b"FROM debian:stable-slim\nRUN true\n").expect("write");
        let tag_c = resolve_image(&source, DEFAULT_IMAGE, &build_root).expect("resolve").tag().to_string();
        assert_ne!(tag_b, tag_c);

        // `image` / 既定はビルドしない。
        assert_eq!(
            resolve_image(&ImageSource::Named("x:1".into()), DEFAULT_IMAGE, &build_root).expect("resolve"),
            ResolvedImage::Ready("x:1".into())
        );
        assert_eq!(
            resolve_image(&ImageSource::Default, DEFAULT_IMAGE, &build_root).expect("resolve"),
            ResolvedImage::Ready(DEFAULT_IMAGE.into())
        );
    }

    /// キャッシュに当たったらビルドを起こさない（`image_exists` を差し替えて見る。ネットワークに出ない）。
    #[tokio::test]
    async fn a_cache_hit_skips_the_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("runs/container-build.log");
        let request = BuildRequest {
            tag: "celeris-ws-0123456789ab".into(),
            dockerfile: dir.path().join(".config/celeris/Dockerfile"),
            context_src: dir.path().join(".config/celeris"),
            build_dir: dir.path().join("containers/celeris-ws-0123456789ab"),
        };
        // 在る → 何もしない（存在しない runtime の名前を渡しても落ちない = 起こしていない証拠）。
        ensure_image("no-such-runtime", &request, Duration::from_secs(5), &log, &|_| true)
            .await
            .expect("cache hit");
        assert!(!log.exists(), "ビルドしていないので記録も無い");
        assert!(!request.build_dir.exists());
    }

    /// 無ければビルドする。偽の runtime（argv を記録して失敗する）で、引数と記録を見る。
    #[tokio::test]
    async fn a_cache_miss_builds_and_logs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join(".config/celeris");
        std::fs::create_dir_all(&config).expect("mkdir");
        std::fs::write(config.join("Dockerfile"), b"FROM scratch\n").expect("write");
        let fake = dir.path().join("fake-runtime");
        std::fs::write(&fake, "#!/bin/sh\necho \"argv: $*\"\necho 'boom' 1>&2\nexit 9\n").expect("write");
        set_executable(&fake);

        let log = dir.path().join("runs/container-build.log");
        let request = BuildRequest {
            tag: "celeris-ws-aaaaaaaaaaaa".into(),
            dockerfile: config.join("Dockerfile"),
            context_src: config.clone(),
            build_dir: dir.path().join("containers/celeris-ws-aaaaaaaaaaaa"),
        };
        // 並列のテストが fork している最中に書いたばかりのスクリプトを exec すると ETXTBSY（Text file busy）に
        // なることがある（他スレッドの子が書き込み fd を一瞬継いでいる）。負荷が高いときだけ出るので、その場合だけやり直す。
        let mut err = String::new();
        for _ in 0..20 {
            err = ensure_image(
                &fake.display().to_string(),
                &request,
                Duration::from_secs(30),
                &log,
                &|_| false,
            )
            .await
            .expect_err("build fails");
            if !err.contains("Text file busy") && !err.contains("os error 26") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(err.contains("celeris-ws-aaaaaaaaaaaa"), "{err}");
        assert!(err.contains("boom"), "{err}");
        let text = std::fs::read_to_string(&log).expect("log");
        assert!(text.contains("argv: build --network host -t celeris-ws-aaaaaaaaaaaa -f"), "{text}");
        assert!(text.contains("[stderr] boom"), "{text}");
        // 文脈は `.config/celeris/` の写しだけ（リポジトリ全体は送らない）。
        assert!(request.build_dir.join("context/Dockerfile").is_file());
    }

    /// 検出は podman → docker の順。両方駄目なら `None` と理由が残る（ADR-0043 D3）。
    #[test]
    fn detection_prefers_podman_and_records_why_each_one_failed() {
        let ok = detect_with(RuntimePreference::Auto, |_| Ok(()));
        assert_eq!(ok.runtime, Some(Runtime::Podman));
        assert_eq!(ok.tried, vec![("podman".to_string(), "ok".to_string())]);

        let fallback = detect_with(RuntimePreference::Auto, |rt| match rt {
            Runtime::Podman => Err("newuidmap: Operation not permitted".into()),
            Runtime::Docker => Ok(()),
        });
        assert_eq!(fallback.runtime, Some(Runtime::Docker));
        assert_eq!(fallback.tried.len(), 2);
        assert!(fallback.tried[0].1.contains("newuidmap"));

        let none = detect_with(RuntimePreference::Auto, |rt| Err(format!("{} が見つからない", rt.as_str())));
        assert!(!none.is_available());
        assert!(none.summary().contains("podman"), "{}", none.summary());
        assert!(none.summary().contains("docker"), "{}", none.summary());

        // 明示した runtime は 1 つだけ試す。
        let only = detect_with(RuntimePreference::Docker, |rt| {
            assert_eq!(rt, Runtime::Docker);
            Ok(())
        });
        assert_eq!(only.runtime, Some(Runtime::Docker));
    }

    /// 実物の `probe_program`（`<program> info`）: 成功・`info` が失敗・実行ファイルが無い。
    /// PATH は触らず、偽物を絶対パスで指す（他の試験と競合しない）。
    #[test]
    fn probing_a_runtime_sees_success_failure_and_absence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let good = dir.path().join("podman");
        std::fs::write(&good, "#!/bin/sh\n[ \"$1\" = info ] || exit 2\necho host: ok\n").expect("write");
        set_executable(&good);
        assert_eq!(probe_program(&good.display().to_string(), Duration::from_secs(10)), Ok(()));

        let bad = dir.path().join("docker");
        std::fs::write(
            &bad,
            "#!/bin/sh\necho 'Cannot connect to the Docker daemon' 1>&2\nexit 1\n",
        )
        .expect("write");
        set_executable(&bad);
        let err = probe_program(&bad.display().to_string(), Duration::from_secs(10)).expect_err("info fails");
        assert!(err.contains("Cannot connect"), "{err}");

        let missing = dir.path().join("nope");
        let err = probe_program(&missing.display().to_string(), Duration::from_secs(10)).expect_err("missing");
        assert!(err.contains("見つからない"), "{err}");
    }

    /// `wrap(cmd, None)` はホスト実行のまま（1 バイトも変えない）。`Some` なら runtime に置き換わる。
    #[tokio::test]
    async fn wrap_is_a_no_op_without_a_plan_and_rewrites_the_command_with_one() {
        let mut host = tokio::process::Command::new("claude");
        host.arg("-p").arg("hi").env("CODEX_HOME", "/creds/c1").current_dir("/w/01TASK/repos/x");
        let kept = wrap(host, None);
        assert_eq!(kept.as_std().get_program().to_string_lossy(), "claude");

        let mut inner = tokio::process::Command::new("claude");
        inner.arg("-p").arg("hi").env("CODEX_HOME", "/creds/c1").current_dir("/w/01TASK/repos/x");
        let mut p = plan(Runtime::Podman);
        p.task_dir = PathBuf::from("/w/01TASK");
        p.dir_repos = vec![];
        let wrapped = wrap(inner, Some(&p));
        let std_cmd = wrapped.as_std();
        assert_eq!(std_cmd.get_program().to_string_lossy(), "podman");
        let args: Vec<String> = std_cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let line = joined(&args);
        assert!(line.starts_with("run --rm -i --network host --userns=keep-id"), "{line}");
        assert!(line.contains("-w /w/01TASK/repos/x"), "{line}");
        assert!(line.contains("-v /w/01TASK:/w/01TASK"), "{line}");
        assert!(line.contains("-v /creds/c1:/creds/c1:ro"), "{line}");
        assert!(line.contains("--env CODEX_HOME=/creds/c1"), "{line}");
        assert!(line.ends_with("celeris-worker:latest claude -p hi"), "{line}");
    }

    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }
}
