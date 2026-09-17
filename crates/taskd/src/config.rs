//! `taskd.toml`（DESIGN §2, ADR-0005 D7）。相対パス（`db`, `workspace_root`）は設定ファイルのある
//! ディレクトリからの相対と解釈する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use task_core::{AccountAdapter, DelegationLimits, OrgKind, OrgNode, RoleSpec, Tier, WorkerHint, valid_org_id};
use task_dispatch::{AccountsRuntimeConfig, ClusterSpec, DispatchConfig, ProviderSpec};

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
    /// ADR-0027 D1: 分野ごとの説明と既定の役割。タスクの値 > 役割の既定 > 分野の既定（`default_role` の役割）> 親の値。
    #[serde(default)]
    pub genres: Vec<GenreConfig>,
    /// ADR-0033 D1: 組織図の**種**を書いたファイル（`[[org]]` の並び）。相対パスは設定ファイル基準。
    /// 省略したら種を蒔かない。蒔くのは **DB の `org_nodes` が空のときだけ**で、以後は DB が正
    /// （編集は GUI → API → DB。設定は再読込しない。ADR-0024 の accounts と同じ扱い）。
    #[serde(default)]
    pub org_include: Option<String>,
    /// `org_include` を読んだ結果（`Config::load` が埋める。TOML の `[taskd]` には書かない）。
    #[serde(skip)]
    pub org: Vec<OrgSeedConfig>,
    /// ADR-0016 D2: 実行中の委譲の上限。
    #[serde(default)]
    pub delegation: DelegationConfig,
    /// ADR-0033 D3: 報告の圧縮の閾値（`compress_after` / `compress_after_secs`）。
    #[serde(default)]
    pub reports: crate::reports::ReportsConfig,
    /// ADR-0024 D1: Claude アカウントのプール。無ければ `account_pool = true` のプロバイダは設定エラー。
    #[serde(default)]
    pub accounts: Option<AccountsConfig>,
    /// ADR-0030 D1: GUI から預かる API キー等の置き場所。無ければこの機能は無効（管理 API は 409）。
    #[serde(default)]
    pub secrets: Option<SecretsConfig>,
    /// `Config::load` で読んだファイルの絶対パス（`GET /api/v1/config` の `config_path`。TOML には書かない）。
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

/// `[secrets]`（ADR-0030 D1）: 1 秘密 = 1 ファイル（ファイル名 = id、中身 = 値 1 行）。`dir` を 0700 で作る。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    /// 相対なら設定ファイル基準。`Config::load` が絶対化する。
    pub dir: PathBuf,
}

/// `[accounts]`（ADR-0024 D1、ADR-0025 D1）: `claude_dir` / `codex_dir` の下の 1 ディレクトリが 1 アカウント。
/// どちらか一方だけでもよい（少なくとも一方は必要。`validate` でチェックする）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountsConfig {
    /// 相対なら設定ファイル基準。`Config::load` が絶対化する。`<claude_dir>/<id>/` = `CLAUDE_SECURESTORAGE_CONFIG_DIR`。
    #[serde(default)]
    pub claude_dir: Option<PathBuf>,
    /// ADR-0025 D1: 相対なら設定ファイル基準。`<codex_dir>/<id>/` = `CODEX_HOME`。
    #[serde(default)]
    pub codex_dir: Option<PathBuf>,
    /// 1 アカウントで同時に走らせる run の上限。
    #[serde(default = "default_max_runs_per_account")]
    pub max_runs_per_account: usize,
    /// D6 の確認に使うモデル（枠はアカウント単位なので最も安いモデルでよい。claude-code の確認にだけ使う）。
    #[serde(default = "default_check_model")]
    pub check_model: String,
}

impl AccountsConfig {
    /// アダプタ → 根ディレクトリ（設定されているものだけ）。
    pub fn roots(&self) -> HashMap<AccountAdapter, PathBuf> {
        let mut roots = HashMap::new();
        if let Some(dir) = &self.claude_dir {
            roots.insert(AccountAdapter::ClaudeCode, dir.clone());
        }
        if let Some(dir) = &self.codex_dir {
            roots.insert(AccountAdapter::Codex, dir.clone());
        }
        roots
    }

    pub fn root_for(&self, adapter: AccountAdapter) -> Option<&PathBuf> {
        match adapter {
            AccountAdapter::ClaudeCode => self.claude_dir.as_ref(),
            AccountAdapter::Codex => self.codex_dir.as_ref(),
        }
    }
}

fn default_max_runs_per_account() -> usize {
    2
}
fn default_check_model() -> String {
    "haiku".to_string()
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

/// `[[genres]]`（ADR-0027 D1）: 分野の説明・既定の役割・分野に属する役割の一覧。分野そのものにはアダプタを
/// 持たせない（D2: `default_role` が指す役割が持つ）。`id` は重複させない。`default_role` と `roles` の各要素は
/// `[[roles]]` に存在すること、`default_role`（あれば）は `roles` に含まれることを `Config::validate` が確認する。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenreConfig {
    /// タスクの `genre` が指す名前（例 `"coding"` / `"literature"`）。
    pub id: String,
    /// プロンプトに入れる分野の説明（ADR-0027 D1）。
    pub description: String,
    /// ADR-0028 D1: この分野で「できること」の自由記述（固定 enum にしない）。省略時は空。
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// ADR-0028 D1: この分野に投げるときに用意すべきものの目安（自由記述。taskd は中身を検査しない）。
    #[serde(default)]
    pub input_artifacts: Vec<String>,
    /// ADR-0028 D1: この分野から戻ってくるものの目安（自由記述）。
    #[serde(default)]
    pub output_artifacts: Vec<String>,
    /// タスクに `role` が無いときに、この分野の既定として使う役割 id。`roles` に含まれること。
    #[serde(default)]
    pub default_role: Option<String>,
    /// この分野に属する役割 id の一覧。`genre` と `role` を両方指定したタスクは、`role` がここに無ければ設定エラー。
    #[serde(default)]
    pub roles: Vec<String>,
}

/// `org_include` の指すファイルの中身（ADR-0033 D1）。`[[org]]` の 1 行 = 組織の 1 ノード。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgSeedFile {
    #[serde(default)]
    pub org: Vec<OrgSeedConfig>,
}

/// `[[org]]` の 1 行（ADR-0033 D1）。DB が空のときだけ蒔かれる種。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgSeedConfig {
    /// 英小文字ケバブ（`secretary` / `coding-frontend` 等）。
    pub id: String,
    /// 日本語の役職名（SPEC §3.2 の言葉）。
    pub name: String,
    /// `secretary` / `department` / `section`。根の `secretary` は 1 つだけ。
    pub kind: OrgKind,
    /// 親の id。`secretary` 以外は必須（`Config::validate` が確認する）。
    #[serde(default)]
    pub parent_id: Option<String>,
    /// ADR-0027/0028 の `[[genres]] id`。持たなくてよい（部は課に振る）。
    #[serde(default)]
    pub genre: Option<String>,
    /// 担当の一言。
    #[serde(default)]
    pub brief: String,
    /// 同じ親の中での並び順。省略したらファイルの並び順（0 始まり）。
    #[serde(default)]
    pub position: Option<i64>,
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
    /// ADR-0021 D4: 委譲した子が `failed` になったときの親の扱い。
    /// `"retry_then_ask"`（既定。やり直し → 駄目なら人に質問して `blocked`）か `"ignore"`（子の失敗を見ない）。
    #[serde(default = "default_on_child_failure")]
    pub on_child_failure: String,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            max_delegate_per_run: default_max_delegate_per_run(),
            max_tree_depth: default_max_tree_depth(),
            max_tree_runs: default_max_tree_runs(),
            on_child_failure: default_on_child_failure(),
        }
    }
}

fn default_on_child_failure() -> String {
    "retry_then_ask".to_string()
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
    /// `worktree`（ADR-0019 D4 の既定の選択。git 管理下のプロジェクト）、`rsync`（設定の既定）、`none`（共有ファイルシステム）。
    #[serde(default = "default_cluster_sync")]
    pub sync: String,
    /// ADR-0032 D1: 接続の認証方式。`"manual"`（既定。taskd は接続を張らない。ADR-0018 D2 のまま）/
    /// `"publickey"`（鍵だけで入れる。ディスパッチャが自動で接続を試みる。ADR-0032 D3）/
    /// `"totp"`（publickey の後に検証コードが要る。GUI から中継する。ADR-0032 D4）。
    #[serde(default = "default_cluster_auth")]
    pub auth: String,
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
    /// ADR-0019 D1: worktree を置く親ディレクトリ（既定は `<project>/.taskd-worktrees`）。`sync = "worktree"` のときだけ使う。
    #[serde(default)]
    pub worktree_root: Option<PathBuf>,
    /// ADR-0019 D1: worktree を切り出す元（既定 `HEAD`）。
    #[serde(default = "default_worktree_base")]
    pub worktree_base: String,
    /// ADR-0019 D1: sparse-checkout で残すパス（空なら全追跡ファイル）。巨大な追跡データを外すのに使う。
    #[serde(default)]
    pub worktree_paths: Vec<String>,
    /// ADR-0019 D2: worktree をいつ消すか。`"never"`（既定、人が消す）のみ実装。
    #[serde(default = "default_remove_worktree_when")]
    pub remove_worktree_when: String,
}

fn default_cluster_concurrency() -> usize {
    2
}
fn default_cluster_sync() -> String {
    "rsync".to_string()
}
fn default_cluster_auth() -> String {
    "manual".to_string()
}
fn default_worktree_base() -> String {
    "HEAD".to_string()
}
fn default_remove_worktree_when() -> String {
    "never".to_string()
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
    #[serde(default)]
    pub acp: AcpAdapterConfig,
    #[serde(default)]
    pub paperqa: PaperQaAdapterConfig,
    #[serde(default)]
    pub local_deep_research: LdrAdapterConfig,
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
    /// ADR-0030 D2: 環境変数名 → `[secrets]` の秘密 id。`env` より優先。
    #[serde(default)]
    pub env_from_secrets: HashMap<String, String>,
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
    /// ADR-0030 D2: 環境変数名 → `[secrets]` の秘密 id。`env` より優先。
    #[serde(default)]
    pub env_from_secrets: HashMap<String, String>,
}

impl Default for ClaudeCodeAdapterConfig {
    fn default() -> Self {
        Self {
            command: default_claude_command(),
            extra_args: Vec::new(),
            permission_mode: default_permission_mode(),
            model: None,
            env: HashMap::new(),
            env_from_secrets: HashMap::new(),
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
    /// ADR-0030 D2: 環境変数名 → `[secrets]` の秘密 id。`env` より優先。
    #[serde(default)]
    pub env_from_secrets: HashMap<String, String>,
}

impl Default for CodexAdapterConfig {
    fn default() -> Self {
        Self {
            command: default_codex_command(),
            extra_args: Vec::new(),
            model: None,
            env: HashMap::new(),
            env_from_secrets: HashMap::new(),
        }
    }
}

fn default_codex_command() -> String {
    "codex".to_string()
}

/// `acp` アダプタの設定（ADR-0026 D2）。最初の実装は `opencode acp`。`command`/`args`/`env`/`model` は
/// `[[providers]]` の行ごとに上書きできる（別の ACP エージェントを同居させるため。行の値は `ProviderConfig`
/// の `command`/`args`/`env`/`model` にある）。ここには `model` は無い（ACP はモデルを CLI フラグではなく
/// `session/set_config_option` で渡すので、行の `model` が空なら `None` になるだけで、この節に既定値を置く
/// 意味が無い。ADR-0026 D3）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcpAdapterConfig {
    /// 起動する ACP エージェントの実行ファイル。既定 `"opencode"`。
    #[serde(default = "default_acp_command")]
    pub command: String,
    /// コマンドへの引数。既定 `["acp"]`。
    #[serde(default = "default_acp_args")]
    pub args: Vec<String>,
    /// 追加の環境変数（共通分。行の `env` を重ねる。同名キーは行が優先）。
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// ADR-0030 D2: 環境変数名 → `[secrets]` の秘密 id。`env` より優先。
    #[serde(default)]
    pub env_from_secrets: HashMap<String, String>,
    /// `session/request_permission` への即答。`"allow"`（既定）| `"deny"`。
    #[serde(default = "default_acp_permission")]
    pub permission: task_worker::AcpPermission,
    /// `session/set_config_option` の `configId`（`session/new` の `configOptions[].id` と対にする）。
    /// 既定 `"model"`（opencode 1.18.31 で確認済み。ADR-0026 D3）。
    #[serde(default = "default_acp_model_option_id")]
    pub model_option_id: String,
    /// `initialize` の応答を待つ上限（秒）。初回はエージェント側のプロバイダ取得で数分かかりうる。既定 300。
    #[serde(default = "default_acp_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
}

impl Default for AcpAdapterConfig {
    fn default() -> Self {
        Self {
            command: default_acp_command(),
            args: default_acp_args(),
            env: HashMap::new(),
            env_from_secrets: HashMap::new(),
            permission: default_acp_permission(),
            model_option_id: default_acp_model_option_id(),
            startup_timeout_secs: default_acp_startup_timeout_secs(),
        }
    }
}

fn default_acp_command() -> String {
    "opencode".to_string()
}
fn default_acp_args() -> Vec<String> {
    vec!["acp".to_string()]
}
fn default_acp_permission() -> task_worker::AcpPermission {
    task_worker::AcpPermission::Allow
}
fn default_acp_model_option_id() -> String {
    "model".to_string()
}
fn default_acp_startup_timeout_secs() -> u64 {
    300
}

/// `paperqa` アダプタの設定（ADR-0027 D3）。フィールドの意味は `task_worker::PaperQaConfig`
/// （`crates/task-worker/src/paperqa.rs`）と同じ。`[[providers]] adapter = "paperqa"` の行ごとの
/// 上書きは `model`/`env` だけ（`ProviderConfig` の既存フィールドを再利用。ADR-0026 D2 と同じ作り）。
/// `settings`/`paper_directory`/`index_directory` は行では上書きしない
/// （調査タスクごとの `[[genres]]`/`[[roles]]` で使い分ける前提。必要になれば別 ADR で足す）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperQaAdapterConfig {
    /// 起動するコマンド名／パス。既定 `"pqa"`。
    #[serde(default = "default_paperqa_command")]
    pub command: String,
    /// `-s <name>`（拡張子は付けない。実機の仕様。ADR-0027 D3）。
    #[serde(default)]
    pub settings: Option<String>,
    /// `--agent.index.paper_directory`。相対パスは設定ファイルのディレクトリ基準で絶対化する。
    #[serde(default)]
    pub paper_directory: Option<PathBuf>,
    /// `--agent.index.index_directory` の親ディレクトリ（タスクごとのサブディレクトリはアダプタが足す）。
    /// 相対パスは設定ファイルのディレクトリ基準で絶対化する。
    #[serde(default)]
    pub index_directory: Option<PathBuf>,
    /// `--agent.index.name`。未指定ならタスク ID を使う。
    #[serde(default)]
    pub index_name: Option<String>,
    /// 末尾に追加する引数（`ask` の前に挿入する）。
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// 追加の環境変数（例: `OPENAI_API_KEY` / `OPENAI_BASE_URL`。LiteLLM 経由の OpenAI 互換エンドポイント向け）。
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// ADR-0030 D2: 環境変数名 → `[secrets]` の秘密 id。`env` より優先。
    #[serde(default)]
    pub env_from_secrets: HashMap<String, String>,
}

impl Default for PaperQaAdapterConfig {
    fn default() -> Self {
        Self {
            command: default_paperqa_command(),
            settings: None,
            paper_directory: None,
            index_directory: None,
            index_name: None,
            extra_args: Vec::new(),
            env: HashMap::new(),
            env_from_secrets: HashMap::new(),
        }
    }
}

fn default_paperqa_command() -> String {
    "pqa".to_string()
}

/// `local-deep-research` アダプタの設定（ADR-0029 D1）。フィールドの意味は
/// `task_worker::LdrConfig`（`crates/task-worker/src/local_deep_research.rs`）と同じ。
/// `[[providers]] adapter = "local-deep-research"` の行ごとの上書きは `model`/`env` だけ
/// （`ProviderConfig` の既存フィールドを再利用。`paperqa`/`acp` と同じ作り）。`settings` は行では
/// 上書きしない（`ProviderConfig.settings` は `paperqa` 専用のフィールドで、LDR では再利用しない。
/// 必要になれば別 ADR で行ごとの上書きを足す）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LdrAdapterConfig {
    /// 起動するコマンド（LDR を入れた venv の python）。既定 `"python3"`。
    #[serde(default = "default_ldr_command")]
    pub command: String,
    /// `quick`（既定）| `detailed` | `report`。
    #[serde(default)]
    pub mode: task_worker::LdrMode,
    #[serde(default)]
    pub iterations: Option<u32>,
    #[serde(default)]
    pub questions_per_iteration: Option<u32>,
    /// `settings_override` に渡すキー。値は文字列で書き、数値・真偽値・JSON 配列/オブジェクトに見える
    /// ものはランナー（Python）側で変換する（ADR-0029 D1/D3: TOML の型を混ぜない）。
    #[serde(default)]
    pub settings: HashMap<String, String>,
    /// 追加の環境変数（例: `search.tool` に対応する SearXNG の URL 等は `settings` 側。ここは
    /// LiteLLM/OpenAI 互換エンドポイントの鍵など、プロセス環境変数として渡すもの）。
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// ADR-0030 D2: 環境変数名 → `[secrets]` の秘密 id（例: `LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily"`）。
    /// `env` より優先。
    #[serde(default)]
    pub env_from_secrets: HashMap<String, String>,
    /// `[adapters.local_deep_research.evidence]`（ADR-0031 D2）: 決定的な証拠ゲートの閾値。
    /// 意味は `task_worker::EvidenceThresholds` と同じ。
    #[serde(default)]
    pub evidence: task_worker::EvidenceThresholds,
}

impl Default for LdrAdapterConfig {
    fn default() -> Self {
        Self {
            command: default_ldr_command(),
            mode: task_worker::LdrMode::default(),
            iterations: None,
            questions_per_iteration: None,
            settings: HashMap::new(),
            env: HashMap::new(),
            env_from_secrets: HashMap::new(),
            evidence: task_worker::EvidenceThresholds::default(),
        }
    }
}

fn default_ldr_command() -> String {
    "python3".to_string()
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
    /// ADR-0030 D2: 環境変数名 → `[secrets]` の秘密 id。この行の `env` より優先（優先順は
    /// taskd の環境 < `[adapters.*].env` < `[adapters.*].env_from_secrets` < 行の `env` < 行の `env_from_secrets`）。
    #[serde(default)]
    pub env_from_secrets: HashMap<String, String>,
    /// ADR-0024 D2: `true` なら `[accounts]` のプールから残量に基づいてアカウントを選ぶ。`adapter = "claude-code"`
    /// かつ `[accounts]` があるときだけ有効（既定 `false`）。
    #[serde(default)]
    pub account_pool: bool,
    /// ADR-0026 D2: `adapter = "acp"` のときだけ意味を持つ、この行の ACP エージェント実行ファイルの上書き
    /// （省略時は `[adapters.acp].command`）。他のアダプタで指定すると `Config::validate` が設定エラーにする。
    #[serde(default)]
    pub command: Option<String>,
    /// ADR-0026 D2: 上と同じ（引数）。省略時は `[adapters.acp].args`。
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// ADR-0027 D3: `adapter = "paperqa"` のときだけ意味を持つ、この行の PaperQA 設定ファイルの上書き
    /// （`-s <name>`、拡張子は付けない。省略時は `[adapters.paperqa].settings`）。他のアダプタで指定すると
    /// `Config::validate` が設定エラーにする。相対パスは設定ファイルのディレクトリ基準で絶対化する。
    #[serde(default)]
    pub settings: Option<String>,
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
        // ADR-0033 D1: 組織図の種。ファイルが無ければ設定エラー（書いたのに読めないのは事故なので黙らない）。
        if let Some(org_include) = &cfg.org_include {
            let path = {
                let p = PathBuf::from(org_include);
                if p.is_relative() { base.join(p) } else { p }
            };
            let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read { path: path.clone(), source })?;
            let file: OrgSeedFile = toml::from_str(&text)?;
            cfg.org = file.org;
        }
        if let Some(pattern) = &cfg.providers_include {
            let dir = providers_include_dir(pattern, &base)?;
            cfg.providers.extend(load_provider_files(&dir)?);
            cfg.providers_dir = Some(dir);
        }
        if let Some(accounts) = &mut cfg.accounts {
            if let Some(dir) = &accounts.claude_dir
                && dir.is_relative()
            {
                accounts.claude_dir = Some(base.join(dir));
            }
            if let Some(dir) = &accounts.codex_dir
                && dir.is_relative()
            {
                accounts.codex_dir = Some(base.join(dir));
            }
        }
        // ADR-0030 D1: `[secrets] dir` も他のパス設定と同じく設定ファイルのディレクトリ基準で絶対化する。
        if let Some(secrets) = &mut cfg.secrets
            && secrets.dir.is_relative()
        {
            secrets.dir = base.join(&secrets.dir);
        }
        // ADR-0027 D3: `[adapters.paperqa]` のパス設定は、他のパス設定と同じく設定ファイルのディレクトリ基準で
        // 絶対化する。`settings` は `pqa -s` に渡す文字列（拡張子無し）だが、パスの形をしているので同様に扱う。
        if let Some(dir) = &cfg.adapters.paperqa.paper_directory
            && dir.is_relative()
        {
            cfg.adapters.paperqa.paper_directory = Some(base.join(dir));
        }
        if let Some(dir) = &cfg.adapters.paperqa.index_directory
            && dir.is_relative()
        {
            cfg.adapters.paperqa.index_directory = Some(base.join(dir));
        }
        if let Some(settings) = &cfg.adapters.paperqa.settings
            && Path::new(settings).is_relative()
        {
            cfg.adapters.paperqa.settings = Some(base.join(settings).to_string_lossy().into_owned());
        }
        // ADR-0027 D3: 行ごとの `settings` の上書きも同じ基準で絶対化する。
        for p in &mut cfg.providers {
            if let Some(settings) = &p.settings
                && Path::new(settings).is_relative()
            {
                p.settings = Some(base.join(settings).to_string_lossy().into_owned());
            }
        }
        cfg.validate()?;
        // API を有効にするなら、トークンが読めることを起動時に確かめる（exit 2）。
        if cfg.api.listen.is_some() {
            cfg.api.read_token()?;
        }
        Ok(cfg)
    }

    /// ADR-0033 D1: `[[org]]` の種を `OrgNode` に写す（`position` を省略した行はファイルの並び順）。
    /// 親が先に来るよう、`parent_id` の依存順（secretary → 部 → 課）に並べ替えて返す。
    pub fn org_nodes(&self, now: time::OffsetDateTime) -> Vec<OrgNode> {
        let mut nodes: Vec<OrgNode> = self
            .org
            .iter()
            .enumerate()
            .map(|(i, seed)| OrgNode {
                id: seed.id.clone(),
                parent_id: seed.parent_id.clone(),
                name: seed.name.clone(),
                kind: seed.kind,
                genre: seed.genre.clone(),
                brief: seed.brief.clone(),
                position: seed.position.unwrap_or(i as i64),
                created_at: now,
                updated_at: now,
            })
            .collect();
        nodes.sort_by_key(|n| match n.kind {
            OrgKind::Secretary => 0,
            OrgKind::Department => 1,
            OrgKind::Section => 2,
        });
        nodes
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
                && p.adapter != task_worker::AcpAdapter::ID
                && p.adapter != task_worker::PaperQaAdapter::ID
                && p.adapter != task_worker::LdrAdapter::ID
            {
                return Err(ConfigError::Invalid(format!(
                    "provider {}: adapter {:?} is not available in this build (fake, claude-code, codex, acp, paperqa, local-deep-research only)",
                    p.id, p.adapter
                )));
            }
            if p.concurrency == 0 {
                return Err(ConfigError::Invalid(format!("provider {}: concurrency must be >= 1", p.id)));
            }
            // ADR-0026 D2: `command`/`args` は `adapter = "acp"` の行だけで意味を持つ。他のアダプタに書いたら
            // 静かに無視せず設定エラーにする（書いた本人の勘違いを早く見つけるため）。
            if p.adapter != task_worker::AcpAdapter::ID && (p.command.is_some() || p.args.is_some()) {
                return Err(ConfigError::Invalid(format!(
                    "provider {}: command/args are only allowed when adapter = \"acp\" (ADR-0026 D2)",
                    p.id
                )));
            }
            // ADR-0027 D3: `settings` は `adapter = "paperqa"` の行だけで意味を持つ（acp の `command`/`args` と同じ考え方）。
            if p.adapter != task_worker::PaperQaAdapter::ID && p.settings.is_some() {
                return Err(ConfigError::Invalid(format!(
                    "provider {}: settings is only allowed when adapter = \"paperqa\" (ADR-0027 D3)",
                    p.id
                )));
            }
            // ADR-0024 D2 / ADR-0025 D1: `account_pool = true` は claude-code か codex だけ、かつ `[accounts]` に
            // そのアダプタの根ディレクトリが設定されている必要がある。
            if p.account_pool {
                let Some(account_adapter) = AccountAdapter::parse(&p.adapter) else {
                    return Err(ConfigError::Invalid(format!(
                        "provider {}: account_pool = true requires adapter = \"claude-code\" or \"codex\"",
                        p.id
                    )));
                };
                match &self.accounts {
                    Some(accounts) if accounts.root_for(account_adapter).is_some() => {}
                    Some(_) => {
                        return Err(ConfigError::Invalid(format!(
                            "provider {}: account_pool = true requires [accounts] {} to be set",
                            p.id,
                            match account_adapter {
                                AccountAdapter::ClaudeCode => "claude_dir",
                                AccountAdapter::Codex => "codex_dir",
                            }
                        )));
                    }
                    None => {
                        return Err(ConfigError::Invalid(format!(
                            "provider {}: account_pool = true requires an [accounts] section",
                            p.id
                        )));
                    }
                }
            }
        }
        if let Some(accounts) = &self.accounts
            && accounts.claude_dir.is_none()
            && accounts.codex_dir.is_none()
        {
            return Err(ConfigError::Invalid(
                "[accounts] requires at least one of claude_dir / codex_dir".into(),
            ));
        }
        if let Some(accounts) = &self.accounts
            && accounts.max_runs_per_account == 0
        {
            return Err(ConfigError::Invalid("[accounts] max_runs_per_account must be >= 1".into()));
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
            if !matches!(c.sync.as_str(), "rsync" | "none" | "worktree") {
                return Err(ConfigError::Invalid(format!(
                    "[[clusters]] {}: sync must be \"worktree\", \"rsync\" or \"none\" (got {:?})",
                    c.id, c.sync
                )));
            }
            // ADR-0032 D1: 認証方式は 3 つだけ。既定は "manual"（taskd は接続を張らない）。
            if !matches!(c.auth.as_str(), "manual" | "publickey" | "totp") {
                return Err(ConfigError::Invalid(format!(
                    "[[clusters]] {}: auth must be \"manual\", \"publickey\" or \"totp\" (got {:?})",
                    c.id, c.auth
                )));
            }
            // ADR-0019 D2: 自動削除は実装しない（実行結果を消してしまわないため）。
            if c.remove_worktree_when != "never" {
                return Err(ConfigError::Invalid(format!(
                    "[[clusters]] {}: remove_worktree_when must be \"never\" (got {:?}); remove the worktree by hand",
                    c.id, c.remove_worktree_when
                )));
            }
            if c.worktree_base.trim().is_empty() {
                return Err(ConfigError::Invalid(format!("[[clusters]] {}: worktree_base must not be empty", c.id)));
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
                && adapter != task_worker::AcpAdapter::ID
                && adapter != task_worker::PaperQaAdapter::ID
                && adapter != task_worker::LdrAdapter::ID
            {
                return Err(ConfigError::Invalid(format!(
                    "[[roles]] {}: adapter {adapter:?} is not available in this build (fake, claude-code, codex, acp, paperqa, local-deep-research only)",
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
        // ADR-0027 D1: 分野の id は重複させない。`default_role` と `roles` の各要素は `[[roles]]` に存在すること、
        // `default_role`（あれば）は `roles` に含まれること。
        let mut genre_ids = std::collections::HashSet::new();
        for g in &self.genres {
            if g.id.trim().is_empty() {
                return Err(ConfigError::Invalid("[[genres]] id must not be empty".to_string()));
            }
            if !genre_ids.insert(&g.id) {
                return Err(ConfigError::Invalid(format!("duplicate genre id: {}", g.id)));
            }
            for role_id in &g.roles {
                if !role_ids.contains(role_id) {
                    return Err(ConfigError::Invalid(format!(
                        "[[genres]] {}: role {role_id:?} in roles is not defined in [[roles]]",
                        g.id
                    )));
                }
            }
            if let Some(default_role) = &g.default_role {
                if !role_ids.contains(default_role) {
                    return Err(ConfigError::Invalid(format!(
                        "[[genres]] {}: default_role {default_role:?} is not defined in [[roles]]",
                        g.id
                    )));
                }
                if !g.roles.iter().any(|r| r == default_role) {
                    return Err(ConfigError::Invalid(format!(
                        "[[genres]] {}: default_role {default_role:?} must be included in roles",
                        g.id
                    )));
                }
            }
        }
        // ADR-0033 D1: 組織図の種。id は重複させず英小文字ケバブ、`secretary` はちょうど 1 つ、
        // それ以外の親は同じファイル内に居ること、`genre` は `[[genres]]` にあること。
        // 木としての整合（循環・種類の順序）はストアの `org_upsert` が最終的に見る。
        let mut org_ids = std::collections::HashSet::new();
        let mut secretaries = 0usize;
        for node in &self.org {
            if !valid_org_id(&node.id) {
                return Err(ConfigError::Invalid(format!(
                    "[[org]] id {:?} must be lowercase kebab-case",
                    node.id
                )));
            }
            if !org_ids.insert(node.id.as_str()) {
                return Err(ConfigError::Invalid(format!("duplicate org id: {}", node.id)));
            }
            if node.name.trim().is_empty() {
                return Err(ConfigError::Invalid(format!("[[org]] {}: name must not be empty", node.id)));
            }
            if node.kind == OrgKind::Secretary {
                secretaries += 1;
            }
            if let Some(genre) = &node.genre
                && !genre_ids.contains(genre)
            {
                return Err(ConfigError::Invalid(format!(
                    "[[org]] {}: genre {genre:?} is not defined in [[genres]]",
                    node.id
                )));
            }
        }
        if !self.org.is_empty() && secretaries != 1 {
            return Err(ConfigError::Invalid(format!(
                "[[org]] must contain exactly one node with kind = \"secretary\" (found {secretaries})"
            )));
        }
        for node in &self.org {
            match (&node.parent_id, node.kind) {
                (Some(parent), _) if !org_ids.contains(parent.as_str()) => {
                    return Err(ConfigError::Invalid(format!(
                        "[[org]] {}: parent_id {parent:?} is not one of the [[org]] entries",
                        node.id
                    )));
                }
                (Some(_), OrgKind::Secretary) => {
                    return Err(ConfigError::Invalid(format!(
                        "[[org]] {}: the secretary is the root and must not have a parent_id",
                        node.id
                    )));
                }
                (None, OrgKind::Secretary) => {}
                (None, _) => {
                    return Err(ConfigError::Invalid(format!(
                        "[[org]] {}: parent_id is required (only the secretary is a root)",
                        node.id
                    )));
                }
                _ => {}
            }
        }
        // ADR-0016 D2 / M6: 0 の上限は「委譲を止める」ではなく設定ミス（拒否理由が毎回出るだけ）なので拒否する。
        // ADR-0021 D4: 知らない値は設定エラー（黙って既定に落とさない）。
        if !matches!(self.delegation.on_child_failure.as_str(), "retry_then_ask" | "ignore") {
            return Err(ConfigError::Invalid(format!(
                "[delegation] on_child_failure must be \"retry_then_ask\" or \"ignore\" (got {:?})",
                self.delegation.on_child_failure
            )));
        }
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
            genres: self.genre_specs(),
            delegation: self.delegation_limits(),
            accounts: self.accounts.as_ref().map(|a| AccountsRuntimeConfig {
                roots: a.roots(),
                max_runs_per_account: a.max_runs_per_account,
                check_model: a.check_model.clone(),
                fallback_cooldown_secs: self.error_cooldown_secs,
            }),
        }
    }

    /// ADR-0024 D1 / ADR-0025 D1: `[accounts]` の下の `account_pool = true` のプロバイダ id（重複なし）。
    pub fn account_pool_providers(&self) -> std::collections::HashSet<String> {
        self.providers
            .iter()
            .filter(|p| p.account_pool)
            .map(|p| p.id.clone())
            .collect()
    }

    /// ADR-0024 D1 / ADR-0025 D1: `[accounts]` の設定された根ディレクトリ（claude_dir・codex_dir）をそれぞれ
    /// 0700 で作る（無ければ）。`[accounts]` が無ければ何もしない。
    pub fn ensure_accounts_dir(&self) -> Result<(), ConfigError> {
        let Some(accounts) = &self.accounts else {
            return Ok(());
        };
        for dir in accounts.roots().values() {
            if dir.exists() {
                continue;
            }
            std::fs::create_dir_all(dir).map_err(|source| ConfigError::Read { path: dir.clone(), source })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o700);
                std::fs::set_permissions(dir, perms).map_err(|source| ConfigError::Read {
                    path: dir.clone(),
                    source,
                })?;
            }
        }
        Ok(())
    }

    /// ADR-0030 D1: `[secrets] dir` を 0700 で作る（無ければ）。`[secrets]` が無ければ何もしない。
    pub fn ensure_secrets_dir(&self) -> Result<(), ConfigError> {
        let Some(secrets) = &self.secrets else {
            return Ok(());
        };
        if secrets.dir.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&secrets.dir).map_err(|source| ConfigError::Read { path: secrets.dir.clone(), source })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&secrets.dir, perms).map_err(|source| ConfigError::Read {
                path: secrets.dir.clone(),
                source,
            })?;
        }
        Ok(())
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

    /// ADR-0027 D1: `[[genres]]` を task-core の型に写す（設定の順）。
    pub fn genre_specs(&self) -> Vec<task_core::GenreSpec> {
        self.genres
            .iter()
            .map(|g| task_core::GenreSpec {
                id: g.id.clone(),
                description: g.description.clone(),
                capabilities: g.capabilities.clone(),
                input_artifacts: g.input_artifacts.clone(),
                output_artifacts: g.output_artifacts.clone(),
                default_role: g.default_role.clone(),
                roles: g.roles.clone(),
            })
            .collect()
    }

    /// ADR-0016 D2: `[delegation]` を task-core の型に写す。
    pub fn delegation_limits(&self) -> DelegationLimits {
        DelegationLimits {
            max_delegate_per_run: self.delegation.max_delegate_per_run,
            max_tree_depth: self.delegation.max_tree_depth,
            max_tree_runs: self.delegation.max_tree_runs,
            on_child_failure: match self.delegation.on_child_failure.as_str() {
                "ignore" => task_core::OnChildFailure::Ignore,
                _ => task_core::OnChildFailure::RetryThenAsk,
            },
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
                        sync: match c.sync.as_str() {
                            "none" => task_worker::SyncMode::None,
                            "worktree" => task_worker::SyncMode::Worktree,
                            _ => task_worker::SyncMode::Rsync,
                        },
                        delete_on_push: c.delete_on_push,
                        setup: c.setup.clone(),
                        env,
                        rsync_excludes: c.rsync_excludes.clone(),
                        auth: c.auth.clone(),
                        worktree: task_worker::WorktreeSettings {
                            root: c.worktree_root.clone(),
                            base: c.worktree_base.clone(),
                            paths: c.worktree_paths.clone(),
                            ..Default::default()
                        },
                    },
                )
            })
            .collect()
    }

    /// ADR-0019 D2: `TaskDetail.worktree` を組み立てるのに要る分だけを写す。
    pub fn cluster_view_infos(&self) -> HashMap<String, task_ops::view::ClusterViewInfo> {
        self.clusters
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    task_ops::view::ClusterViewInfo {
                        sync: c.sync.clone(),
                        worktree_root: c.worktree_root.clone(),
                        auth: c.auth.clone(),
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

    /// `config/org.example.toml` が指す分野（`coding` / `literature`）を持つ最小の設定。
    const ORG_TEST_GENRES: &str = r#"
[[providers]]
id = "x"
adapter = "fake"

[[roles]]
id = "implementer"

[[roles]]
id = "literature-reader"

[[genres]]
id = "coding"
description = "コードを書く"
default_role = "implementer"
roles = ["implementer"]

[[genres]]
id = "literature"
description = "関連研究の調査"
default_role = "literature-reader"
roles = ["literature-reader"]
"#;

    // ---- ADR-0033 D1（Phase 23）: 組織図の種 ----

    /// 例の設定（`config/org.example.toml`）が読め、SPEC §3.2 の 10 ノードになる。
    /// `genre` は実在する分野 id（`coding` / `literature`）だけを指す。
    #[test]
    fn loads_the_org_example_and_maps_it_to_org_nodes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/org.example.toml"),
            dir.path().join("org.toml"),
        )
        .unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(&path, format!("db = \"t.sqlite3\"\norg_include = \"org.toml\"\n{}", ORG_TEST_GENRES)).unwrap();

        let cfg = Config::load(&path).unwrap();
        let ids: Vec<&str> = cfg.org.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "secretary",
                "coding",
                "coding-frontend",
                "coding-performance",
                "coding-poc",
                "research",
                "research-survey",
                "research-writing",
                "research-data",
                "infra",
            ]
        );
        let nodes = cfg.org_nodes(time::OffsetDateTime::now_utc());
        assert_eq!(nodes.len(), 10);
        // 親が子より先に来る（secretary → 部 → 課）。
        let order: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(order[0], "secretary");
        assert!(order.iter().position(|id| *id == "coding") < order.iter().position(|id| *id == "coding-poc"));
        let survey = nodes.iter().find(|n| n.id == "research-survey").unwrap();
        assert_eq!(survey.kind, OrgKind::Section);
        assert_eq!(survey.genre.as_deref(), Some("literature"));
        assert_eq!(survey.parent_id.as_deref(), Some("research"));
        assert!(!survey.brief.is_empty());
        // 分野を当てていないノードもある（まだその分野が無い）。
        assert_eq!(nodes.iter().find(|n| n.id == "research-writing").unwrap().genre, None);
        assert_eq!(nodes.iter().filter(|n| n.kind == OrgKind::Secretary).count(), 1);
    }

    /// `[[genres]]` に無い分野・重複 id・秘書が 0 か 2・知らない親は設定エラー。
    #[test]
    fn rejects_org_seeds_that_do_not_form_one_tree() {
        let base = "db = \"t.sqlite3\"\norg_include = \"org.toml\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n";
        let load = |org: &str| -> Result<Config, ConfigError> {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("org.toml"), org).unwrap();
            let path = dir.path().join("taskd.toml");
            std::fs::write(&path, base).unwrap();
            Config::load(&path)
        };
        let secretary = "[[org]]\nid = \"secretary\"\nname = \"秘書\"\nkind = \"secretary\"\n";
        load(secretary).expect("a lone secretary is fine");

        let err = load(&format!("{secretary}[[org]]\nid = \"coding\"\nname = \"部\"\nkind = \"department\"\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("parent_id is required"), "{err}");

        let err = load("[[org]]\nid = \"coding\"\nname = \"部\"\nkind = \"department\"\nparent_id = \"secretary\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("exactly one node"), "{err}");

        let err = load(&format!("{secretary}{secretary}")).unwrap_err().to_string();
        assert!(err.contains("duplicate org id"), "{err}");

        let err = load(&format!(
            "{secretary}[[org]]\nid = \"coding\"\nname = \"部\"\nkind = \"department\"\nparent_id = \"nobody\"\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("is not one of the [[org]] entries"), "{err}");

        let err = load(&format!(
            "{secretary}[[org]]\nid = \"X\"\nname = \"部\"\nkind = \"department\"\nparent_id = \"secretary\"\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("kebab-case"), "{err}");

        let err = load(&format!(
            "{secretary}[[org]]\nid = \"c\"\nname = \"課\"\nkind = \"section\"\nparent_id = \"secretary\"\ngenre = \"nope\"\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("is not defined in [[genres]]"), "{err}");
    }

    /// `org_include` を書かなければ種は空、書いたのにファイルが無ければ設定エラー。
    #[test]
    fn org_include_is_optional_but_must_exist_when_written() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(cfg.org.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(&path, "db = \"t.sqlite3\"\norg_include = \"missing.toml\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(matches!(Config::load(&path), Err(ConfigError::Read { .. })));
    }

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

    /// ADR-0019: `sync = "worktree"` が読めて、worktree の設定が `ClusterSpec` と `ViewContext` に写ること。
    /// 例の設定ファイル（config/taskd.clusters.example.toml）もここで一度読んで、書き間違いを拾う。
    #[test]
    fn parses_worktree_sync_and_maps_it_to_the_worker_settings() {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/taskd.clusters.example.toml"));
        let cfg = Config::load(path).unwrap();
        cfg.validate().unwrap();
        let specs = cfg.cluster_specs();
        assert_eq!(specs["pegasus"].sync, task_worker::SyncMode::Worktree);
        // ADR-0032 D1: pegasus/sirius は 2 要素認証（totp）、fern03 は鍵だけで入れる（publickey）の例。
        assert_eq!(specs["pegasus"].auth, "totp");
        assert_eq!(specs["sirius"].auth, "totp");
        assert_eq!(specs["fern03"].auth, "publickey");

        let cfg: Config = toml::from_str(
            r#"[[providers]]
id = "x"
adapter = "fake"
[[clusters]]
id = "pegasus"
host = "pegasus"
sync = "worktree"
worktree_root = "/work/NBB/rmaeda/.taskd-worktrees"
worktree_base = "origin/main"
worktree_paths = ["src", "Cargo.toml"]
"#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let spec = &cfg.cluster_specs()["pegasus"];
        assert_eq!(spec.sync, task_worker::SyncMode::Worktree);
        assert_eq!(spec.worktree.root.as_deref(), Some(Path::new("/work/NBB/rmaeda/.taskd-worktrees")));
        assert_eq!(spec.worktree.base, "origin/main");
        assert_eq!(spec.worktree.paths, vec!["src".to_string(), "Cargo.toml".to_string()]);
        assert_eq!(spec.worktree.branch_prefix, "taskd/");
        let view = &cfg.cluster_view_infos()["pegasus"];
        assert_eq!(view.sync, "worktree");
        assert_eq!(view.worktree_root.as_deref(), Some(Path::new("/work/NBB/rmaeda/.taskd-worktrees")));
    }

    /// 既定は `sync = "rsync"` のまま（ADR-0018 からの互換）。知らない sync と自動削除は設定エラー。
    #[test]
    fn rejects_unknown_sync_modes_and_worktree_auto_removal() {
        let base = |extra: &str| {
            format!(
                r#"[[providers]]
id = "x"
adapter = "fake"
[[clusters]]
id = "c"
host = "h"
{extra}
"#
            )
        };
        let cfg: Config = toml::from_str(&base("")).unwrap();
        assert_eq!(cfg.clusters[0].sync, "rsync");
        assert_eq!(cfg.cluster_specs()["c"].sync, task_worker::SyncMode::Rsync);

        let cfg: Config = toml::from_str(&base(r#"sync = "worktre""#)).unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("sync must be"), "{err}");

        let cfg: Config = toml::from_str(&base(r#"remove_worktree_when = "done""#)).unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("remove_worktree_when"), "{err}");
    }

    /// ADR-0032 D1: `auth` の既定は `"manual"`（省略した既存設定の挙動は変わらない）。3 値だけ許し、
    /// `ClusterSpec` と `ClusterViewInfo` の両方に写る。それ以外は設定エラー。
    #[test]
    fn cluster_auth_defaults_to_manual_and_only_three_values_are_accepted() {
        let base = |extra: &str| {
            format!(
                r#"[[providers]]
id = "x"
adapter = "fake"
[[clusters]]
id = "c"
host = "h"
{extra}
"#
            )
        };
        // 既定: auth を書かなければ "manual"。既存設定の挙動が変わらない。
        let cfg: Config = toml::from_str(&base("")).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.clusters[0].auth, "manual");
        assert_eq!(cfg.cluster_specs()["c"].auth, "manual");
        assert_eq!(cfg.cluster_view_infos()["c"].auth, "manual");

        for auth in ["manual", "publickey", "totp"] {
            let cfg: Config = toml::from_str(&base(&format!(r#"auth = "{auth}""#))).unwrap();
            cfg.validate().unwrap();
            assert_eq!(cfg.clusters[0].auth, auth);
            assert_eq!(cfg.cluster_specs()["c"].auth, auth);
            assert_eq!(cfg.cluster_view_infos()["c"].auth, auth);
        }

        let cfg: Config = toml::from_str(&base(r#"auth = "password""#)).unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("auth must be"), "{err}");
        assert!(err.contains("password"), "{err}");
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

    /// ADR-0026 D2: `[adapters.acp]` の既定値（opencode を素の状態で使う）。
    #[test]
    fn accepts_acp_adapter_with_default_config() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"acp\"\n").unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.acp.command, "opencode");
        assert_eq!(cfg.adapters.acp.args, vec!["acp".to_string()]);
        assert_eq!(cfg.adapters.acp.permission, task_worker::AcpPermission::Allow);
        assert_eq!(cfg.adapters.acp.model_option_id, "model");
        assert_eq!(cfg.adapters.acp.startup_timeout_secs, 300);
        assert!(cfg.adapters.acp.env.is_empty());
        assert!(cfg.providers[0].command.is_none());
        assert!(cfg.providers[0].args.is_none());
    }

    #[test]
    fn rejects_unknown_fields_in_acp_adapter_config() {
        let text = "[[providers]]\nid = \"x\"\nadapter = \"acp\"\n\n[adapters.acp]\nbogus = 1\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    /// ADR-0026 D2: `permission` は `AcpPermission` の `allow`/`deny` 以外は設定エラー（deny_unknown ではなく
    /// serde の enum 検証で拒否される）。
    #[test]
    fn rejects_unknown_acp_permission_value() {
        let text = "[[providers]]\nid = \"x\"\nadapter = \"acp\"\n\n[adapters.acp]\npermission = \"maybe\"\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    /// ADR-0026 D2: `command`/`args` は `adapter = "acp"` の行だけで意味を持つ。行ごとに上書きできる。
    #[test]
    fn command_and_args_are_only_allowed_on_acp_providers_and_override_per_row() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\ncommand = \"whatever\"\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("command/args are only allowed when adapter"), "{err}");

        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"codex\"\nargs = [\"x\"]\n").unwrap();
        assert!(cfg.validate().is_err());

        let cfg: Config = toml::from_str(
            "[[providers]]\nid = \"x\"\nadapter = \"acp\"\ncommand = \"goose\"\nargs = [\"acp\"]\n",
        )
        .unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.providers[0].command.as_deref(), Some("goose"));
        assert_eq!(cfg.providers[0].args.as_deref(), Some(&["acp".to_string()][..]));
    }

    /// ADR-0026 D6: 冷スタート用の例の設定ファイルが読め、Phase 15 の設定検証を通る。
    #[test]
    fn loads_acp_opencode_example_config() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/taskd.acp-opencode.example.toml"
        ));
        let cfg = Config::load(path).unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.providers[0].adapter, "acp");
        assert_eq!(cfg.adapters.acp.command, "opencode");
        assert_eq!(cfg.adapters.acp.permission, task_worker::AcpPermission::Allow);
        assert_eq!(cfg.providers[0].env.get("OPENCODE_DISABLE_PROJECT_CONFIG").map(String::as_str), Some("1"));
    }

    /// ADR-0027 D3: `[adapters.paperqa]` の既定値（`pqa` を素の状態で使う）。
    #[test]
    fn accepts_paperqa_adapter_with_default_config() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"paperqa\"\n").unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.paperqa.command, "pqa");
        assert!(cfg.adapters.paperqa.settings.is_none());
        assert!(cfg.adapters.paperqa.paper_directory.is_none());
        assert!(cfg.adapters.paperqa.index_directory.is_none());
        assert!(cfg.adapters.paperqa.index_name.is_none());
        assert!(cfg.adapters.paperqa.extra_args.is_empty());
        assert!(cfg.adapters.paperqa.env.is_empty());
        assert!(cfg.providers[0].settings.is_none());
    }

    #[test]
    fn rejects_unknown_fields_in_paperqa_adapter_config() {
        let text = "[[providers]]\nid = \"x\"\nadapter = \"paperqa\"\n\n[adapters.paperqa]\nbogus = 1\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    /// ADR-0029 D1: `[adapters.local_deep_research]` の既定値（`python3` を素の状態で使う。mode 既定 quick）。
    #[test]
    fn accepts_local_deep_research_adapter_with_default_config() {
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"local-deep-research\"\n").unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.local_deep_research.command, "python3");
        assert_eq!(cfg.adapters.local_deep_research.mode, task_worker::LdrMode::Quick);
        assert!(cfg.adapters.local_deep_research.iterations.is_none());
        assert!(cfg.adapters.local_deep_research.questions_per_iteration.is_none());
        assert!(cfg.adapters.local_deep_research.settings.is_empty());
        assert!(cfg.adapters.local_deep_research.env.is_empty());
        // ADR-0031 D2: 既定の閾値。
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_search_results, 5);
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_sources, 3);
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_cited, 2);
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_domains, 2);
    }

    #[test]
    fn rejects_unknown_fields_in_local_deep_research_adapter_config() {
        let text =
            "[[providers]]\nid = \"x\"\nadapter = \"local-deep-research\"\n\n[adapters.local_deep_research]\nbogus = 1\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    /// ADR-0031 D2: `[adapters.local_deep_research.evidence]` を読める。`0` を書けばその項目は無効になる
    /// （下の値のとおり読めることだけをここでは確認する。ゲートの判定自体は `task_worker::local_deep_research`
    /// 側のテスト）。未知のキーは拒否する。
    #[test]
    fn reads_local_deep_research_evidence_thresholds() {
        let text = "[[providers]]\nid = \"x\"\nadapter = \"local-deep-research\"\n\n\
             [adapters.local_deep_research.evidence]\n\
             min_search_results = 10\n\
             min_sources = 4\n\
             min_cited = 1\n\
             min_domains = 0\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate().is_ok());
        let ev = cfg.adapters.local_deep_research.evidence;
        assert_eq!(ev.min_search_results, 10);
        assert_eq!(ev.min_sources, 4);
        assert_eq!(ev.min_cited, 1);
        assert_eq!(ev.min_domains, 0);
    }

    #[test]
    fn rejects_unknown_fields_in_local_deep_research_evidence_table() {
        let text = "[[providers]]\nid = \"x\"\nadapter = \"local-deep-research\"\n\n\
             [adapters.local_deep_research.evidence]\nbogus = 1\n";
        assert!(toml::from_str::<Config>(text).is_err());
    }

    /// ADR-0029 D1: `mode`/`iterations`/`questions_per_iteration`/`settings`/`env` を読める。
    #[test]
    fn reads_local_deep_research_adapter_settings() {
        let text = "[adapters.local_deep_research]\n\
             command = \"/home/u/taskd/ldr/.venv/bin/python\"\n\
             mode = \"detailed\"\n\
             iterations = 2\n\
             questions_per_iteration = 2\n\
             env = { OPENAI_API_KEY = \"unused\" }\n\
             \n\
             [adapters.local_deep_research.settings]\n\
             \"llm.provider\" = \"openai_endpoint\"\n\
             \"search.engine.web.searxng.default_params.engines\" = \"[\\\"bing\\\"]\"\n\
             \n\
             [[providers]]\n\
             id = \"ldr\"\n\
             adapter = \"local-deep-research\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.local_deep_research.command, "/home/u/taskd/ldr/.venv/bin/python");
        assert_eq!(cfg.adapters.local_deep_research.mode, task_worker::LdrMode::Detailed);
        assert_eq!(cfg.adapters.local_deep_research.iterations, Some(2));
        assert_eq!(cfg.adapters.local_deep_research.questions_per_iteration, Some(2));
        assert_eq!(
            cfg.adapters.local_deep_research.settings.get("llm.provider").map(String::as_str),
            Some("openai_endpoint")
        );
        assert_eq!(
            cfg.adapters
                .local_deep_research
                .settings
                .get("search.engine.web.searxng.default_params.engines")
                .map(String::as_str),
            Some("[\"bing\"]")
        );
        assert_eq!(cfg.adapters.local_deep_research.env.get("OPENAI_API_KEY").map(String::as_str), Some("unused"));
    }

    /// ADR-0029 D1: `local-deep-research` の行には `paperqa` 専用の `settings`（`ProviderConfig.settings`）を
    /// 書けない（`paperqa` の行だけで意味を持つフィールドのまま。LDR の設定は `[adapters.local_deep_research]`
    /// の table 側だけで持つ、という taskd 側の実装判断）。
    #[test]
    fn rejects_row_level_settings_field_for_local_deep_research_provider() {
        let cfg: Config = toml::from_str(
            "[[providers]]\nid = \"x\"\nadapter = \"local-deep-research\"\nsettings = \"whatever\"\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("settings is only allowed when adapter"), "{err}");
    }

    /// ADR-0027 D3: `settings` は `adapter = "paperqa"` の行だけで意味を持つ。行ごとに上書きできる
    /// （`acp` の `command`/`args` と同じ作り）。
    #[test]
    fn settings_is_only_allowed_on_paperqa_providers_and_overrides_per_row() {
        let cfg: Config =
            toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\nsettings = \"whatever\"\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("settings is only allowed when adapter"), "{err}");

        let cfg: Config = toml::from_str(
            "[[providers]]\nid = \"x\"\nadapter = \"paperqa\"\nsettings = \"/settings/other\"\n",
        )
        .unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.providers[0].settings.as_deref(), Some("/settings/other"));
    }

    /// ADR-0027 D3: `[adapters.paperqa]` の `paper_directory`/`index_directory`/`settings`（共通・行の上書き
    /// どちらも）は他のパス設定と同じく設定ファイルのディレクトリ基準で絶対化する。
    #[test]
    fn paperqa_paths_are_resolved_relative_to_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "[adapters.paperqa]\n\
             paper_directory = \"papers\"\n\
             index_directory = \"index\"\n\
             settings = \"settings/qwen-local\"\n\
             \n\
             [[providers]]\n\
             id = \"pqa\"\n\
             adapter = \"paperqa\"\n\
             settings = \"settings/other\"\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let base = dir.path().canonicalize().unwrap();
        assert_eq!(cfg.adapters.paperqa.paper_directory, Some(base.join("papers")));
        assert_eq!(cfg.adapters.paperqa.index_directory, Some(base.join("index")));
        assert_eq!(
            cfg.adapters.paperqa.settings.as_deref(),
            Some(base.join("settings/qwen-local").to_string_lossy().into_owned().as_str())
        );
        assert_eq!(
            cfg.providers[0].settings.as_deref(),
            Some(base.join("settings/other").to_string_lossy().into_owned().as_str())
        );
    }

    /// ADR-0027 D3: 分野・調査ハーネスを両方載せた例の設定ファイルが読め、検証を通る。
    #[test]
    fn loads_research_example_config() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/taskd.research.example.toml"
        ));
        let cfg = Config::load(path).unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.paperqa.command, "/home/u/taskd/paperqa/.venv/bin/pqa");
        // `.json` を付けずに渡す（実機の仕様）。
        assert_eq!(cfg.adapters.paperqa.settings.as_deref(), Some("/home/u/taskd/paperqa/settings/qwen-local"));
        assert_eq!(
            cfg.adapters.paperqa.paper_directory.as_deref(),
            Some(Path::new("/home/u/taskd/paperqa/papers"))
        );
        assert_eq!(cfg.adapters.paperqa.env.get("OPENAI_BASE_URL").map(String::as_str), Some("http://127.0.0.1:18000/v1"));
        let paperqa_provider = cfg.providers.iter().find(|p| p.adapter == "paperqa").expect("paperqa provider");
        assert_eq!(paperqa_provider.model, "openai/qwen3.8-27b");
        let genre_ids: Vec<&str> = cfg.genres.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(genre_ids, vec!["coding", "literature"]);
        let literature = cfg.genres.iter().find(|g| g.id == "literature").expect("literature genre");
        assert_eq!(literature.default_role.as_deref(), Some("literature-reader"));
        assert_eq!(
            literature.roles,
            vec!["literature-scout".to_string(), "literature-reader".to_string(), "novelty-skeptic".to_string()]
        );
        // ADR-0028 D1: 能力・入出力の目安も読める。
        assert_eq!(
            literature.capabilities,
            vec!["学術文献の検索".to_string(), "引用グラフの探索".to_string(), "PDF 全文からの根拠抽出".to_string()]
        );
        assert_eq!(literature.input_artifacts, vec!["question".to_string(), "pdf".to_string(), "bibliography".to_string()]);
        assert_eq!(literature.output_artifacts, vec!["answer.md".to_string(), "citations.json".to_string()]);
        let coding = cfg.genres.iter().find(|g| g.id == "coding").expect("coding genre");
        assert!(!coding.capabilities.is_empty());
        assert!(!coding.input_artifacts.is_empty());
        assert!(!coding.output_artifacts.is_empty());
    }

    /// ADR-0029 D1/D2: Web 調査（Local Deep Research）の例の設定ファイルが読め、検証を通る。
    /// `web-research` 分野の manifest は ADR-0029 D2 のとおり。
    #[test]
    fn loads_web_research_example_config() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/taskd.web-research.example.toml"
        ));
        let cfg = Config::load(path).unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.adapters.local_deep_research.command, "/home/u/taskd/ldr/.venv/bin/python");
        assert_eq!(cfg.adapters.local_deep_research.mode, task_worker::LdrMode::Quick);
        // ADR-0031 D4: 既定は Tavily（鍵は `env_from_secrets` で渡す）。
        assert_eq!(
            cfg.adapters.local_deep_research.settings.get("search.tool").map(String::as_str),
            Some("tavily")
        );
        // 実機の罠（PROGRESS の Phase 21「真因: DNS」）: これが無いと、このホストの DNS では
        // LDR の DNS ピン留めが 5 秒で fail-closed し、どのエンジンでも「0 件」になる。
        assert_eq!(
            cfg.adapters.local_deep_research.env.get("RES_OPTIONS").map(String::as_str),
            Some("single-request")
        );
        let ldr_provider = cfg.providers.iter().find(|p| p.adapter == "local-deep-research").expect("ldr provider");
        assert_eq!(ldr_provider.model, "qwen3.8-27b");
        // ADR-0031 D2: 既定の証拠ゲート閾値を明示している。
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_search_results, 5);
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_sources, 3);
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_cited, 2);
        assert_eq!(cfg.adapters.local_deep_research.evidence.min_domains, 2);
        let genre = cfg.genres.iter().find(|g| g.id == "web-research").expect("web-research genre");
        assert_eq!(genre.default_role.as_deref(), Some("web-scout"));
        assert_eq!(genre.roles, vec!["web-scout".to_string()]);
        assert_eq!(genre.output_artifacts, vec!["report.md".to_string()]);
        let role = cfg.roles.iter().find(|r| r.id == "web-scout").expect("web-scout role");
        assert_eq!(role.adapter.as_deref(), Some("local-deep-research"));
        // ADR-0030 D1: `[secrets] dir` が読め、設定ファイル基準で絶対化される。鍵の値そのものはファイルに無い。
        let secrets = cfg.secrets.as_ref().expect("[secrets]");
        assert!(secrets.dir.is_absolute());
        assert_eq!(secrets.dir.file_name().and_then(|n| n.to_str()), Some("secrets"));
        assert!(!std::fs::read_to_string(path).unwrap().contains("tvly-"), "example config must not contain a real key");
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
        assert_eq!(
            cfg.delegation_limits(),
            DelegationLimits {
                max_delegate_per_run: 8,
                max_tree_depth: 5,
                max_tree_runs: 100,
                on_child_failure: task_core::OnChildFailure::RetryThenAsk,
            }
        );

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
        assert_eq!(
            limits,
            DelegationLimits {
                max_delegate_per_run: 3,
                max_tree_depth: 2,
                max_tree_runs: 100,
                on_child_failure: task_core::OnChildFailure::RetryThenAsk,
            }
        );
        let d = cfg.dispatch_config();
        assert_eq!(d.roles, specs);
        assert_eq!(d.delegation, limits);

        assert!(toml::from_str::<Config>("[[roles]]\nid = \"a\"\nbogus = 1\n").is_err());
        assert!(toml::from_str::<Config>("[delegation]\nbogus = 1\n").is_err());
    }

    /// ADR-0021 D4: `on_child_failure` は `retry_then_ask`（既定）と `ignore` だけ。知らない値は設定エラー。
    #[test]
    fn delegation_on_child_failure_is_parsed_and_validated() {
        let with = |v: &str| {
            format!("[delegation]\non_child_failure = \"{v}\"\n[[providers]]\nid = \"p\"\nadapter = \"fake\"\n")
        };
        let cfg: Config = toml::from_str(&with("ignore")).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.delegation_limits().on_child_failure, task_core::OnChildFailure::Ignore);

        let cfg: Config = toml::from_str(&with("retry_then_ask")).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.delegation_limits().on_child_failure, task_core::OnChildFailure::RetryThenAsk);

        let cfg: Config = toml::from_str(&with("fail_parent")).unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("on_child_failure"), "{err}");
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
            "invalid config: [[roles]] lead: adapter \"bogus\" is not available in this build (fake, claude-code, codex, acp, paperqa, local-deep-research only)"
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

    /// ADR-0027 D1: `[[genres]]` を読み、task-core の `GenreSpec` に写す。`[[genres]]` を書かない設定は
    /// 今までどおり動く（分野は任意）。
    #[test]
    fn genres_are_parsed_and_mapped() {
        let providers = "[[providers]]\nid = \"x\"\nadapter = \"fake\"\n";
        let cfg: Config = toml::from_str(providers).unwrap();
        assert!(cfg.validate().is_ok());
        assert!(cfg.genres.is_empty());
        assert!(cfg.genre_specs().is_empty());

        let text = format!(
            r#"[[roles]]
id = "lead"

[[roles]]
id = "implementer"

[[genres]]
id = "coding"
description = "write and fix code"
default_role = "implementer"
roles = ["lead", "implementer"]

[[genres]]
id = "related-research"
description = "先行研究の確認・新規性の検討"
capabilities = ["学術文献の検索", "引用グラフの探索", "PDF 全文からの根拠抽出"]
input_artifacts = ["question", "pdf", "bibliography"]
output_artifacts = ["answer.md", "citations.json"]
default_role = "lead"
roles = ["lead"]

{providers}"#
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        assert!(cfg.validate().is_ok());
        let specs = cfg.genre_specs();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].id, "coding");
        assert_eq!(specs[0].description, "write and fix code");
        assert_eq!(specs[0].default_role.as_deref(), Some("implementer"));
        assert_eq!(specs[0].roles, vec!["lead".to_string(), "implementer".to_string()]);
        // ADR-0028 D1: 3 フィールドを書かなければ空（既存設定との互換）。
        assert!(specs[0].capabilities.is_empty());
        assert!(specs[0].input_artifacts.is_empty());
        assert!(specs[0].output_artifacts.is_empty());
        // ADR-0028 D1: 書けば `GenreSpec` に写る。
        assert_eq!(
            specs[1].capabilities,
            vec!["学術文献の検索".to_string(), "引用グラフの探索".to_string(), "PDF 全文からの根拠抽出".to_string()]
        );
        assert_eq!(specs[1].input_artifacts, vec!["question".to_string(), "pdf".to_string(), "bibliography".to_string()]);
        assert_eq!(specs[1].output_artifacts, vec!["answer.md".to_string(), "citations.json".to_string()]);
        let d = cfg.dispatch_config();
        assert_eq!(d.genres, specs);

        assert!(toml::from_str::<Config>("[[genres]]\nid = \"a\"\ndescription = \"d\"\nbogus = 1\n").is_err());
    }

    /// ADR-0027 D1: 分野 id の重複、知らない役割を指す `roles`/`default_role`、`roles` に無い
    /// `default_role` は設定エラー。
    #[test]
    fn rejects_duplicate_genre_ids_and_genres_referencing_unknown_or_mismatched_roles() {
        let providers = "[[providers]]\nid = \"x\"\nadapter = \"fake\"\n";
        let roles = "[[roles]]\nid = \"lead\"\n\n[[roles]]\nid = \"implementer\"\n";

        let dup = format!(
            "{roles}[[genres]]\nid = \"coding\"\ndescription = \"d\"\n[[genres]]\nid = \"coding\"\ndescription = \"d\"\n{providers}"
        );
        let cfg: Config = toml::from_str(&dup).unwrap();
        assert_eq!(cfg.validate().unwrap_err().to_string(), "invalid config: duplicate genre id: coding");

        let empty_id = format!("[[genres]]\nid = \"  \"\ndescription = \"d\"\n{providers}");
        let cfg: Config = toml::from_str(&empty_id).unwrap();
        assert!(cfg.validate().unwrap_err().to_string().contains("id must not be empty"));

        let unknown_role_in_roles = format!(
            "{roles}[[genres]]\nid = \"coding\"\ndescription = \"d\"\nroles = [\"lead\", \"nobody\"]\n{providers}"
        );
        let cfg: Config = toml::from_str(&unknown_role_in_roles).unwrap();
        assert_eq!(
            cfg.validate().unwrap_err().to_string(),
            "invalid config: [[genres]] coding: role \"nobody\" in roles is not defined in [[roles]]"
        );

        let unknown_default_role = format!(
            "{roles}[[genres]]\nid = \"coding\"\ndescription = \"d\"\nroles = [\"lead\"]\ndefault_role = \"nobody\"\n{providers}"
        );
        let cfg: Config = toml::from_str(&unknown_default_role).unwrap();
        assert_eq!(
            cfg.validate().unwrap_err().to_string(),
            "invalid config: [[genres]] coding: default_role \"nobody\" is not defined in [[roles]]"
        );

        let default_role_not_in_roles = format!(
            "{roles}[[genres]]\nid = \"coding\"\ndescription = \"d\"\nroles = [\"lead\"]\ndefault_role = \"implementer\"\n{providers}"
        );
        let cfg: Config = toml::from_str(&default_role_not_in_roles).unwrap();
        assert_eq!(
            cfg.validate().unwrap_err().to_string(),
            "invalid config: [[genres]] coding: default_role \"implementer\" must be included in roles"
        );
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

    // ---- ADR-0024/0025: [accounts] / account_pool ----

    /// `account_pool = true` は `adapter = "claude-code"` かつ `[accounts] claude_dir` を要求する（ADR-0024 D2）。
    #[test]
    fn account_pool_requires_claude_code_adapter_and_accounts_section() {
        // account_pool のプロバイダはあるが [accounts] が無い。
        let cfg: Config = toml::from_str(
            "[[providers]]\nid = \"pool\"\nadapter = \"claude-code\"\naccount_pool = true\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("[accounts]"), "{err}");

        // [accounts] はあるが adapter が claude-code/codex でない。
        let cfg: Config = toml::from_str(
            "[accounts]\nclaude_dir = \"acct\"\n[[providers]]\nid = \"pool\"\nadapter = \"fake\"\naccount_pool = true\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("claude-code"), "{err}");

        // 両方あれば通る。
        let cfg: Config = toml::from_str(
            "[accounts]\nclaude_dir = \"acct\"\n[[providers]]\nid = \"pool\"\nadapter = \"claude-code\"\naccount_pool = true\n",
        )
        .unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.account_pool_providers(), ["pool".to_string()].into());
    }

    /// ADR-0025 D1: `account_pool = true` の codex プロバイダは `[accounts] codex_dir` を要求する
    /// （`claude_dir` だけでは足りない）。
    #[test]
    fn account_pool_for_codex_requires_codex_dir_specifically() {
        let cfg: Config = toml::from_str(
            "[accounts]\nclaude_dir = \"acct\"\n[[providers]]\nid = \"pool\"\nadapter = \"codex\"\naccount_pool = true\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("codex_dir"), "{err}");

        let cfg: Config = toml::from_str(
            "[accounts]\ncodex_dir = \"acct\"\n[[providers]]\nid = \"pool\"\nadapter = \"codex\"\naccount_pool = true\n",
        )
        .unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.account_pool_providers(), ["pool".to_string()].into());
    }

    /// `[accounts]` の既定値と、相対 `claude_dir`/`codex_dir` の解決（設定ファイル基準）。
    #[test]
    fn accounts_section_defaults_and_relative_dirs_are_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "[accounts]\nclaude_dir = \"claude-accounts\"\ncodex_dir = \"codex-accounts\"\n[[providers]]\nid = \"pool\"\nadapter = \"claude-code\"\naccount_pool = true\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let accounts = cfg.accounts.as_ref().unwrap();
        let claude_dir = accounts.claude_dir.clone().expect("claude_dir");
        let codex_dir = accounts.codex_dir.clone().expect("codex_dir");
        assert!(claude_dir.is_absolute());
        assert_eq!(claude_dir, dir.path().canonicalize().unwrap().join("claude-accounts"));
        assert!(codex_dir.is_absolute());
        assert_eq!(codex_dir, dir.path().canonicalize().unwrap().join("codex-accounts"));
        assert_eq!(accounts.max_runs_per_account, 2);
        assert_eq!(accounts.check_model, "haiku");

        let d = cfg.dispatch_config();
        let runtime = d.accounts.expect("dispatch_config carries [accounts]");
        assert_eq!(runtime.root_for(AccountAdapter::ClaudeCode), Some(&claude_dir));
        assert_eq!(runtime.root_for(AccountAdapter::Codex), Some(&codex_dir));
        assert_eq!(runtime.max_runs_per_account, 2);
        assert_eq!(runtime.check_model, "haiku");
        assert_eq!(runtime.fallback_cooldown_secs, cfg.error_cooldown_secs);
    }

    /// `max_runs_per_account = 0` は設定エラー。未知キーも拒否。どちらの根ディレクトリも無ければ設定エラー。
    #[test]
    fn accounts_section_rejects_zero_max_runs_and_unknown_keys() {
        let cfg: Config = toml::from_str(
            "[accounts]\nclaude_dir = \"acct\"\nmax_runs_per_account = 0\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("max_runs_per_account"), "{err}");

        assert!(toml::from_str::<Config>("[accounts]\nbogus = 1\n").is_err());

        // Neither claude_dir nor codex_dir: deserializes fine (both optional) but validate() rejects it.
        let cfg: Config = toml::from_str("[accounts]\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("claude_dir") && err.contains("codex_dir"), "{err}");
    }

    /// `ensure_accounts_dir` は設定された根ディレクトリ（claude_dir・codex_dir それぞれ）を 0700 で作る
    /// （無ければ）。`[accounts]` が無ければ何もしない。
    #[test]
    fn ensure_accounts_dir_creates_the_directories_with_0700() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "[accounts]\nclaude_dir = \"claude-accounts\"\ncodex_dir = \"codex-accounts\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let claude_dir = cfg.accounts.as_ref().unwrap().claude_dir.clone().unwrap();
        let codex_dir = cfg.accounts.as_ref().unwrap().codex_dir.clone().unwrap();
        assert!(!claude_dir.exists());
        assert!(!codex_dir.exists());
        cfg.ensure_accounts_dir().unwrap();
        assert!(claude_dir.is_dir());
        assert!(codex_dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for d in [&claude_dir, &codex_dir] {
                let mode = std::fs::metadata(d).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700);
            }
        }
        // 既にあれば触らない（既存の中身・権限を壊さない）。
        cfg.ensure_accounts_dir().unwrap();

        // [accounts] 無しは no-op。
        let no_accounts: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(no_accounts.ensure_accounts_dir().is_ok());
    }

    // ---- ADR-0030: [secrets] / env_from_secrets ----

    /// `[secrets] dir` を読み、相対パスを設定ファイル基準で絶対化する。
    #[test]
    fn secrets_dir_is_parsed_and_resolved_relative_to_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "[secrets]\ndir = \"secrets\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let secrets = cfg.secrets.as_ref().unwrap();
        assert!(secrets.dir.is_absolute());
        assert_eq!(secrets.dir, dir.path().canonicalize().unwrap().join("secrets"));

        // 節を書かなければ `None`。
        let no_secrets: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(no_secrets.secrets.is_none());
        // 未知キーは拒否。
        assert!(toml::from_str::<Config>("[secrets]\nbogus = 1\n").is_err());
    }

    /// `ensure_secrets_dir` は `[secrets] dir` を 0700 で作る（無ければ）。`[secrets]` が無ければ何もしない。
    #[test]
    fn ensure_secrets_dir_creates_the_directory_with_0700() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "[secrets]\ndir = \"secrets\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let secrets_dir = cfg.secrets.as_ref().unwrap().dir.clone();
        assert!(!secrets_dir.exists());
        cfg.ensure_secrets_dir().unwrap();
        assert!(secrets_dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&secrets_dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        // 既にあれば触らない。
        cfg.ensure_secrets_dir().unwrap();

        // [secrets] 無しは no-op。
        let no_secrets: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(no_secrets.ensure_secrets_dir().is_ok());
    }

    /// `env_from_secrets` は `[adapters.*]` と行の両方で読める（未知キーは拒否）。
    #[test]
    fn env_from_secrets_is_parsed_on_adapters_and_providers() {
        let text = r#"[adapters.local_deep_research]
env_from_secrets = { LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily", LDR_SEARCH_ENGINE_WEB_EXA_API_KEY = "exa" }

[[providers]]
id = "ldr"
adapter = "local-deep-research"
env_from_secrets = { LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily-row" }
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.validate().is_ok());
        assert_eq!(
            cfg.adapters.local_deep_research.env_from_secrets.get("LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY").map(String::as_str),
            Some("tavily")
        );
        assert_eq!(
            cfg.providers[0].env_from_secrets.get("LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY").map(String::as_str),
            Some("tavily-row")
        );

        for (section, extra) in [
            ("claude_code", ""),
            ("codex", ""),
            ("fake", ""),
            ("acp", ""),
            ("paperqa", ""),
        ] {
            let _ = extra;
            let text = format!(
                "[adapters.{section}]\nenv_from_secrets = {{ FOO = \"bar\" }}\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n"
            );
            let cfg: Config = toml::from_str(&text).unwrap_or_else(|e| panic!("{section}: {e}"));
            let _ = cfg;
        }
    }

    /// 例の設定ファイルにコメントアウトされた `[accounts]` / `account_pool` の節も構文として妥当なことを確認する
    /// （読み込み自体は動かないが `toml` として壊れていないことは grep で確認できる）。
    #[test]
    fn multi_account_example_mentions_account_pool_commented_out() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/taskd.multi-account.example.toml"
        ));
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("# [accounts]"));
        assert!(text.contains("# account_pool = true"));
        // 既存の受け入れ条件（Config::load が通る）はコメントアウトされているので変わらない。
        assert!(Config::load(path).is_ok());
    }
}
