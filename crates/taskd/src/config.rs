//! `taskd.toml`（DESIGN §2, ADR-0005 D7）。相対パス（`db`, `workspace_root`）は設定ファイルのある
//! ディレクトリからの相対と解釈する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use task_core::{DelegationLimits, RoleSpec, Tier, WorkerHint};
use task_dispatch::{ClusterSpec, DispatchConfig, ProviderSpec};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_db")]
    pub db: PathBuf,
    #[serde(default = "default_workspace_root")]
    pub workspace_root: PathBuf,
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_lease_grace_secs")]
    pub lease_grace_secs: u64,
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_kill_grace_secs")]
    pub kill_grace_secs: u64,
    #[serde(default = "default_review_timeout_secs")]
    pub review_timeout_secs: u64,
    #[serde(default = "default_error_cooldown_secs")]
    pub error_cooldown_secs: u64,
    /// ADR-0010 D6（P-3）: リトライのバックオフ `min(base·2^(attempts-1), max)` 秒。`base = 0` で無効。
    #[serde(default = "default_retry_backoff_base_secs")]
    pub retry_backoff_base_secs: u64,
    #[serde(default = "default_retry_backoff_max_secs")]
    pub retry_backoff_max_secs: u64,
    /// ADR-0011（P-38）: 同じ試行での連続 requeue の上限。達したら供給側失敗を通常の失敗（attempts 消費）として扱う。0 で requeue しない。
    #[serde(default = "default_max_requeues")]
    pub max_requeues: u32,
    #[serde(default)]
    pub adapters: AdaptersConfig,
    #[serde(default)]
    pub plan: PlanConfig,
    #[serde(default)]
    pub reviewer: ReviewerConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// ADR-0017 M1: `providers.d/*.toml`（1 ファイル 1 アカウント）を追加で読み込む glob。末尾は必ず `/*.toml`。
    /// 相対パスは設定ファイル基準。マッチしたファイルはファイル名昇順で `providers` に追記してから `validate()` を通す。
    #[serde(default)]
    pub providers_include: Option<String>,
    /// `providers_include` を解決したディレクトリの絶対パス（`Config::load` が計算。TOML には書かない。
    /// `POST /api/v1/providers` 等が書き込む先）。
    #[serde(skip)]
    pub providers_dir: Option<PathBuf>,
    /// ADR-0018: コマンドを実行するクラスタ。`WorkspaceSpec::Remote{cluster, path}` の `cluster` がここの `id` を指す。
    #[serde(default)]
    pub clusters: Vec<ClusterConfig>,
    /// ADR-0016 D1: 役割ごとの既定と指示文。タスクの値 > 役割の既定 > 全体の既定。
    #[serde(default)]
    pub roles: Vec<RoleConfig>,
    /// ADR-0016 D2: 実行中の委譲の上限。
    #[serde(default)]
    pub delegation: DelegationConfig,
    /// `Config::load` で読んだファイルの絶対パス（`GET /api/v1/config` の `config_path`。TOML には書かない）。
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

/// `[api]`（ADR-0013 D3 / D11）: HTTP API 層。`listen` が無ければ API を起動しない（既定）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// 例: `"127.0.0.1:7700"`。
    #[serde(default)]
    pub listen: Option<std::net::SocketAddr>,
    /// Bearer トークンを書いたファイル（前後の空白は除く）。loopback 以外で `listen` するときは必須。相対パスは設定ファイル基準。
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// 追加で許可する `Host` ヘッダの値（`localhost` / `127.0.0.1` / `[::1]` とポート付きの形は常に許可）。
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl ApiConfig {
    /// `token_file` の内容（前後の空白を除く）。読めない・空なら設定エラー。トークンの値はエラー文にもログにも出さない（`docs/gui/api.md` §1.1）。
    pub fn read_token(&self) -> Result<Option<String>, ConfigError> {
        let Some(path) = &self.token_file else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Invalid(format!("[api] token_file {} cannot be read: {e}", path.display())))?;
        let token = text.trim();
        if token.is_empty() {
            return Err(ConfigError::Invalid(format!("[api] token_file {} is empty", path.display())));
        }
        Ok(Some(token.to_string()))
    }
}

/// `[[roles]]`（ADR-0016 D1）: 役割ごとの既定。タスクに書かれた値 > ここの既定 > 全体の既定の順に効く。
/// `id` は自由記述で、ここに無い役割名をタスクに付けてもよい（既定も指示文も無いだけ）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleConfig {
    /// タスクの `role` が指す名前（例 `"lead"` / `"implementer"` / `"reviewer"`）。
    pub id: String,
    #[serde(default)]
    pub tier: Option<Tier>,
    /// 省略時は tier だけで選ぶ（fake / claude-code / codex）。
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub max_wall_secs: Option<u64>,
    /// ワーカーのプロンプトに前置きする指示文（何を任され、何を任せてよいか）。`GET /config` には**出さない**。
    #[serde(default)]
    pub instructions: Option<String>,
}

/// `[delegation]`（ADR-0016 D2 / M6）: 実行中の委譲の上限。既定は `task_core::DelegationLimits::default()` と同じ。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationConfig {
    /// 1 run あたりに受け付ける提案の件数（複数の `delegate` メッセージをまたいで数える）。
    #[serde(default = "default_max_delegate_per_run")]
    pub max_delegate_per_run: usize,
    /// 木の深さ（根 = 1）。
    #[serde(default = "default_max_tree_depth")]
    pub max_tree_depth: u32,
    /// 木全体のワーカー run 数。
    #[serde(default = "default_max_tree_runs")]
    pub max_tree_runs: u32,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            max_delegate_per_run: default_max_delegate_per_run(),
            max_tree_depth: default_max_tree_depth(),
            max_tree_runs: default_max_tree_runs(),
        }
    }
}

fn default_max_delegate_per_run() -> usize {
    task_core::DelegationLimits::default().max_delegate_per_run
}
fn default_max_tree_depth() -> u32 {
    task_core::DelegationLimits::default().max_tree_depth
}
fn default_max_tree_runs() -> u32 {
    task_core::DelegationLimits::default().max_tree_runs
}

/// `[[clusters]]`（ADR-0018）: ssh でコマンドを実行するクラスタ。接続は人が張った ControlMaster を借りる。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    /// タスクの `WorkspaceSpec::Remote{cluster}` が指す名前。
    pub id: String,
    /// `~/.ssh/config` の `Host` 名（`ControlMaster` の設定が要る）。
    pub host: String,
    /// このクラスタで同時に走らせる run の上限。
    #[serde(default = "default_cluster_concurrency")]
    pub concurrency: usize,
    /// `rsync`（既定）か `none`（共有ファイルシステム）。
    #[serde(default = "default_cluster_sync")]
    pub sync: String,
    /// push（手元 → クラスタ）で手元に無いファイルを消すか。既定 false（既存プロジェクトを壊さない）。
    #[serde(default)]
    pub delete_on_push: bool,
    /// コマンドの前に流す準備（`module load ...` など）。
    #[serde(default)]
    pub setup: Vec<String>,
    /// リモートで `export` する環境変数。
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// `rsync` から除外するパターン（`.taskd/` は常に除外）。
    #[serde(default)]
    pub rsync_excludes: Vec<String>,
}

fn default_cluster_concurrency() -> usize {
    2
}
fn default_cluster_sync() -> String {
    "rsync".to_string()
}

/// `[reviewer]`（ADR-0010 D9, P-30）: `Check::Reviewer` の判定 run に使う adapter / tier。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewerConfig {
    /// 省略時は tier だけで選ぶ（設定表の優先順）。
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default = "default_reviewer_tier")]
    pub tier: Tier,
}

impl Default for ReviewerConfig {
    fn default() -> Self {
        Self {
            adapter: None,
            tier: default_reviewer_tier(),
        }
    }
}

fn default_reviewer_tier() -> Tier {
    Tier::Standard
}
fn default_retry_backoff_base_secs() -> u64 {
    10
}
fn default_retry_backoff_max_secs() -> u64 {
    300
}
fn default_max_requeues() -> u32 {
    5
}

/// `[plan]`（DESIGN §4.2, ADR-0007 D7）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanConfig {
    /// Plan の子を親 `done` と同時に `ready` にする（true）か、人間の `taskctl approve` を待つ（false、既定）か。
    #[serde(default)]
    pub auto_accept: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptersConfig {
    #[serde(default)]
    pub fake: FakeConfig,
    #[serde(default)]
    pub claude_code: ClaudeCodeAdapterConfig,
    #[serde(default)]
    pub codex: CodexAdapterConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeConfig {
    /// 起動するコマンド（省略時は `FakeAdapter::default_command()`）。
    #[serde(default)]
    pub command: Vec<String>,
    /// 追加の環境変数。
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// `claude-code` アダプタの設定（ADR-0006 D6）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeCodeAdapterConfig {
    /// 起動するコマンド名／パス。
    #[serde(default = "default_claude_command")]
    pub command: String,
    /// 末尾に追加する引数。
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// `--permission-mode`。taskd は許可プロンプトに応答できないため既定は `bypassPermissions`。
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// `--model`（省略時は claude の既定モデル）。
    #[serde(default)]
    pub model: Option<String>,
    /// 追加の環境変数（例: `CLAUDE_CONFIG_DIR`）。
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl Default for ClaudeCodeAdapterConfig {
    fn default() -> Self {
        Self {
            command: default_claude_command(),
            extra_args: Vec::new(),
            permission_mode: default_permission_mode(),
            model: None,
            env: HashMap::new(),
        }
    }
}

fn default_claude_command() -> String {
    "claude".to_string()
}
fn default_permission_mode() -> String {
    "bypassPermissions".to_string()
}

/// `codex` アダプタの設定（ADR-0008 D4）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexAdapterConfig {
    /// 起動するコマンド名／パス。
    #[serde(default = "default_codex_command")]
    pub command: String,
    /// `exec --json` の後、プロンプトの前に追加する引数（ADR-0008 D3）。
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// `--model`（省略時は codex の既定モデル）。
    #[serde(default)]
    pub model: Option<String>,
    /// 追加の環境変数。
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl Default for CodexAdapterConfig {
    fn default() -> Self {
        Self {
            command: default_codex_command(),
            extra_args: Vec::new(),
            model: None,
            env: HashMap::new(),
        }
    }
}

fn default_codex_command() -> String {
    "codex".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: String,
    pub adapter: String,
    #[serde(default = "default_tiers")]
    pub tiers: Vec<Tier>,
    #[serde(default = "default_provider_concurrency")]
    pub concurrency: usize,
    /// 空でなければ、このプロバイダの run の `--model` に使う（空なら `[adapters.<種別>].model`。ADR-0012 D1）。
    #[serde(default)]
    pub model: String,
    /// このプロバイダ（アカウント）の run にだけ渡す環境変数。`[adapters.<種別>].env` に重ね、同名キーはこちらが優先
    /// （例: `CLAUDE_CONFIG_DIR`、`CODEX_HOME`。ADR-0012 D1）。
    #[serde(default)]
    pub env: HashMap<String, String>,
}

fn default_db() -> PathBuf {
    PathBuf::from("taskd.sqlite3")
}
fn default_workspace_root() -> PathBuf {
    PathBuf::from("workspaces")
}
fn default_tick_ms() -> u64 {
    2000
}
fn default_max_concurrency() -> usize {
    4
}
fn default_lease_grace_secs() -> u64 {
    60
}
fn default_idle_timeout_secs() -> u64 {
    300
}
fn default_kill_grace_secs() -> u64 {
    10
}
fn default_review_timeout_secs() -> u64 {
    600
}
fn default_error_cooldown_secs() -> u64 {
    300
}
fn default_tiers() -> Vec<Tier> {
    vec![Tier::Frontier, Tier::Standard, Tier::Cheap]
}
fn default_provider_concurrency() -> usize {
    1
}

/// `providers_include` の glob（`<dir>/*.toml` の形だけを受け付ける）からディレクトリを取り出し、
/// 相対なら `base` 基準で絶対化する（ADR-0017 M1）。
fn providers_include_dir(pattern: &str, base: &Path) -> Result<PathBuf, ConfigError> {
    let dir_part = pattern
        .strip_suffix("*.toml")
        .ok_or_else(|| ConfigError::Invalid(format!("providers_include must end with \"*.toml\" (got {pattern:?})")))?;
    let dir_part = dir_part.strip_suffix('/').unwrap_or(dir_part);
    let dir = PathBuf::from(dir_part);
    Ok(if dir.is_relative() { base.join(dir) } else { dir })
}

/// `dir` 配下の `*.toml` をファイル名昇順で読み、それぞれを 1 件の `ProviderConfig`（`[[providers]]` の 1 行と同じ形）として
/// 解析する（ADR-0017 M1）。`dir` が無ければ空のまま（アカウントをまだ 1 つも追加していない状態）。
pub fn load_provider_files(dir: &Path) -> Result<Vec<ProviderConfig>, ConfigError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|source| ConfigError::Read { path: dir.to_path_buf(), source })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    let mut providers = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read { path: path.clone(), source })?;
        let provider: ProviderConfig = toml::from_str(&text)?;
        providers.push(provider);
    }
    Ok(providers)
}

impl Config {
    /// ファイルから読み、相対パスを設定ファイルのディレクトリ基準で絶対化する。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut cfg: Config = toml::from_str(&text)?;
        let base = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let base = base.canonicalize().unwrap_or(base);
        cfg.source_path = Some(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        if cfg.db.is_relative() {
            cfg.db = base.join(&cfg.db);
        }
        if cfg.workspace_root.is_relative() {
            cfg.workspace_root = base.join(&cfg.workspace_root);
        }
        if let Some(token_file) = &cfg.api.token_file
            && token_file.is_relative()
        {
            cfg.api.token_file = Some(base.join(token_file));
        }
        if let Some(pattern) = &cfg.providers_include {
            let dir = providers_include_dir(pattern, &base)?;
            cfg.providers.extend(load_provider_files(&dir)?);
            cfg.providers_dir = Some(dir);
        }
        cfg.validate()?;
        // API を有効にするなら、トークンが読めることを起動時に確かめる（exit 2）。
        if cfg.api.listen.is_some() {
            cfg.api.read_token()?;
        }
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_concurrency == 0 {
            return Err(ConfigError::Invalid("max_concurrency must be >= 1".into()));
        }
        if self.tick_ms == 0 {
            return Err(ConfigError::Invalid("tick_ms must be >= 1".into()));
        }
        if self.providers.is_empty() {
            return Err(ConfigError::Invalid("at least one [[providers]] entry is required".into()));
        }
        let mut seen_ids = std::collections::HashSet::new();
        for p in &self.providers {
            // ADR-0012 D1: アダプタのインスタンスはプロバイダ ID で引くので重複は許さない。
            if !seen_ids.insert(p.id.as_str()) {
                return Err(ConfigError::Invalid(format!("duplicate provider id {:?}", p.id)));
            }
            if p.adapter != task_worker::FakeAdapter::ID
                && p.adapter != task_worker::ClaudeCodeAdapter::ID
                && p.adapter != task_worker::CodexAdapter::ID
            {
                return Err(ConfigError::Invalid(format!(
                    "provider {}: adapter {:?} is not available in this build (fake, claude-code, codex only)",
                    p.id, p.adapter
                )));
            }
            if p.concurrency == 0 {
                return Err(ConfigError::Invalid(format!("provider {}: concurrency must be >= 1", p.id)));
            }
        }
        // ADR-0010 D9: Reviewer run を満たせるプロバイダが無い設定は、Reviewer 条件のタスクが無音で待ち続ける原因になる。
        let reviewer = &self.reviewer;
        let reviewer_ok = self.providers.iter().any(|p| {
            p.tiers.contains(&reviewer.tier) && reviewer.adapter.as_deref().is_none_or(|a| p.adapter == a)
        });
        if !reviewer_ok {
            return Err(ConfigError::Invalid(format!(
                "[reviewer] no provider offers tier {:?}{} for reviewer runs",
                reviewer.tier,
                reviewer.adapter.as_deref().map(|a| format!(" with adapter {a:?}")).unwrap_or_default()
            )));
        }
        // ADR-0013 D11: loopback 以外で API をリッスンするならトークンを必須にする。
        if let Some(listen) = self.api.listen
            && !listen.ip().is_loopback()
            && self.api.token_file.is_none()
        {
            return Err(ConfigError::Invalid(format!(
                "[api] listen = {listen} is not a loopback address; token_file is required"
            )));
        }
        // ADR-0018: クラスタの id は重複させない。sync は rsync / none のみ。並列度は 1 以上。
        let mut cluster_ids = std::collections::HashSet::new();
        for c in &self.clusters {
            if c.id.trim().is_empty() {
                return Err(ConfigError::Invalid("[[clusters]] id must not be empty".to_string()));
            }
            if !cluster_ids.insert(&c.id) {
                return Err(ConfigError::Invalid(format!("duplicate cluster id: {}", c.id)));
            }
            if c.host.trim().is_empty() {
                return Err(ConfigError::Invalid(format!("[[clusters]] {}: host must not be empty", c.id)));
            }
            if !matches!(c.sync.as_str(), "rsync" | "none") {
                return Err(ConfigError::Invalid(format!(
                    "[[clusters]] {}: sync must be \"rsync\" or \"none\" (got {:?})",
                    c.id, c.sync
                )));
            }
            if c.concurrency == 0 {
                return Err(ConfigError::Invalid(format!("[[clusters]] {}: concurrency must be >= 1", c.id)));
            }
        }
        // ADR-0016 D1: 役割の id は重複させない。adapter は providers と同じ判定。上限は 1 以上。
        let mut role_ids = std::collections::HashSet::new();
        for r in &self.roles {
            if r.id.trim().is_empty() {
                return Err(ConfigError::Invalid("[[roles]] id must not be empty".to_string()));
            }
            if !role_ids.insert(&r.id) {
                return Err(ConfigError::Invalid(format!("duplicate role id: {}", r.id)));
            }
            if let Some(adapter) = &r.adapter
                && adapter != task_worker::FakeAdapter::ID
                && adapter != task_worker::ClaudeCodeAdapter::ID
                && adapter != task_worker::CodexAdapter::ID
            {
                return Err(ConfigError::Invalid(format!(
                    "[[roles]] {}: adapter {adapter:?} is not available in this build (fake, claude-code, codex only)",
                    r.id
                )));
            }
            if r.max_turns == Some(0) {
                return Err(ConfigError::Invalid(format!("[[roles]] {}: max_turns must be >= 1", r.id)));
            }
            if r.max_wall_secs == Some(0) {
                return Err(ConfigError::Invalid(format!("[[roles]] {}: max_wall_secs must be >= 1", r.id)));
            }
        }
        // ADR-0016 D2 / M6: 0 の上限は「委譲を止める」ではなく設定ミス（拒否理由が毎回出るだけ）なので拒否する。
        if self.delegation.max_delegate_per_run == 0 {
            return Err(ConfigError::Invalid("[delegation] max_delegate_per_run must be >= 1".to_string()));
        }
        if self.delegation.max_tree_depth == 0 {
            return Err(ConfigError::Invalid("[delegation] max_tree_depth must be >= 1".to_string()));
        }
        if self.delegation.max_tree_runs == 0 {
            return Err(ConfigError::Invalid("[delegation] max_tree_runs must be >= 1".to_string()));
        }
        // Phase 7 監査: cooldown 0 だと供給側失敗の requeue が毎 tick の再 dispatch になる。
        if self.error_cooldown_secs == 0 {
            return Err(ConfigError::Invalid("error_cooldown_secs must be >= 1".into()));
        }
        // ADR-0010 D7 の前提: 無出力で強制終了された run の結果（SIGKILL までの kill_grace + 次 tick での取り込み）が、
        // 延長後のリース期限より先に処理されること。
        if self.kill_grace_secs * 1000 + self.tick_ms >= self.lease_grace_secs * 1000 / 2 {
            return Err(ConfigError::Invalid(
                "kill_grace_secs + tick_ms must be shorter than lease_grace_secs / 2 (lease renewal safety, ADR-0010 D7)".into(),
            ));
        }
        if self.retry_backoff_max_secs < self.retry_backoff_base_secs {
            return Err(ConfigError::Invalid("retry_backoff_max_secs must be >= retry_backoff_base_secs".into()));
        }
        Ok(())
    }

    pub fn tick(&self) -> Duration {
        Duration::from_millis(self.tick_ms)
    }

    pub fn dispatch_config(&self) -> DispatchConfig {
        DispatchConfig {
            max_concurrency: self.max_concurrency,
            lease_grace: Duration::from_secs(self.lease_grace_secs),
            idle_timeout: Duration::from_secs(self.idle_timeout_secs),
            kill_grace: Duration::from_secs(self.kill_grace_secs),
            review_timeout: Duration::from_secs(self.review_timeout_secs),
            workspace_root: self.workspace_root.clone(),
            plan_auto_accept: self.plan.auto_accept,
            retry_backoff_base: Duration::from_secs(self.retry_backoff_base_secs),
            retry_backoff_max: Duration::from_secs(self.retry_backoff_max_secs),
            max_requeues: self.max_requeues,
            reviewer_hint: WorkerHint {
                tier: self.reviewer.tier,
                adapter: self.reviewer.adapter.clone(),
            },
            clusters: self.cluster_specs(),
            // ADR-0018 D2: 多重接続が無いクラスタは、プロバイダの cooldown と同じ長さだけ外す。
            cluster_cooldown: Duration::from_secs(self.error_cooldown_secs),
            roles: self.role_specs(),
            delegation: self.delegation_limits(),
        }
    }

    /// ADR-0016 D1: `[[roles]]` を task-core の型に写す（設定の順）。
    pub fn role_specs(&self) -> Vec<RoleSpec> {
        self.roles
            .iter()
            .map(|r| RoleSpec {
                id: r.id.clone(),
                tier: r.tier,
                adapter: r.adapter.clone(),
                max_turns: r.max_turns,
                max_wall_secs: r.max_wall_secs,
                instructions: r.instructions.clone(),
            })
            .collect()
    }

    /// ADR-0016 D2: `[delegation]` を task-core の型に写す。
    pub fn delegation_limits(&self) -> DelegationLimits {
        DelegationLimits {
            max_delegate_per_run: self.delegation.max_delegate_per_run,
            max_tree_depth: self.delegation.max_tree_depth,
            max_tree_runs: self.delegation.max_tree_runs,
        }
    }

    /// ADR-0018: `[[clusters]]` を task-dispatch の型に写す（`env` はキー順で決定的に並べる）。
    pub fn cluster_specs(&self) -> HashMap<String, ClusterSpec> {
        self.clusters
            .iter()
            .map(|c| {
                let mut env: Vec<(String, String)> = c.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                env.sort();
                (
                    c.id.clone(),
                    ClusterSpec {
                        id: c.id.clone(),
                        host: c.host.clone(),
                        concurrency: c.concurrency,
                        sync: if c.sync == "none" { task_worker::SyncMode::None } else { task_worker::SyncMode::Rsync },
                        delete_on_push: c.delete_on_push,
                        setup: c.setup.clone(),
                        env,
                        rsync_excludes: c.rsync_excludes.clone(),
                    },
                )
            })
            .collect()
    }

    pub fn provider_specs(&self) -> Vec<ProviderSpec> {
        self.providers
            .iter()
            .map(|p| ProviderSpec {
                id: p.id.clone(),
                adapter: p.adapter.clone(),
                tiers: p.tiers.clone(),
                concurrency: p.concurrency,
                model: p.model.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_example_config_and_resolves_relative_paths() {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/taskd.example.toml"));
        let cfg = Config::load(path).unwrap();
        assert!(cfg.db.is_absolute());
        assert!(cfg.workspace_root.is_absolute());
        assert_eq!(cfg.max_concurrency, 2);
        assert_eq!(cfg.providers[0].adapter, "fake");
        assert_eq!(cfg.tick(), Duration::from_millis(2000));
        assert_eq!(cfg.provider_specs()[0].concurrency, 2);
        assert!(!cfg.plan.auto_accept);
        assert!(!cfg.dispatch_config().plan_auto_accept);
    }

    #[test]
    fn plan_auto_accept_is_parsed_and_unknown_plan_keys_are_rejected() {
        let cfg: Config = toml::from_str(r#"[plan]
auto_accept = true
[[providers]]
id = "x"
adapter = "fake"
"#).unwrap();
        assert!(cfg.plan.auto_accept);
        assert!(cfg.dispatch_config().plan_auto_accept);
        assert!(toml::from_str::<Config>("[plan]\nbogus = 1\n").is_err());
    }

    #[test]
    fn rejects_unknown_adapter_and_missing_providers() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(matches!(cfg.validate(), Err(ConfigError::Invalid(_))));
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"bogus-adapter\"\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("bogus-adapter"));
        assert!(toml::from_str::<Config>("bogus = 1\n").is_err());
    }

    #[test]
    fn loads_claude_code_dogfood_example_config() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/taskd.claude-code.example.toml"
        ));
        let cfg = Config::load(path).unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.providers[0].adapter, "claude-code");
        assert_eq!(cfg.adapters.claude_code.command, "claude");
    }

    #[test]
    fn accepts_claude_code_adapter_with_default_config() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"claude-code\"\n").unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.claude_code.command, "claude");
        assert_eq!(cfg.adapters.claude_code.permission_mode, "bypassPermissions");
    }

    #[test]
    fn rejects_unknown_fields_in_claude_code_adapter_config() {
        let text = "[[providers]]\nid = \"x\"\nadapter = \"claude-code\"\n\n[adapters.claude_code]\nbogus = 1\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    #[test]
    fn loads_codex_dogfood_example_config() {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/taskd.codex.example.toml"));
        let cfg = Config::load(path).unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.providers[0].adapter, "codex");
        assert_eq!(cfg.adapters.codex.command, "codex");
    }

    #[test]
    fn accepts_codex_adapter_with_default_config() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"codex\"\n").unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.codex.command, "codex");
        assert!(cfg.adapters.codex.model.is_none());
    }

    /// ADR-0010 D6/D9: バックオフと `[reviewer]` の既定値・指定値が DispatchConfig に写る。
    #[test]
    fn backoff_and_reviewer_settings_map_to_dispatch_config() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(cfg.validate().is_ok());
        let d = cfg.dispatch_config();
        assert_eq!(d.retry_backoff_base, Duration::from_secs(10));
        assert_eq!(d.retry_backoff_max, Duration::from_secs(300));
        assert_eq!(d.max_requeues, 5);
        assert_eq!(d.reviewer_hint, WorkerHint { tier: Tier::Standard, adapter: None });

        let text = r#"retry_backoff_base_secs = 0
retry_backoff_max_secs = 0
max_requeues = 0
[reviewer]
adapter = "claude-code"
tier = "cheap"
[[providers]]
id = "f"
adapter = "fake"
[[providers]]
id = "c"
adapter = "claude-code"
tiers = ["cheap"]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate().is_ok());
        let d = cfg.dispatch_config();
        assert_eq!(d.retry_backoff_base, Duration::ZERO);
        assert_eq!(d.max_requeues, 0);
        assert_eq!(d.reviewer_hint, WorkerHint { tier: Tier::Cheap, adapter: Some("claude-code".into()) });
    }

    /// ADR-0010 D9: Reviewer run を満たせるプロバイダが無い設定はエラー。未知キーも拒否。
    #[test]
    fn rejects_reviewer_without_matching_provider_and_unknown_reviewer_keys() {
        let cfg: Config = toml::from_str("[reviewer]\nadapter = \"codex\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("[reviewer]") && err.contains("codex"), "{err}");
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\ntiers = [\"frontier\"]\n").unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("Standard"));
        assert!(toml::from_str::<Config>("[reviewer]\nbogus = 1\n").is_err());
        let cfg: Config = toml::from_str("retry_backoff_base_secs = 20\nretry_backoff_max_secs = 10\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    /// Phase 7 監査: cooldown 0（requeue のホットループ）と、リース延長の前提を破る猶予の組み合わせを拒否する。
    #[test]
    fn rejects_zero_cooldown_and_unsafe_lease_grace() {
        let providers = "[[providers]]\nid = \"x\"\nadapter = \"fake\"\n";
        let cfg: Config = toml::from_str(&format!("error_cooldown_secs = 0\n{providers}")).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("error_cooldown_secs"));
        let cfg: Config = toml::from_str(&format!("lease_grace_secs = 10\nkill_grace_secs = 10\n{providers}")).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("lease_grace_secs"));
        let cfg: Config = toml::from_str(&format!("lease_grace_secs = 60\nkill_grace_secs = 1\ntick_ms = 50\n{providers}")).unwrap();
        assert!(cfg.validate().is_ok());
    }

    /// ADR-0012 D1: 同じアダプタ種別のプロバイダを複数並べ、それぞれに env を持たせられる。ID の重複は拒否。
    #[test]
    fn multi_account_providers_parse_and_duplicate_ids_are_rejected() {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/taskd.multi-account.example.toml"));
        let cfg = Config::load(path).unwrap();
        let claude: Vec<&ProviderConfig> = cfg.providers.iter().filter(|p| p.adapter == "claude-code").collect();
        assert!(claude.len() >= 2);
        assert_ne!(claude[0].env.get("CLAUDE_CONFIG_DIR"), claude[1].env.get("CLAUDE_CONFIG_DIR"));

        let dup = "[[providers]]\nid = \"a\"\nadapter = \"fake\"\n[[providers]]\nid = \"a\"\nadapter = \"fake\"\n";
        let cfg: Config = toml::from_str(dup).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("duplicate provider id"));
    }

    /// ADR-0013 D3 / D11: `[api]` は既定で無効。loopback 以外はトークンファイル必須。相対パスは設定ファイル基準。
    #[test]
    fn api_section_defaults_to_disabled_and_requires_token_off_loopback() {
        let providers = "[[providers]]\nid = \"x\"\nadapter = \"fake\"\n";
        let cfg: Config = toml::from_str(providers).unwrap();
        assert!(cfg.validate().is_ok());
        assert!(cfg.api.listen.is_none());

        let cfg: Config = toml::from_str(&format!("[api]\nlisten = \"127.0.0.1:7700\"\n{providers}")).unwrap();
        assert!(cfg.validate().is_ok());
        let cfg: Config = toml::from_str(&format!("[api]\nlisten = \"[::1]:7700\"\n{providers}")).unwrap();
        assert!(cfg.validate().is_ok());

        let cfg: Config = toml::from_str(&format!("[api]\nlisten = \"0.0.0.0:7700\"\n{providers}")).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("token_file is required"));
        let cfg: Config =
            toml::from_str(&format!("[api]\nlisten = \"0.0.0.0:7700\"\ntoken_file = \"api.token\"\n{providers}")).unwrap();
        assert!(cfg.validate().is_ok());
        assert!(toml::from_str::<Config>("[api]\nbogus = 1\n").is_err());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(&path, format!("[api]\nlisten = \"127.0.0.1:7700\"\ntoken_file = \"secrets/api.token\"\n{providers}")).unwrap();
        // token_file が無い・空なら起動時の設定エラー（値は出さない）。
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("token_file") && err.contains("cannot be read"), "{err}");
        std::fs::create_dir_all(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets/api.token"), " \n").unwrap();
        assert!(Config::load(&path).unwrap_err().to_string().contains("is empty"));
        std::fs::write(dir.path().join("secrets/api.token"), "  tok-123\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.api.read_token().unwrap().as_deref(), Some("tok-123"));
        assert_eq!(
            cfg.api.token_file.unwrap(),
            dir.path().canonicalize().unwrap().join("secrets/api.token")
        );
    }

    /// ADR-0016 D1 / D2: `[[roles]]` と `[delegation]` を読み、task-core の型と DispatchConfig に写す。
    #[test]
    fn roles_and_delegation_are_parsed_and_mapped() {
        let providers = "[[providers]]\nid = \"x\"\nadapter = \"fake\"\n";
        // 既定（節を書かなければ空の役割表と DelegationLimits の既定）。
        let cfg: Config = toml::from_str(providers).unwrap();
        assert!(cfg.validate().is_ok());
        assert!(cfg.roles.is_empty());
        assert_eq!(cfg.delegation_limits(), task_core::DelegationLimits::default());
        assert_eq!(cfg.delegation_limits(), DelegationLimits { max_delegate_per_run: 8, max_tree_depth: 5, max_tree_runs: 100 });

        let text = format!(
            r#"[[roles]]
id = "lead"
tier = "frontier"
max_turns = 40
max_wall_secs = 1800
instructions = "You lead the work. Delegate implementation."

[[roles]]
id = "implementer"
adapter = "fake"

[delegation]
max_delegate_per_run = 3
max_tree_depth = 2

{providers}"#
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        assert!(cfg.validate().is_ok());
        let specs = cfg.role_specs();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].id, "lead");
        assert_eq!(specs[0].tier, Some(Tier::Frontier));
        assert_eq!((specs[0].max_turns, specs[0].max_wall_secs), (Some(40), Some(1800)));
        assert!(specs[0].instructions.as_deref().unwrap().starts_with("You lead"));
        assert_eq!(specs[0].adapter, None);
        assert_eq!(specs[1].adapter.as_deref(), Some("fake"));
        assert_eq!(specs[1].tier, None);
        // 書いていない値は既定のまま。
        let limits = cfg.delegation_limits();
        assert_eq!(limits, DelegationLimits { max_delegate_per_run: 3, max_tree_depth: 2, max_tree_runs: 100 });
        let d = cfg.dispatch_config();
        assert_eq!(d.roles, specs);
        assert_eq!(d.delegation, limits);

        assert!(toml::from_str::<Config>("[[roles]]\nid = \"a\"\nbogus = 1\n").is_err());
        assert!(toml::from_str::<Config>("[delegation]\nbogus = 1\n").is_err());
    }

    /// ADR-0016: 役割 id の重複、未知の adapter、0 の上限は設定エラー。
    #[test]
    fn rejects_duplicate_roles_unknown_role_adapter_and_zero_limits() {
        let providers = "[[providers]]\nid = \"x\"\nadapter = \"fake\"\n";
        let dup = format!("[[roles]]\nid = \"lead\"\n[[roles]]\nid = \"lead\"\n{providers}");
        let cfg: Config = toml::from_str(&dup).unwrap();
        assert_eq!(cfg.validate().unwrap_err().to_string(), "invalid config: duplicate role id: lead");

        let bogus = format!("[[roles]]\nid = \"lead\"\nadapter = \"bogus\"\n{providers}");
        let cfg: Config = toml::from_str(&bogus).unwrap();
        assert_eq!(
            cfg.validate().unwrap_err().to_string(),
            "invalid config: [[roles]] lead: adapter \"bogus\" is not available in this build (fake, claude-code, codex only)"
        );

        let empty = format!("[[roles]]\nid = \"  \"\n{providers}");
        let cfg: Config = toml::from_str(&empty).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("id must not be empty"));

        let zero_turns = format!("[[roles]]\nid = \"lead\"\nmax_turns = 0\n{providers}");
        let cfg: Config = toml::from_str(&zero_turns).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("max_turns must be >= 1"));
        let zero_wall = format!("[[roles]]\nid = \"lead\"\nmax_wall_secs = 0\n{providers}");
        let cfg: Config = toml::from_str(&zero_wall).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("max_wall_secs must be >= 1"));

        for key in ["max_delegate_per_run", "max_tree_depth", "max_tree_runs"] {
            let text = format!("[delegation]\n{key} = 0\n{providers}");
            let cfg: Config = toml::from_str(&text).unwrap();
            let err = cfg.validate().unwrap_err().to_string();
            assert_eq!(err, format!("invalid config: [delegation] {key} must be >= 1"));
        }
    }

    #[test]
    fn rejects_unknown_fields_in_codex_adapter_config() {
        let text = "[[providers]]\nid = \"x\"\nadapter = \"codex\"\n\n[adapters.codex]\nbogus = 1\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    /// ADR-0017 M1: `providers_include` が `providers.d/*.toml` をファイル名昇順で読み、
    /// `[[providers]]` と合わせて重複 id を検出する。
    #[test]
    fn providers_include_merges_files_in_filename_order_and_still_rejects_duplicate_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "providers_include = \"providers.d/*.toml\"\n[[providers]]\nid = \"inline\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("providers.d")).unwrap();
        std::fs::write(
            dir.path().join("providers.d/b-acct.toml"),
            "id = \"b-acct\"\nadapter = \"claude-code\"\nconcurrency = 2\n[env]\nCLAUDE_CONFIG_DIR = \"/x/b\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("providers.d/a-acct.toml"),
            "id = \"a-acct\"\nadapter = \"fake\"\n",
        )
        .unwrap();

        let cfg = Config::load(&path).unwrap();
        let ids: Vec<&str> = cfg.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["inline", "a-acct", "b-acct"], "providers.d files load in filename order after inline ones");
        let b = cfg.providers.iter().find(|p| p.id == "b-acct").unwrap();
        assert_eq!(b.concurrency, 2);
        assert_eq!(b.env.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some("/x/b"));
        assert_eq!(cfg.providers_dir.as_deref(), Some(dir.path().join("providers.d").canonicalize().unwrap().as_path()));

        // 重複 id（inline と providers.d の両方に "inline"）は既存の検証がそのまま拒否する。
        std::fs::write(
            dir.path().join("providers.d/dup.toml"),
            "id = \"inline\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("duplicate provider id"), "{err}");
    }

    /// `providers_include` は末尾が `*.toml` である glob だけを受け付ける。
    #[test]
    fn providers_include_rejects_patterns_not_ending_in_glob_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "providers_include = \"providers.d/*.yaml\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("must end with"), "{err}");
    }

    /// `providers.d/` がまだ無い（1 つもアカウントを追加していない）ときは空のまま、inline だけで起動できる。
    #[test]
    fn providers_include_with_missing_directory_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "providers_include = \"providers.d/*.toml\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.providers.len(), 1);
    }
}
