//! taskd: デーモン本体（DESIGN §3, §5.2, ADR-0005 D7）。設定読込、ログ初期化、tick ループ。
//! 判断ロジックは `task-dispatch` にあり、ここはループと配線だけ。

mod accounts_admin;
mod cluster_admin;
pub mod config;
/// ADR-0033 D3（Phase 25）: 報告の圧縮（まとめの run を起こす決定的な判断）。
pub mod reports;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use task_api::types::{
    ApiConfigView, ClusterConfigView, ConfigView, GenreConfigView, ProviderConfigView, ReviewerConfigView,
    RoleConfigView,
};
use task_api::{ApiError, ApiSettings, ApiState};
use task_core::{SqliteStore, StoreError, StoreOptions, TaskStore};
use task_ops::view::ViewContext;
use task_dispatch::{DispatchError, Dispatcher, ProviderId, SnapshotPublisher, StaticPolicy, TickReport};
use task_ops::daemon::{ProviderCheckView, ProviderLive};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use task_worker::{
    AcpAdapter, AcpConfig, ClaudeCodeAdapter, ClaudeCodeConfig, CodexAdapter, CodexConfig, FakeAdapter, LdrAdapter,
    LdrConfig, PaperQaAdapter, PaperQaConfig, WorkerAdapter, Workspace,
};

pub use config::{Config, ConfigError};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("dispatch: {0}")]
    Dispatch(#[from] DispatchError),
    #[error("api: {0}")]
    Api(#[from] ApiError),
}

/// ループの終了条件。
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// idle な tick で exit する（テスト・バッチ用）。
    pub until_idle: bool,
    /// tick 数の上限（0 = 無制限）。
    pub max_ticks: u64,
}

/// ループ終了の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Idle,
    MaxTicks,
    Signal,
}

/// `[adapters.<種別>].env` にプロバイダの `env` を重ねる（同名キーはプロバイダが優先。順序は決定的）。
/// ADR-0030 以降、本体（`build_adapters`）は `merged_env_with_secrets` を使う。これはテストが期待値を
/// 組み立てるのに使う（`env_from_secrets` が空なら `merged_env_with_secrets` と同じ結果になる）。
#[cfg(test)]
fn merged_env(base: &HashMap<String, String>, provider: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut merged: BTreeMap<String, String> = base.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    merged.extend(provider.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged.into_iter().collect()
}

/// ADR-0030 D1: `[secrets] dir` の下の `<id>` ファイルを読み、末尾の改行を落とした値を返す。無い・読めない
/// ときは設定エラーにせず `warn!` を出して `None`（値はログに出さない。id だけ記録する。ADR-0024 D5 と同じ規律）。
fn resolve_secret(secrets_dir: Option<&Path>, id: &str) -> Option<String> {
    let dir = secrets_dir?;
    let path = dir.join(id);
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text.trim_end_matches(['\n', '\r']).to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(secret_id = %id, "secret not found; omitting env var (ADR-0030 D2)");
            None
        }
        Err(e) => {
            tracing::warn!(secret_id = %id, error = %e, "cannot read secret; omitting env var (ADR-0030 D2)");
            None
        }
    }
}

/// `mapping`（環境変数名 → 秘密 id）のキーを決定的な順で解決し、見つかったものだけ `merged` に上書きする
/// （見つからなければそのキーには**触れない**。下の層の値が残る。ADR-0030 D2）。
fn apply_env_from_secrets(merged: &mut BTreeMap<String, String>, mapping: &HashMap<String, String>, secrets_dir: Option<&Path>) {
    let mut env_keys: Vec<&String> = mapping.keys().collect();
    env_keys.sort();
    for env_key in env_keys {
        let secret_id = &mapping[env_key];
        if let Some(value) = resolve_secret(secrets_dir, secret_id) {
            merged.insert(env_key.clone(), value);
        }
    }
}

/// ADR-0030 D2: 優先順は taskd の環境（プロセス継承。ここでは扱わない）< `[adapters.*].env` <
/// `[adapters.*].env_from_secrets` < 行の `env` < 行の `env_from_secrets`。
fn merged_env_with_secrets(
    base_env: &HashMap<String, String>,
    base_env_from_secrets: &HashMap<String, String>,
    row_env: &HashMap<String, String>,
    row_env_from_secrets: &HashMap<String, String>,
    secrets_dir: Option<&Path>,
) -> Vec<(String, String)> {
    let mut merged: BTreeMap<String, String> = base_env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    apply_env_from_secrets(&mut merged, base_env_from_secrets, secrets_dir);
    merged.extend(row_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    apply_env_from_secrets(&mut merged, row_env_from_secrets, secrets_dir);
    merged.into_iter().collect()
}

/// プロバイダの `model` が空でなければそれ、空なら `[adapters.<種別>].model`（ADR-0012 D1）。
fn effective_model(provider_model: &str, adapter_model: &Option<String>) -> Option<String> {
    if provider_model.is_empty() {
        adapter_model.clone()
    } else {
        Some(provider_model.to_string())
    }
}

/// ADR-0012 D1: `[[providers]]` の各行（= 1 アカウント）ごとにアダプタのインスタンスを作る。`[adapters.<種別>]` を基本設定とし、
/// プロバイダの `env` と `model` を重ねる。キーはプロバイダ ID。
pub fn build_adapters(config: &Config) -> HashMap<ProviderId, Arc<dyn WorkerAdapter>> {
    let secrets_dir = config.secrets.as_ref().map(|s| s.dir.as_path());
    let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
    for p in &config.providers {
        let adapter: Arc<dyn WorkerAdapter> = match p.adapter.as_str() {
            ClaudeCodeAdapter::ID => {
                let base = &config.adapters.claude_code;
                Arc::new(ClaudeCodeAdapter::new(ClaudeCodeConfig {
                    command: base.command.clone(),
                    extra_args: base.extra_args.clone(),
                    permission_mode: base.permission_mode.clone(),
                    model: effective_model(&p.model, &base.model),
                    env: merged_env_with_secrets(&base.env, &base.env_from_secrets, &p.env, &p.env_from_secrets, secrets_dir),
                }))
            }
            CodexAdapter::ID => {
                let base = &config.adapters.codex;
                Arc::new(CodexAdapter::new(CodexConfig {
                    command: base.command.clone(),
                    extra_args: base.extra_args.clone(),
                    model: effective_model(&p.model, &base.model),
                    env: merged_env_with_secrets(&base.env, &base.env_from_secrets, &p.env, &p.env_from_secrets, secrets_dir),
                }))
            }
            AcpAdapter::ID => {
                let base = &config.adapters.acp;
                Arc::new(AcpAdapter::new(AcpConfig {
                    // ADR-0026 D2: `command`/`args` は行ごとに上書きできる（別の ACP エージェントを同居させる
                    // ため）。`Config::validate` が acp 以外の行での指定を拒否している。
                    command: p.command.clone().unwrap_or_else(|| base.command.clone()),
                    args: p.args.clone().unwrap_or_else(|| base.args.clone()),
                    env: merged_env_with_secrets(&base.env, &base.env_from_secrets, &p.env, &p.env_from_secrets, secrets_dir),
                    permission: base.permission,
                    // ADR-0026 D3: `[adapters.acp]` にモデルの既定値は無い（CLI の `--model` フラグではなく
                    // `session/set_config_option` で渡すので、行の `model` が空ならモデル指定なしになるだけ）。
                    model: effective_model(&p.model, &None),
                    model_option_id: base.model_option_id.clone(),
                    startup_timeout: Duration::from_secs(base.startup_timeout_secs),
                }))
            }
            PaperQaAdapter::ID => {
                let base = &config.adapters.paperqa;
                Arc::new(PaperQaAdapter::new(PaperQaConfig {
                    command: base.command.clone(),
                    // ADR-0027 D3: 行ごとに設定ファイルを上書きできる（`command`/`args` と同じ作り）。
                    settings: p.settings.clone().or_else(|| base.settings.clone()),
                    paper_directory: base.paper_directory.clone(),
                    index_directory: base.index_directory.clone(),
                    index_name: base.index_name.clone(),
                    // ADR-0027 D3: `model`（行の値。空なら None）は PaperQA の設定ファイルより優先して `--llm` に渡す。
                    // `[adapters.paperqa]` にモデルの既定値は無い（acp と同じ理由: 行＝アカウント/エンドポイントごと）。
                    model: effective_model(&p.model, &None),
                    env: merged_env_with_secrets(&base.env, &base.env_from_secrets, &p.env, &p.env_from_secrets, secrets_dir),
                    extra_args: base.extra_args.clone(),
                    // ADR-0035 D1 / D3: 取得と証拠ゲートは行ごとの上書きが無い（他の paperqa 設定と同じ扱い）。
                    acquire: base.acquire.clone(),
                    evidence: base.evidence,
                }))
            }
            LdrAdapter::ID => {
                let base = &config.adapters.local_deep_research;
                // ADR-0029 D1: 行ごとの上書きは `model`（`settings` の `llm.model` を上書き）と `env` だけ
                // （`ProviderConfig.settings` は `paperqa` 専用フィールドなので LDR では再利用しない。
                // taskd 側の実装判断。行ごとに調査対象を変えたければ `[[roles]]`/`[[genres]]` で使い分ける）。
                let mut settings: Vec<(String, String)> = base.settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                settings.sort();
                Arc::new(LdrAdapter::new(LdrConfig {
                    command: base.command.clone(),
                    mode: base.mode,
                    iterations: base.iterations,
                    questions_per_iteration: base.questions_per_iteration,
                    settings,
                    model: effective_model(&p.model, &None),
                    env: merged_env_with_secrets(&base.env, &base.env_from_secrets, &p.env, &p.env_from_secrets, secrets_dir),
                    // ADR-0031 D2: 証拠ゲートの閾値は行ごとの上書きが無い（他の LDR 設定と同じ扱い）。
                    evidence: base.evidence,
                }))
            }
            // `Config::validate` が fake / claude-code / codex / acp / paperqa / local-deep-research 以外を拒否している。
            _ => {
                let mut fake = FakeAdapter::new(config.adapters.fake.command.clone());
                fake.set_env(merged_env_with_secrets(
                    &config.adapters.fake.env,
                    &config.adapters.fake.env_from_secrets,
                    &p.env,
                    &p.env_from_secrets,
                    secrets_dir,
                ));
                Arc::new(fake)
            }
        };
        adapters.insert(p.id.clone(), adapter);
    }
    adapters
}

/// ADR-0030 D3: `GET /secrets` の `used_by` を設定から導く（秘密 id → それを使っている adapter/provider の
/// env_from_secrets の一覧）。設定順・キー順で決定的に並べる。
pub fn secret_usage(config: &Config) -> HashMap<String, Vec<task_api::types::SecretUse>> {
    fn push(
        map: &mut HashMap<String, Vec<task_api::types::SecretUse>>,
        secret_id: &str,
        scope: &str,
        name: &str,
        env: &str,
    ) {
        map.entry(secret_id.to_string()).or_default().push(task_api::types::SecretUse {
            scope: scope.to_string(),
            name: name.to_string(),
            env: env.to_string(),
        });
    }

    let mut map: HashMap<String, Vec<task_api::types::SecretUse>> = HashMap::new();
    let adapters: [(&str, &HashMap<String, String>); 6] = [
        (ClaudeCodeAdapter::ID, &config.adapters.claude_code.env_from_secrets),
        (CodexAdapter::ID, &config.adapters.codex.env_from_secrets),
        (FakeAdapter::ID, &config.adapters.fake.env_from_secrets),
        (AcpAdapter::ID, &config.adapters.acp.env_from_secrets),
        (PaperQaAdapter::ID, &config.adapters.paperqa.env_from_secrets),
        (LdrAdapter::ID, &config.adapters.local_deep_research.env_from_secrets),
    ];
    for (name, from_secrets) in adapters {
        let mut env_keys: Vec<&String> = from_secrets.keys().collect();
        env_keys.sort();
        for env_key in env_keys {
            push(&mut map, &from_secrets[env_key], "adapter", name, env_key);
        }
    }
    let mut providers: Vec<&config::ProviderConfig> = config.providers.iter().collect();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    for p in providers {
        let mut env_keys: Vec<&String> = p.env_from_secrets.keys().collect();
        env_keys.sort();
        for env_key in env_keys {
            push(&mut map, &p.env_from_secrets[env_key], "provider", &p.id, env_key);
        }
    }
    map
}

/// `WorkerStarted.model` に記録する、プロバイダごとの実効モデル名（ADR-0012 D1）。
pub fn effective_models(config: &Config) -> HashMap<ProviderId, String> {
    config
        .providers
        .iter()
        .map(|p| {
            let adapter_model = match p.adapter.as_str() {
                ClaudeCodeAdapter::ID => config.adapters.claude_code.model.clone(),
                CodexAdapter::ID => config.adapters.codex.model.clone(),
                // ADR-0026 D3 / ADR-0027 D3: acp / paperqa には `[adapters.<種別>].model` が無い。
                // 行の `model` が空なら `None` になる。
                _ => None,
            };
            (p.id.clone(), effective_model(&p.model, &adapter_model).unwrap_or_default())
        })
        .collect()
}

/// ADR-0013 D4: デーモンのスナップショットに載せる `[[providers]]` の定義（`in_use` はディスパッチャが毎 tick 埋める）。
pub fn provider_lives(config: &Config) -> Vec<ProviderLive> {
    let models = effective_models(config);
    config
        .providers
        .iter()
        .map(|p| {
            let mut env_keys: Vec<String> = p.env.keys().cloned().collect();
            env_keys.sort();
            ProviderLive {
                id: p.id.clone(),
                adapter: p.adapter.clone(),
                tiers: p.tiers.clone(),
                concurrency: p.concurrency,
                model: models.get(&p.id).filter(|m| !m.is_empty()).cloned(),
                env_keys,
                in_use: 0,
                // ADR-0022 D2: 確認の記録は Dispatcher 側（SnapshotPublisher.provider_checks）が持つ。
                last_check: None,
                account_pool: p.account_pool,
            }
        })
        .collect()
}

/// ADR-0013 D5 の前提（DB はローカルディスク）を破っている場合に警告するための、ネットワーク FS の一覧。
const NETWORK_FILESYSTEMS: &[&str] = &[
    "nfs", "nfs4", "cifs", "smb3", "9p", "afs", "ceph", "lustre", "gpfs", "beegfs", "glusterfs",
];

/// `/proc/self/mountinfo` の内容から、`target` を含む最長一致のマウント点のファイルシステム種別を返す。
fn filesystem_type_in(mountinfo: &str, target: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mount_point) = before.split_whitespace().nth(4) else {
            continue;
        };
        let Some(fstype) = after.split_whitespace().next() else {
            continue;
        };
        // 同じマウント点の行が複数ある場合（autofs → nfs4 など）は後の行が有効なので `>=` で上書きする。
        if target.starts_with(mount_point) && best.as_ref().is_none_or(|(len, _)| mount_point.len() >= *len) {
            best = Some((mount_point.len(), fstype.to_string()));
        }
    }
    best.map(|(_, fstype)| fstype)
}

/// ADR-0015 D3: DB がネットワーク FS 上なら警告する（起動は止めない。判定できない環境では何もしない）。
fn warn_if_db_on_network_filesystem(db: &Path) {
    let dir = db.parent().unwrap_or(Path::new("."));
    let Ok(target) = dir.canonicalize() else {
        return;
    };
    let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return;
    };
    let Some(fstype) = filesystem_type_in(&mountinfo, &target) else {
        return;
    };
    if NETWORK_FILESYSTEMS.contains(&fstype.as_str()) || fstype.starts_with("fuse.") {
        tracing::warn!(
            db = %db.display(),
            filesystem = %fstype,
            "the database is on a network filesystem; SQLite WAL needs a local disk (ADR-0013 D5). \
             Expect stalls, `database is locked` and possible corruption"
        );
    }
}

/// スナップショットの `hostname`: `/proc/sys/kernel/hostname`（Linux）→ `HOSTNAME` → `"unknown"`。
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// 設定から `Dispatcher` を組み立てる。`[accounts]`/`[secrets]`/`[memory]` があればディレクトリを 0700 で作る
/// （ADR-0024 D1、ADR-0030 D1、ADR-0033 D6）。
pub fn build_dispatcher(config: &Config, masters: ClusterMasters) -> Result<Dispatcher, DaemonError> {
    config.ensure_accounts_dir()?;
    config.ensure_secrets_dir()?;
    config.ensure_memory_dir()?;
    let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open(&config.db)?);
    seed_org_if_empty(store.as_ref(), config)?;
    let policy = StaticPolicy::new(
        config.provider_specs(),
        std::time::Duration::from_secs(config.error_cooldown_secs),
    );
    let mut dispatcher = Dispatcher::new(
        store,
        Box::new(policy),
        effective_models(config),
        build_adapters(config),
        config.account_pool_providers(),
        config.dispatch_config(),
    );
    dispatcher.set_cluster_connector(cluster_connector(masters));
    Ok(dispatcher)
}


/// ADR-0033 D1: 組織図の種を蒔く。**`org_nodes` が空のときだけ**書き、それ以外は何もしない
/// （以後の編集は GUI → API → DB。設定は再読込しない）。蒔いた件数を返す。
pub fn seed_org_if_empty(store: &dyn TaskStore, config: &Config) -> Result<usize, DaemonError> {
    if config.org.is_empty() {
        return Ok(0);
    }
    if !store.org_list()?.is_empty() {
        tracing::debug!("org: org_nodes is not empty; the config seed is not applied (the DB wins)");
        return Ok(0);
    }
    let now = OffsetDateTime::now_utc();
    let nodes = config.org_nodes(now);
    // 監査 D-4: 1 トランザクションで蒔く。途中の 1 件が不正でも部分的に書かれた組織が残らない
    // （残ると次回起動時は `org_list` が空でなくなり、二度と補完されない）。
    store.org_seed(&nodes)?;
    tracing::info!(count = nodes.len(), "org: seeded the organization from the config");
    Ok(nodes.len())
}

/// ADR-0032 D2: taskd が張った ssh master を保持する場所。`ClusterMaster` を落とすと接続も切れるので、
/// **接続を生かしておきたい間はここに置く**（`DELETE /clusters/{id}/connect` はここから取り除く）。
/// 人が `cluster-login.sh` で張った master はこのマップに載らない（taskd の持ち物ではないため）。
pub type ClusterMasters = Arc<std::sync::Mutex<HashMap<String, task_worker::cluster_login::ClusterMaster>>>;

/// ADR-0032 D3: `auth = "publickey"` のクラスタを、ディスパッチの直前に 1 回だけ自分で張る。
///
/// **時間の設計**: これはディスパッチループの中から同期で呼ばれる（`control_master_alive_blocking` と
/// 同じ立場）。`-O check` は 1 秒で返るが接続はもっとかかるので、長く待つと tick 全体が止まる。
/// そこで **`AUTO_CONNECT_TIMEOUT` を短く（8 秒）**切る。鍵だけの接続は実測で 1 秒未満なので
/// （ADR-0032 §1 の fern03）、これで足りる。間に合わなければその tick は cooldown に落ち、
/// 次の機会に再試行される（人を待たせるより tick を止めない方を優先する）。
fn cluster_connector(masters: ClusterMasters) -> task_dispatch::dispatcher::ClusterConnector {
    /// 自動接続に使う上限。ディスパッチループを止めないために短くしてある（上の説明）。
    const AUTO_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

    Arc::new(move |cluster_id: &str, host: &str| {
        let host = host.to_string();
        let cluster_id = cluster_id.to_string();
        // ディスパッチループは同期なので、非同期の `start_connect` を専用ランタイムで回す。
        // `Handle::current().block_on` は同じランタイムのワーカースレッドを塞いでパニックしうるため使わない。
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("could not build a runtime for the cluster connect: {e}"))?;
        let outcome = runtime.block_on(task_worker::cluster_login::start_connect(
            &["ssh".to_string()],
            &host,
            false, // publickey のみ。人の入力は要らない（要るクラスタはここに来ない）
            AUTO_CONNECT_TIMEOUT,
            AUTO_CONNECT_TIMEOUT,
        ));
        match outcome {
            Ok(task_worker::cluster_login::ClusterConnectStart::Connected(master)) => {
                // `master` を落とすと接続も切れるので、生かしておく場所へ移す（`None` は人が張った master）。
                if let Some(master) = master {
                    match masters.lock() {
                        Ok(mut held) => {
                            held.insert(cluster_id.clone(), master);
                        }
                        // 保持できないなら接続を維持できない。master はここで drop されて切れる。
                        Err(_) => return Err("the cluster master registry is poisoned".to_string()),
                    }
                }
                tracing::info!(cluster = %cluster_id, host = %host, "cluster: auto-connected (publickey)");
                Ok(())
            }
            // `interactive = false` では起こらないが、型のうえではありうる。
            Ok(task_worker::cluster_login::ClusterConnectStart::NeedsCode { session, .. }) => {
                runtime.block_on(session.cancel());
                Err("the host asked for a verification code; set auth = \"totp\" for this cluster".to_string())
            }
            Err(e) => Err(format!("{e}")),
        }
    })
}


/// ADR-0013 / `docs/gui/api.md` §3.21: `GET /api/v1/config` に出す設定の要約。env は**キー名だけ**、トークンとその場所は出さない。
pub fn config_view(config: &Config, listen: SocketAddr) -> ConfigView {
    let models = effective_models(config);
    ConfigView {
        config_path: config.source_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
        db: config.db.display().to_string(),
        workspace_root: config.workspace_root.display().to_string(),
        tick_ms: config.tick_ms,
        max_concurrency: config.max_concurrency,
        lease_grace_secs: config.lease_grace_secs,
        idle_timeout_secs: config.idle_timeout_secs,
        kill_grace_secs: config.kill_grace_secs,
        review_timeout_secs: config.review_timeout_secs,
        error_cooldown_secs: config.error_cooldown_secs,
        retry_backoff_base_secs: config.retry_backoff_base_secs,
        retry_backoff_max_secs: config.retry_backoff_max_secs,
        max_requeues: config.max_requeues,
        plan_auto_accept: config.plan.auto_accept,
        reviewer: ReviewerConfigView { adapter: config.reviewer.adapter.clone(), tier: config.reviewer.tier },
        providers: config
            .providers
            .iter()
            .map(|p| {
                let mut env_keys: Vec<String> = p.env.keys().cloned().collect();
                env_keys.sort();
                ProviderConfigView {
                    id: p.id.clone(),
                    adapter: p.adapter.clone(),
                    tiers: p.tiers.clone(),
                    concurrency: p.concurrency,
                    model: models.get(&p.id).filter(|m| !m.is_empty()).cloned(),
                    env_keys,
                    account_pool: p.account_pool,
                }
            })
            .collect(),
        clusters: config
            .clusters
            .iter()
            .map(|c| {
                let mut env_keys: Vec<String> = c.env.keys().cloned().collect();
                env_keys.sort();
                ClusterConfigView {
                    id: c.id.clone(),
                    host: c.host.clone(),
                    // ADR-0032 D1: 接続の張り方（`manual` / `publickey` / `totp`）。GUI が出し分けに使う。
                    auth: c.auth.clone(),
                    concurrency: c.concurrency,
                    sync: c.sync.clone(),
                    delete_on_push: c.delete_on_push,
                    has_setup: !c.setup.is_empty(),
                    env_keys,
                    rsync_excludes: c.rsync_excludes.clone(),
                }
            })
            .collect(),
        // ADR-0016 D1: 指示文は**本文を出さない**（有無だけ）。
        roles: config
            .roles
            .iter()
            .map(|r| RoleConfigView {
                id: r.id.clone(),
                tier: r.tier,
                adapter: r.adapter.clone(),
                max_turns: r.max_turns,
                max_wall_secs: r.max_wall_secs,
                has_instructions: r.instructions.as_ref().is_some_and(|s| !s.is_empty()),
            })
            .collect(),
        // ADR-0027 D1 / ADR-0028 D1: `[[genres]]` の要約（`role` と同じく設定順のまま）。
        genres: config
            .genres
            .iter()
            .map(|g| GenreConfigView {
                id: g.id.clone(),
                description: g.description.clone(),
                capabilities: g.capabilities.clone(),
                input_artifacts: g.input_artifacts.clone(),
                output_artifacts: g.output_artifacts.clone(),
                default_role: g.default_role.clone(),
                roles: g.roles.clone(),
            })
            .collect(),
        delegation: config.delegation_limits(),
        api: ApiConfigView {
            bind: listen.to_string(),
            auth_required: config.api.token_file.is_some(),
            allowed_hosts: config.api.allowed_hosts.clone(),
        },
    }
}

/// `task_api::ApiSettings` を設定から作る。`instance_id` / `started_at` はディスパッチャのスナップショットと同じ値を渡す。
pub fn api_settings(
    config: &Config,
    listen: SocketAddr,
    token: Option<String>,
    instance_id: String,
    started_at: String,
    admin_tx: Option<tokio::sync::mpsc::Sender<task_api::AdminRequest>>,
) -> ApiSettings {
    ApiSettings {
        listen,
        token,
        allowed_hosts: config.api.allowed_hosts.clone(),
        db_path: config.db.clone(),
        busy_timeout: StoreOptions::default().busy_timeout,
        view: ViewContext {
            workspace_root: config.workspace_root.clone(),
            retry_backoff_base: Duration::from_secs(config.retry_backoff_base_secs),
            retry_backoff_max: Duration::from_secs(config.retry_backoff_max_secs),
            max_requeues: config.max_requeues,
            clusters: config.cluster_view_infos(),
        },
        config_view: config_view(config, listen),
        roles: config.role_specs(),
        genres: config.genre_specs(),
        conversation_genre: config.conversation_genre_id().to_string(),
        taskd_version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id,
        started_at,
        providers_dir: config.providers_dir.clone(),
        admin_tx,
        accounts_roots: config.accounts.as_ref().map(|a| a.roots()).unwrap_or_default(),
        max_runs_per_account: config.accounts.as_ref().map(|a| a.max_runs_per_account).unwrap_or(0),
        secrets_dir: config.secrets.as_ref().map(|s| s.dir.clone()),
        secret_usage: secret_usage(config),
        memory_dir: config.memory.as_ref().map(|m| m.dir.clone()),
    }
}

/// 動いている API サーバ。`stop` で graceful に止める。
struct RunningApi {
    stop: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<Result<(), ApiError>>,
}

impl RunningApi {
    async fn stop(self) {
        let _ = self.stop.send(());
        match tokio::time::timeout(Duration::from_secs(5), self.handle).await {
            Ok(Ok(Ok(()))) => tracing::info!("api stopped"),
            Ok(Ok(Err(e))) => tracing::error!(error = %e, "api server failed"),
            Ok(Err(e)) => tracing::error!(error = %e, "api task panicked"),
            Err(_) => tracing::warn!("api did not stop within 5s"),
        }
    }
}

/// ADR-0013 D3 / D4: ディスパッチャにスナップショットの送り口を付け、API 専用の DB 接続を開いて bind する。
/// 開けない・bind できないときは起動を失敗させる（黙って API 無しで動かない）。
async fn start_api(
    config: &Config,
    listen: SocketAddr,
    dispatcher: &mut Dispatcher,
) -> Result<(RunningApi, tokio::sync::mpsc::Receiver<task_api::AdminRequest>), DaemonError> {
    let token = config.api.read_token()?;
    let instance_id = ulid::Ulid::new().to_string();
    let started_at = OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default();
    let (tx, rx) = tokio::sync::watch::channel(None);
    dispatcher.set_snapshot_publisher(SnapshotPublisher {
        tx,
        instance_id: instance_id.clone(),
        hostname: hostname(),
        started_at: started_at.clone(),
        tick_ms: config.tick_ms,
        providers: provider_lives(config),
        provider_checks: std::collections::HashMap::new(),
    });
    // ADR-0017 M2: `reload`/`check` は API 側では実行できない（task-worker/task-dispatch に依存しない
    // 境界を守るため）。taskd の tick ループへ委譲するチャネルを作り、送信側だけ API に渡す。
    let (admin_tx, admin_rx) = tokio::sync::mpsc::channel(8);
    let settings = api_settings(config, listen, token, instance_id, started_at, Some(admin_tx));
    let state = tokio::task::spawn_blocking(move || ApiState::new(settings, rx))
        .await
        .map_err(|e| ApiError::Startup(e.to_string()))??;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|source| ApiError::Bind { addr: listen, source })?;
    let addr = listener.local_addr().unwrap_or(listen);
    tracing::info!(%addr, auth_required = config.api.token_file.is_some(), "api listening");
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(task_api::serve_with_listener(listener, state, async move {
        let _ = stop_rx.await;
    }));
    Ok((RunningApi { stop, handle }, admin_rx))
}

/// デーモン本体。`[api]` があれば同じランタイムで HTTP API も動かし、tick ループの終了時に止める。
pub async fn run(config: Config, opts: RunOptions) -> Result<Exit, DaemonError> {
    warn_if_db_on_network_filesystem(&config.db);
    let cluster_masters: ClusterMasters = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let mut dispatcher = build_dispatcher(&config, Arc::clone(&cluster_masters))?;
    let (api, admin_rx) = match config.api.listen {
        Some(listen) => {
            let (api, admin_rx) = start_api(&config, listen, &mut dispatcher).await?;
            (Some(api), Some(admin_rx))
        }
        None => (None, None),
    };
    let result = tick_loop(&mut dispatcher, &config, opts, admin_rx, cluster_masters).await;
    if let Some(api) = api {
        api.stop().await;
    }
    result
}

/// tick ループ。SIGINT/SIGTERM で停止する。`admin_rx` があれば `POST /api/v1/reload` /
/// `POST /api/v1/providers/{id}/check`（ADR-0017 M2）も同じループで受ける。
async fn tick_loop(
    dispatcher: &mut Dispatcher,
    config: &Config,
    opts: RunOptions,
    mut admin_rx: Option<tokio::sync::mpsc::Receiver<task_api::AdminRequest>>,
    // ADR-0032 D2: taskd が張った ssh master の置き場所。ここが持っている間だけ接続が生きる。
    cluster_masters: ClusterMasters,
) -> Result<Exit, DaemonError> {
    // ADR-0022 D2: `check` は spawn した先で終わるので、結果をここへ戻してスナップショットに載せる。
    let (check_tx, mut check_rx) = tokio::sync::mpsc::channel::<(String, ProviderCheckView)>(16);
    // ADR-0024 D5〜D7: アカウントの確認・ログイン中継も同様に、spawn した先の結果をここへ戻す。
    let (account_tx, mut account_rx) = tokio::sync::mpsc::channel::<accounts_admin::AccountAdminEvent>(16);
    let login_sessions = accounts_admin::new_sessions();
    // ADR-0025 D5: codex のログイン中継（別の流儀なので別のマップ）。
    let codex_login_sessions = accounts_admin::new_codex_sessions();
    // ADR-0032 D4: クラスタ接続の中継（進行中のセッションと、taskd が保持している ssh master）。
    let (cluster_tx, mut cluster_rx) = tokio::sync::mpsc::channel::<cluster_admin::ClusterConnectPending>(16);
    let cluster_sessions: cluster_admin::ClusterConnectSessions = Default::default();
    let tick = config.tick();
    let mut ticks: u64 = 0;
    tracing::info!(db = %config.db.display(), workspace_root = %config.workspace_root.display(), max_concurrency = config.max_concurrency, tick_ms = config.tick_ms, "taskd started");

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    // ADR-0015 D2: tick の所要時間を測り、遅い tick を警告する（止まっているのがディスパッチャか API かの切り分け用）。
    let slow_tick = std::cmp::max(Duration::from_secs(1), tick * 2);
    loop {
        // ADR-0024 D7 / B1: 10 分を超えたログイン中継を打ち切る（tick をブロックしない軽い処理）。
        // `expire_stale_logins` はチャネルを使わない（このループ自身が drain するチャネルへ `await` で
        // 送るとデッドロックしうるため）。打ち切った id は戻り値で受け取り、ここで直接反映する。
        for id in accounts_admin::expire_stale_logins(&login_sessions, accounts_admin::LOGIN_EXPIRY).await {
            dispatcher.set_account_login_pending(task_core::AccountAdapter::ClaudeCode, &id, false);
        }
        // ADR-0025 D5: codex も同様に 15 分で打ち切り、完了したものはポーリングで検知する（どちらもチャネルを
        // 使わない。B1 と同じ理由）。
        for id in accounts_admin::expire_stale_codex_logins(&codex_login_sessions, accounts_admin::LOGIN_EXPIRY_CODEX).await {
            dispatcher.set_account_login_pending(task_core::AccountAdapter::Codex, &id, false);
        }
        for (id, _ok) in accounts_admin::poll_codex_logins(&codex_login_sessions).await {
            dispatcher.set_account_login_pending(task_core::AccountAdapter::Codex, &id, false);
        }
        // ADR-0032 D4 / B1: 放置されたクラスタ接続のセッションも同じ規約で畳む（チャネルを使わない）。
        for id in
            cluster_admin::expire_stale_cluster_sessions(&cluster_sessions, cluster_admin::SESSION_EXPIRY).await
        {
            dispatcher.set_cluster_connect_pending(&id, false);
        }
        // ADR-0033 D3 / B1: 報告の圧縮（まとめの run を起こすかの決定的な判断）。チャネルには送らず、
        // tick の直前にストアを見るだけ（LLM もワーカーも起動しない。起動するのは次の tick の dispatch）。
        {
            let store = dispatcher.store();
            match reports::schedule_report_compaction(
                store.as_ref(),
                &config.reports,
                &config.role_specs(),
                &config.genre_specs(),
                OffsetDateTime::now_utc(),
            ) {
                Ok(created) if !created.is_empty() => {
                    tracing::info!(count = created.len(), "reports: compaction runs scheduled");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "reports: could not schedule the compaction runs"),
            }
        }
        let tick_started = std::time::Instant::now();
        let report: TickReport = dispatcher.tick()?;
        let tick_elapsed = tick_started.elapsed();
        ticks += 1;
        if tick_elapsed >= slow_tick {
            tracing::warn!(ticks, duration_ms = tick_elapsed.as_millis() as u64, ?report, "slow tick");
        }
        if report.reclaimed + report.dispatched + report.finished + report.reviewed > 0 {
            tracing::info!(ticks, ?report, "tick");
        } else {
            tracing::debug!(ticks, ?report, "tick");
        }
        if opts.until_idle && report.idle {
            tracing::info!(ticks, "idle; exiting");
            return Ok(Exit::Idle);
        }
        if opts.max_ticks > 0 && ticks >= opts.max_ticks {
            tracing::info!(ticks, "max ticks reached; exiting");
            return Ok(Exit::MaxTicks);
        }
        let term = async {
            match sigterm.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        let admin = async {
            match admin_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending::<Option<task_api::AdminRequest>>().await,
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT; exiting");
                return Ok(Exit::Signal);
            }
            _ = term => {
                tracing::info!("SIGTERM; exiting");
                return Ok(Exit::Signal);
            }
            req = admin => {
                // ADR-0017 M2: 処理後は select に戻らず即座にループの先頭（次の `dispatcher.tick()`）へ進む
                // ので、reload の効果は「次の tick から」になる。`tick_ms` の残りを待たない。
                if let Some(req) = req {
                    handle_admin_request(dispatcher, config, req, check_tx.clone(), login_sessions.clone(), codex_login_sessions.clone(), account_tx.clone(), cluster_sessions.clone(), Arc::clone(&cluster_masters), cluster_tx.clone()).await;
                }
            }
            // ADR-0022 D2: 終わった `check` の結果を受け取り、次の tick のスナップショットに載せる。
            Some((provider_id, check)) = check_rx.recv() => {
                tracing::info!(who = "admin", provider_id = %provider_id, result = %check.result, "provider check recorded");
                dispatcher.set_provider_check(&provider_id, check);
            }
            // ADR-0024 D4〜D7 / ADR-0025 D4/D5: アカウントの確認・ログイン中継の結果を `AccountBook` /
            // `login_pending` に反映する。
            Some(event) = account_rx.recv() => {
                match event {
                    accounts_admin::AccountAdminEvent::Checked { adapter, id, result, detail, observation } => {
                        tracing::info!(who = "admin", op = "account_check", account_id = %id, %adapter, result = %result, "account check recorded");
                        dispatcher.record_account_check(adapter, &id, &result, detail, observation);
                    }
                    accounts_admin::AccountAdminEvent::LoginPending { adapter, id, pending } => {
                        dispatcher.set_account_login_pending(adapter, &id, pending);
                    }
                }
            }
            // ADR-0032 D5: クラスタ接続の進行状況を `ClusterLive.connect_pending` に反映する。
            Some(cluster_admin::ClusterConnectPending { id, pending }) = cluster_rx.recv() => {
                dispatcher.set_cluster_connect_pending(&id, pending);
            }
        }
    }
}

/// ADR-0017 M2: API から委譲された `reload`/`check` を処理する。`reload` はその場で（`Dispatcher` を直接
/// 差し替えるだけの軽い処理）、`check` は最大 30 秒かかりうるので tick をブロックしないよう `tokio::spawn` する。
#[allow(clippy::too_many_arguments)]
async fn handle_admin_request(
    dispatcher: &mut Dispatcher,
    config: &Config,
    req: task_api::AdminRequest,
    check_tx: tokio::sync::mpsc::Sender<(String, ProviderCheckView)>,
    login_sessions: accounts_admin::LoginSessions,
    codex_login_sessions: accounts_admin::CodexLoginSessions,
    account_tx: tokio::sync::mpsc::Sender<accounts_admin::AccountAdminEvent>,
    cluster_sessions: cluster_admin::ClusterConnectSessions,
    cluster_masters: ClusterMasters,
    cluster_tx: tokio::sync::mpsc::Sender<cluster_admin::ClusterConnectPending>,
) {
    match req {
        task_api::AdminRequest::Reload { reply } => {
            let result = reload_providers(dispatcher, config);
            if result.is_ok() {
                tracing::info!(who = "admin", "providers reloaded");
            } else {
                tracing::warn!(who = "admin", ?result, "reload rejected");
            }
            let _ = reply.send(result);
        }
        task_api::AdminRequest::Check { provider_id, reply } => {
            let config_path = config.source_path.clone();
            tokio::spawn(async move {
                let outcome = check_provider(config_path, provider_id.clone()).await;
                // ADR-0022 D2: 確認できたときだけ記録する（設定エラー・taskd 側の都合は「確認の結果」ではない）。
                if let Ok(outcome) = &outcome {
                    let check = ProviderCheckView {
                        at: OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default(),
                        result: provider_check_result_name(&outcome.result).to_string(),
                        detail: outcome.detail.clone(),
                    };
                    let _ = check_tx.send((provider_id, check)).await;
                }
                let _ = reply.send(outcome);
            });
        }
        task_api::AdminRequest::AccountCheck { adapter, id, reply } => {
            accounts_admin::spawn_check(config, adapter, id, account_tx, reply);
        }
        task_api::AdminRequest::AccountLoginStart { adapter, id, reply } => {
            accounts_admin::spawn_login_start(config, login_sessions, codex_login_sessions, adapter, id, account_tx, reply);
        }
        task_api::AdminRequest::AccountLoginCode { id, code, reply } => {
            accounts_admin::spawn_login_code(login_sessions, id, code, account_tx, reply);
        }
        task_api::AdminRequest::AccountLoginCancel { adapter, id, reply } => {
            accounts_admin::spawn_login_cancel(login_sessions, codex_login_sessions, adapter, id, account_tx, reply);
        }
        // ADR-0032 D5: クラスタ接続の中継。どれも `tokio::spawn` するので tick を止めない。
        task_api::AdminRequest::ClusterConnectStart { id, reply } => {
            cluster_admin::spawn_connect_start(config, cluster_sessions, cluster_masters, id, cluster_tx, reply);
        }
        task_api::AdminRequest::ClusterConnectCode { id, code, reply } => {
            cluster_admin::spawn_connect_code(
                config,
                cluster_sessions,
                cluster_masters,
                id,
                code,
                cluster_tx,
                reply,
            );
        }
        task_api::AdminRequest::ClusterConnectCancel { id, reply } => {
            cluster_admin::spawn_connect_cancel(config, cluster_sessions, cluster_masters, id, cluster_tx, reply);
        }
        // S2+S8: cheap な fs 操作（ディレクトリの rename）だけなので spawn せず、ここで直接（同期的に）行う。
        // ディスパッチャの権威ある `account_in_use` を使うため `&mut Dispatcher` が要る。
        task_api::AdminRequest::AccountRemove { adapter, id, reply } => {
            let result = accounts_admin::remove_account(config, dispatcher, &login_sessions, &codex_login_sessions, adapter, &id).await;
            if result.is_ok() {
                tracing::info!(who = "admin", op = "account_remove", account_id = %id, %adapter, "admin: account removed");
            } else {
                tracing::warn!(who = "admin", op = "account_remove", account_id = %id, %adapter, ?result, "account remove rejected");
            }
            let _ = reply.send(result);
        }
    }
}

/// `Config::load` を読み直し、稼働中のプロバイダ選定・アダプタ一式・次 tick のスナップショット提供元を差し替える。
/// 失敗したら稼働中の状態には触れない（古い設定のまま動き続ける）。
///
/// S7: `[accounts]` は reload の対象外（`Dispatcher::accounts` はプロセス起動時に固定され、`AccountBook` の
/// 保存先もそこから決まる）。`claude_dir` / `max_runs_per_account` / `check_model` のどれかが変わっていたら、
/// 反映されない値のまま動き続けるより、エラーにしてタスクを止めずに知らせる（400。再起動が必要と伝える）。
fn reload_providers(dispatcher: &mut Dispatcher, config: &Config) -> Result<(), String> {
    let path = config
        .source_path
        .clone()
        .ok_or_else(|| "config was not loaded from a file; cannot reload".to_string())?;
    let new_config = Config::load(&path).map_err(|e| e.to_string())?;
    if accounts_section_changed(&config.accounts, &new_config.accounts) {
        return Err(
            "[accounts] changed (claude_dir / max_runs_per_account / check_model); \
             this section is not reloaded, restart taskd to apply the change"
                .to_string(),
        );
    }
    let policy = StaticPolicy::new(
        new_config.provider_specs(),
        Duration::from_secs(new_config.error_cooldown_secs),
    );
    let adapters = build_adapters(&new_config);
    let models = effective_models(&new_config);
    dispatcher.reload_providers(Box::new(policy), models, adapters, new_config.account_pool_providers());
    dispatcher.set_snapshot_providers(provider_lives(&new_config));
    Ok(())
}

/// S7: `claude_dir` / `max_runs_per_account` / `check_model` のどれかが変わっていれば `true`
/// （`None` ⇔ `Some` の変化も含む）。
fn accounts_section_changed(old: &Option<config::AccountsConfig>, new: &Option<config::AccountsConfig>) -> bool {
    match (old, new) {
        (None, None) => false,
        (Some(o), Some(n)) => {
            o.claude_dir != n.claude_dir
                || o.codex_dir != n.codex_dir
                || o.max_runs_per_account != n.max_runs_per_account
                || o.check_model != n.check_model
        }
        _ => true,
    }
}

/// ADR-0022 D2: `ProviderCheckResult` の serde 名（`GET /providers` の `last_check.result` に出る文字列）。
fn provider_check_result_name(result: &task_api::ProviderCheckResult) -> &'static str {
    match result {
        task_api::ProviderCheckResult::Ok => "ok",
        task_api::ProviderCheckResult::AuthFailed => "auth_failed",
        task_api::ProviderCheckResult::Throttled => "throttled",
        task_api::ProviderCheckResult::SpawnFailed => "spawn_failed",
    }
}

/// ADR-0017 D2: 1 アカウントだけ短い疎通確認を行う。`Dispatcher`/DB には触れない（タスク・イベントに残さない）。
/// 設定は毎回 `Config::load` で読み直すので、`reload` 前の `providers.d/` の新規ファイルも確認できる。
async fn check_provider(
    config_path: Option<PathBuf>,
    provider_id: String,
) -> Result<task_api::ProviderCheckOutcome, task_api::CheckError> {
    let path = config_path
        .ok_or_else(|| task_api::CheckError::Unavailable("config was not loaded from a file; cannot check".into()))?;
    let config = Config::load(&path).map_err(|e| task_api::CheckError::ConfigInvalid(e.to_string()))?;
    if !config.providers.iter().any(|p| p.id == provider_id) {
        return Err(task_api::CheckError::NotFound);
    }
    let adapters = build_adapters(&config);
    let adapter = adapters.get(&provider_id).ok_or(task_api::CheckError::NotFound)?.clone();

    let dir = std::env::temp_dir().join(format!("taskd-provider-check-{}", ulid::Ulid::new()));
    let now = OffsetDateTime::now_utc();
    let task = task_core::Task {
        id: task_core::TaskId::new(),
        parent_id: None,
        kind: task_core::TaskKind::Execute,
        title: "provider check".into(),
        objective: "Reply with a short confirmation that you are ready. Do not change any files.".into(),
        acceptance: vec![task_core::Criterion { text: "reply".into(), check: task_core::Check::Human }],
        inputs: vec![],
        depends_on: vec![],
        status: task_core::Status::Ready,
        priority: 0,
        worker_hint: task_core::WorkerHint { tier: task_core::Tier::Standard, adapter: None },
        workspace: task_core::WorkspaceSpec::Local { path: dir.clone() },
        // ADR-0022 M1: 1 ターンではワーカープロトコル（`artifacts/result.json` を書く）を完了できず、
        // 健全なアカウントでも `error_max_turns` になる。人が読む信号にするため少しだけ余裕を持たせる。
        budget: task_core::Budget { max_turns: 3, max_wall_secs: 30, max_retries: 0 },
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
        conversation: None,
    };
    let prepared = task_worker::LocalWorkspace::new(dir.clone())
        .prepare(&task)
        .await
        .map_err(|e| task_api::CheckError::Unavailable(format!("failed to prepare check workspace: {e}")))?;
    let run_id = task_core::TaskId::new().to_string();
    // ADR-0036 D1: 疎通確認用の単独タスク（親なし）なので従来どおり `<workspace>/artifacts`。
    let artifacts_dir = task_core::artifacts::artifacts_dir_for(&task, &prepared);
    let req = task_worker::RunRequest {
        protocol: task_worker::PROTOCOL_VERSION,
        task,
        workspace: prepared,
        artifacts_dir,
        context: task_worker::RunContext {
            prior_review: vec![],
            inputs: vec![],
            answers: vec![],
            review: None,
            role: None,
            children: vec![],
            available_genres: vec![],
            // プロバイダの疎通確認なので、役職・記憶・やり取り・組織図は渡さない（ADR-0033 D4 / D6）。
            ..task_worker::RunContext::default()
        },
    };
    let limits = task_worker::RunLimits {
        wall_clock: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(config.idle_timeout_secs.min(30)),
        kill_grace: Duration::from_secs(config.kill_grace_secs),
    };
    let result = adapter.run(req, &run_id, limits, &task_worker::adapter::NullSink).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    // ADR-0022 M1（実機確認で修正）: 見ているのは「このアカウントで CLI が起動して応答するか」だけ。
    // ワーカープロトコル上のエラー（`Terminal::Error`。1 ターンでは result.json を書けない等）は
    // **アカウントの問題ではない**ので `ok` とし、理由を `detail` に残す。起動できない・認証切れ・
    // 枯渇は `AdapterError` 側で分かる。
    Ok(match result {
        Ok(outcome) => match outcome.terminal {
            task_worker::Terminal::Done { summary, .. } => (task_api::ProviderCheckResult::Ok, Some(summary)),
            task_worker::Terminal::Question { text } => (task_api::ProviderCheckResult::Ok, Some(text)),
            task_worker::Terminal::Error { message, .. } => (task_api::ProviderCheckResult::Ok, Some(message)),
        },
        Err(e @ task_worker::AdapterError::AuthFailed(_)) => (task_api::ProviderCheckResult::AuthFailed, Some(e.to_string())),
        Err(e @ (task_worker::AdapterError::Throttled { .. } | task_worker::AdapterError::Exhausted(_))) => {
            (task_api::ProviderCheckResult::Throttled, Some(e.to_string()))
        }
        Err(e) => (task_api::ProviderCheckResult::SpawnFailed, Some(e.to_string())),
    })
    .map(|(result, detail)| task_api::ProviderCheckOutcome { result, detail: detail.map(|d| truncate_detail(&d)) })
}

/// `detail` は人が読む手がかりなので短くする（1 行・200 文字まで）。
fn truncate_detail(text: &str) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 200 {
        return one_line;
    }
    one_line.chars().take(199).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ADR-0033 D1（Phase 23）: 組織図の種蒔き ----

    /// 空の DB には例の組織図（11 ノード）が入り、2 回目は何もしない（以後は DB が正）。
    #[test]
    fn seeds_the_org_once_into_an_empty_db_and_never_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/org.example.toml"),
            dir.path().join("org.toml"),
        )
        .unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            format!(
                "db = \"taskd.sqlite3\"\nworkspace_root = \"ws\"\norg_include = \"org.toml\"\n{}",
                r#"
[[providers]]
id = "x"
adapter = "fake"

[[roles]]
id = "implementer"

[[roles]]
id = "literature-reader"

[[roles]]
id = "secretary"

[[genres]]
id = "secretary"
description = "人と話す"
default_role = "secretary"
roles = ["secretary"]

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

[[roles]]
id = "web-researcher"

[[genres]]
id = "web-research"
description = "一般 Web の調査"
default_role = "web-researcher"
roles = ["web-researcher"]
"#
            ),
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        let store = SqliteStore::open(&config.db).unwrap();

        assert_eq!(seed_org_if_empty(&store, &config).unwrap(), 11);
        let nodes = store.org_list().unwrap();
        assert_eq!(nodes.len(), 11);
        let secretary = nodes.iter().find(|n| n.id == "secretary").unwrap();
        assert_eq!(secretary.kind, task_core::OrgKind::Secretary);
        assert_eq!(secretary.parent_id, None);
        assert_eq!(nodes.iter().find(|n| n.id == "coding-poc").unwrap().genre.as_deref(), Some("coding"));

        // 人が GUI で名前を変えても、次の起動で設定に戻されない。
        let mut renamed = secretary.clone();
        renamed.name = "本人".into();
        store.org_upsert(&renamed).unwrap();
        assert_eq!(seed_org_if_empty(&store, &config).unwrap(), 0);
        assert_eq!(store.org_get("secretary").unwrap().unwrap().name, "本人");
        assert_eq!(store.org_list().unwrap().len(), 11);
    }

    /// 監査 D-4: `org_include` の並びに木としての不整合（種類の順序。`Config::load` は循環・順序までは
    /// 見ない。org.rs のコメント参照）があれば、`seed_org_if_empty` は 1 件も書かずにエラーを返す
    /// （部分的に蒔かれた組織が残ると、次回起動時は `org_list` が空でなくなり二度と補完されない）。
    #[test]
    fn seed_org_if_empty_writes_nothing_when_one_node_breaks_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("org.toml"),
            r#"
[[org]]
id = "secretary"
name = "秘書"
kind = "secretary"

[[org]]
id = "research"
name = "研究部"
kind = "department"
parent_id = "secretary"

[[org]]
id = "research-survey"
name = "調査課"
kind = "section"
parent_id = "research"

[[org]]
id = "research-survey-sub"
name = "壊れた子"
kind = "section"
parent_id = "research-survey"
"#,
        )
        .unwrap();
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "db = \"taskd.sqlite3\"\nworkspace_root = \"ws\"\norg_include = \"org.toml\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        let store = SqliteStore::open(&config.db).unwrap();

        let err = seed_org_if_empty(&store, &config).unwrap_err();
        assert!(err.to_string().contains("placed under"), "{err}");
        assert!(store.org_list().unwrap().is_empty(), "nothing is written on failure");
    }

    /// `org_include` が無い設定では何も蒔かない。
    #[test]
    fn without_org_include_nothing_is_seeded() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("taskd.sqlite3");
        let config: Config = toml::from_str(&format!(
            "db = \"{}\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
            db.display()
        ))
        .unwrap();
        let store = SqliteStore::open(&config.db).unwrap();
        assert_eq!(seed_org_if_empty(&store, &config).unwrap(), 0);
        assert!(store.org_list().unwrap().is_empty());
    }

    /// ADR-0012 D1: 同じ claude-code を使う 2 アカウントが、それぞれの env と model を持つアダプタになる。
    #[test]
    fn build_adapters_creates_one_adapter_per_provider_with_merged_env_and_model() {
        let text = r#"
[adapters.claude_code]
model = "adapter-default-model"
env = { SHARED = "base", CLAUDE_CONFIG_DIR = "/base" }

[[providers]]
id = "acct-a"
adapter = "claude-code"
model = "model-a"
env = { CLAUDE_CONFIG_DIR = "/accounts/a" }

[[providers]]
id = "acct-b"
adapter = "claude-code"
env = { CLAUDE_CONFIG_DIR = "/accounts/b" }

[[providers]]
id = "local-fake"
adapter = "fake"
model = "fake"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 3);
        assert_eq!(adapters["acct-a"].id(), "claude-code");
        assert_eq!(adapters["local-fake"].id(), "fake");

        let a = merged_env(&cfg.adapters.claude_code.env, &cfg.providers[0].env);
        assert_eq!(a, vec![("CLAUDE_CONFIG_DIR".into(), "/accounts/a".into()), ("SHARED".into(), "base".into())]);
        let b = merged_env(&cfg.adapters.claude_code.env, &cfg.providers[1].env);
        assert!(b.contains(&("CLAUDE_CONFIG_DIR".into(), "/accounts/b".into())));

        let models = effective_models(&cfg);
        assert_eq!(models["acct-a"], "model-a");
        assert_eq!(models["acct-b"], "adapter-default-model");
        assert_eq!(models["local-fake"], "fake");

        // ADR-0013 D4: スナップショットの定義部分は設定の順・実効モデル（env は載せない）。
        let lives = provider_lives(&cfg);
        let ids: Vec<&str> = lives.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["acct-a", "acct-b", "local-fake"]);
        assert_eq!(lives[1].model.as_deref(), Some("adapter-default-model"));
        assert_eq!((lives[0].adapter.as_str(), lives[0].concurrency, lives[0].in_use), ("claude-code", 1, 0));
        assert!(!hostname().is_empty());

        // ADR-0015 D3: マウント点の最長一致でファイルシステム種別を引く。
        let mountinfo = "\
25 30 0:24 / / rw,relatime shared:1 - ext4 /dev/mapper/root rw
26 25 0:52 / /home rw,relatime shared:2 - nfs4 server:/home rw,vers=4.2
27 26 0:53 / /home/u/local rw,relatime shared:3 - ext4 /dev/sdb1 rw";
        assert_eq!(filesystem_type_in(mountinfo, Path::new("/var/lib/taskd")).as_deref(), Some("ext4"));
        assert_eq!(filesystem_type_in(mountinfo, Path::new("/home/u/workspace")).as_deref(), Some("nfs4"));
        // 同じマウント点に autofs と実体が並ぶ場合は後の行（実体）を採る。
        let autofs_first = "\
25 30 0:24 / / rw,relatime shared:1 - ext4 /dev/mapper/root rw
26 25 0:51 / /home rw,relatime shared:2 - autofs systemd-1 rw
27 25 0:52 / /home rw,relatime shared:3 - nfs4 server:/home rw,vers=4.2";
        assert_eq!(filesystem_type_in(autofs_first, Path::new("/home/u/x")).as_deref(), Some("nfs4"));
        assert_eq!(filesystem_type_in(mountinfo, Path::new("/home/u/local/db")).as_deref(), Some("ext4"));
        assert_eq!(filesystem_type_in("garbage", Path::new("/home")), None);

        // ADR-0013 D11: /config の要約には env のキー名だけが載り、値は載らない。
        let view = config_view(&cfg, "127.0.0.1:7710".parse().unwrap());
        assert_eq!(view.providers[0].env_keys, ["CLAUDE_CONFIG_DIR"]);
        assert_eq!(view.providers[1].model.as_deref(), Some("adapter-default-model"));
        assert_eq!((view.api.bind.as_str(), view.api.auth_required), ("127.0.0.1:7710", false));
        let json = serde_json::to_string(&view).unwrap();
        for secret in ["/accounts/a", "/accounts/b", "/base", "base\""] {
            assert!(!json.contains(secret), "{secret} leaked: {json}");
        }
    }

    /// ADR-0027 D1 / ADR-0028 D1: `[[genres]]` は `config_view` の `genres[]` に設定順のまま写る。
    /// `capabilities` / `input_artifacts` / `output_artifacts` を書かない分野は空のまま（既存設定との互換）。
    #[test]
    fn config_view_exposes_genres() {
        let text = r#"
[[roles]]
id = "lead"

[[roles]]
id = "implementer"

[[genres]]
id = "coding"
description = "write and fix code"
default_role = "implementer"
roles = ["lead", "implementer"]

[[providers]]
id = "local-fake"
adapter = "fake"
model = "fake"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let view = config_view(&cfg, "127.0.0.1:7710".parse().unwrap());
        assert_eq!(
            view.genres,
            vec![GenreConfigView {
                id: "coding".into(),
                description: "write and fix code".into(),
                capabilities: vec![],
                input_artifacts: vec![],
                output_artifacts: vec![],
                default_role: Some("implementer".into()),
                roles: vec!["lead".into(), "implementer".into()],
            }]
        );
    }

    /// ADR-0028 D1: `capabilities` / `input_artifacts` / `output_artifacts` を書けば `GET /config` の
    /// `genres[]` にそのまま出る。
    #[test]
    fn config_view_exposes_genre_capabilities_and_artifacts() {
        let text = r#"
[[roles]]
id = "literature-reader"

[[genres]]
id = "literature"
description = "related work survey"
capabilities = ["academic literature search", "citation graph traversal"]
input_artifacts = ["question", "pdf"]
output_artifacts = ["answer.md", "citations.json"]
default_role = "literature-reader"
roles = ["literature-reader"]

[[providers]]
id = "local-fake"
adapter = "fake"
model = "fake"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let view = config_view(&cfg, "127.0.0.1:7710".parse().unwrap());
        assert_eq!(view.genres[0].capabilities, vec!["academic literature search".to_string(), "citation graph traversal".to_string()]);
        assert_eq!(view.genres[0].input_artifacts, vec!["question".to_string(), "pdf".to_string()]);
        assert_eq!(view.genres[0].output_artifacts, vec!["answer.md".to_string(), "citations.json".to_string()]);
        let json = serde_json::to_value(&view.genres[0]).unwrap();
        assert_eq!(json["capabilities"][0], "academic literature search");
    }

    /// ADR-0026 D2/D3: `acp` プロバイダの行は `[adapters.acp]` の env に重ね、`command`/`args` は行の値が
    /// 優先し、`model` は行の値がそのまま（`[adapters.acp]` にモデルの既定値は無い）。
    #[test]
    fn build_adapters_wires_an_acp_provider_with_merged_env_and_row_model() {
        let text = r#"
[adapters.acp]
env = { SHARED = "base", OPENCODE_DISABLE_PROJECT_CONFIG = "1" }
permission = "deny"
model_option_id = "model"
startup_timeout_secs = 120

[[providers]]
id = "opencode-qwen"
adapter = "acp"
tiers = ["standard"]
model = "qwen-local/qwen3.8-27b"
env = { OPENCODE_CONFIG = "/x/qwen.json" }

[[providers]]
id = "opencode-default"
adapter = "acp"
tiers = ["standard"]
command = "goose"
args = ["acp"]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 2);
        assert_eq!(adapters["opencode-qwen"].id(), "acp");
        assert_eq!(adapters["opencode-default"].id(), "acp");

        let models = effective_models(&cfg);
        assert_eq!(models["opencode-qwen"], "qwen-local/qwen3.8-27b");
        // 行に model が無ければ空文字（`[adapters.acp]` にモデルの既定値が無いため、他のアダプタのような
        // フォールバックは起きない）。
        assert_eq!(models["opencode-default"], "");

        let merged = merged_env(&cfg.adapters.acp.env, &cfg.providers[0].env);
        assert_eq!(
            merged,
            vec![
                ("OPENCODE_CONFIG".to_string(), "/x/qwen.json".to_string()),
                ("OPENCODE_DISABLE_PROJECT_CONFIG".to_string(), "1".to_string()),
                ("SHARED".to_string(), "base".to_string()),
            ]
        );

        // 行の command/args が [adapters.acp] の既定（opencode/["acp"]）を上書きする。
        assert_eq!(cfg.providers[1].command.as_deref(), Some("goose"));
        assert_eq!(cfg.providers[1].args.as_deref(), Some(&["acp".to_string()][..]));
    }

    /// ADR-0027 D3: `paperqa` プロバイダの行は `[adapters.paperqa]` の env に重ね、`settings` は行の値が
    /// あればそちらを使い、`model` は行の値がそのまま（`[adapters.paperqa]` にモデルの既定値は無い）。
    #[test]
    fn build_adapters_wires_a_paperqa_provider_with_merged_env_and_row_overrides() {
        let text = r#"
[adapters.paperqa]
command = "/opt/paperqa/.venv/bin/pqa"
settings = "/opt/paperqa/settings/base"
paper_directory = "/opt/paperqa/papers"
index_directory = "/opt/paperqa/index"
env = { SHARED = "base", OPENAI_BASE_URL = "http://old:1/v1" }

[[providers]]
id = "paperqa-qwen"
adapter = "paperqa"
tiers = ["standard"]
model = "openai/qwen3.8-27b"
settings = "/opt/paperqa/settings/qwen-local"
env = { OPENAI_BASE_URL = "http://127.0.0.1:18000/v1" }

[[providers]]
id = "paperqa-default"
adapter = "paperqa"
tiers = ["standard"]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 2);
        assert_eq!(adapters["paperqa-qwen"].id(), "paperqa");
        assert_eq!(adapters["paperqa-default"].id(), "paperqa");

        let models = effective_models(&cfg);
        assert_eq!(models["paperqa-qwen"], "openai/qwen3.8-27b");
        // 行に model が無ければ空文字（`[adapters.paperqa]` にモデルの既定値が無いため、他のアダプタのような
        // フォールバックは起きない。ADR-0026 D3 と同じ理由）。
        assert_eq!(models["paperqa-default"], "");

        let merged = merged_env(&cfg.adapters.paperqa.env, &cfg.providers[0].env);
        assert_eq!(
            merged,
            vec![
                ("OPENAI_BASE_URL".to_string(), "http://127.0.0.1:18000/v1".to_string()),
                ("SHARED".to_string(), "base".to_string()),
            ]
        );

        // 行の settings が [adapters.paperqa] の既定を上書きする。上書きしない行は共通設定のまま。
        assert_eq!(cfg.providers[0].settings.as_deref(), Some("/opt/paperqa/settings/qwen-local"));
        assert!(cfg.providers[1].settings.is_none());
        assert_eq!(cfg.adapters.paperqa.settings.as_deref(), Some("/opt/paperqa/settings/base"));
    }

    /// ADR-0029 D1: `local-deep-research` プロバイダの行は `[adapters.local_deep_research]` の env に重ね、
    /// `model` は行の値がそのまま（`[adapters.local_deep_research]` にモデルの既定値は無い。`acp`/`paperqa`
    /// と同じ理由）。行ごとの `settings` の上書きは無い（taskd 側の実装判断。`ProviderConfig.settings` は
    /// `paperqa` 専用のまま）。
    #[test]
    fn build_adapters_wires_a_local_deep_research_provider_with_merged_env() {
        let text = r#"
[adapters.local_deep_research]
command = "/opt/ldr/.venv/bin/python"
mode = "detailed"
iterations = 3
env = { SHARED = "base", OPENAI_BASE_URL = "http://old:1/v1" }

[adapters.local_deep_research.settings]
"llm.provider" = "openai_endpoint"
"search.tool" = "wikipedia"

[[providers]]
id = "ldr-qwen"
adapter = "local-deep-research"
tiers = ["standard"]
model = "qwen3.8-27b"
env = { OPENAI_BASE_URL = "http://127.0.0.1:18000/v1" }

[[providers]]
id = "ldr-default"
adapter = "local-deep-research"
tiers = ["standard"]
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 2);
        assert_eq!(adapters["ldr-qwen"].id(), "local-deep-research");
        assert_eq!(adapters["ldr-default"].id(), "local-deep-research");

        let models = effective_models(&cfg);
        assert_eq!(models["ldr-qwen"], "qwen3.8-27b");
        // 行に model が無ければ空文字（`[adapters.local_deep_research]` にモデルの既定値が無い）。
        assert_eq!(models["ldr-default"], "");

        let merged = merged_env(&cfg.adapters.local_deep_research.env, &cfg.providers[0].env);
        assert_eq!(
            merged,
            vec![
                ("OPENAI_BASE_URL".to_string(), "http://127.0.0.1:18000/v1".to_string()),
                ("SHARED".to_string(), "base".to_string()),
            ]
        );

        assert_eq!(
            cfg.adapters.local_deep_research.settings.get("llm.provider").map(String::as_str),
            Some("openai_endpoint")
        );
        assert_eq!(cfg.adapters.local_deep_research.mode, task_worker::LdrMode::Detailed);
        assert_eq!(cfg.adapters.local_deep_research.iterations, Some(3));
    }

    // ---- ADR-0030 D2: `env_from_secrets` の優先順と欠落時の扱い ----

    /// 優先順は `[adapters.*].env` < `[adapters.*].env_from_secrets` < 行の `env` < 行の `env_from_secrets`
    /// （taskd 自身の環境はプロセス継承なのでここでは扱わない）。秘密が見つからない層はそのキーに触れず、
    /// 下の層の値が残る。
    #[test]
    fn merged_env_with_secrets_follows_the_precedence_order_and_falls_back_when_a_secret_is_missing() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let secrets_dir = dir.path().join("secrets");
        std::fs::create_dir_all(&secrets_dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(secrets_dir.join("id-base"), "base-secret\n").unwrap_or_else(|e| panic!("write: {e}"));
        std::fs::write(secrets_dir.join("id-row"), "row-secret").unwrap_or_else(|e| panic!("write: {e}"));
        // `id-row-missing` はわざと作らない（欠落を再現する）。

        let base_env = HashMap::from([("K".to_string(), "base-env".to_string()), ("ONLY_BASE".to_string(), "b".to_string())]);
        let base_secrets = HashMap::from([("K".to_string(), "id-base".to_string())]);
        let row_env = HashMap::from([("K".to_string(), "row-env".to_string())]);
        let row_secrets = HashMap::from([("K".to_string(), "id-row".to_string())]);

        // 全層が揃っていれば行の env_from_secrets が勝つ。
        let merged = merged_env_with_secrets(&base_env, &base_secrets, &row_env, &row_secrets, Some(&secrets_dir));
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"row-secret".to_string()));
        assert_eq!(map.get("ONLY_BASE"), Some(&"b".to_string()));

        // 行の env_from_secrets の秘密が無ければ、そのキーには触れず 1 段下（行の env）が残る。
        let row_secrets_missing = HashMap::from([("K".to_string(), "id-row-missing".to_string())]);
        let merged =
            merged_env_with_secrets(&base_env, &base_secrets, &row_env, &row_secrets_missing, Some(&secrets_dir));
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"row-env".to_string()));

        // 行の env も無ければ、その下（`[adapters.*].env_from_secrets`）が残る。
        let empty: HashMap<String, String> = HashMap::new();
        let merged = merged_env_with_secrets(&base_env, &base_secrets, &empty, &row_secrets_missing, Some(&secrets_dir));
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"base-secret".to_string()));

        // `[secrets]` 自体が未設定（`secrets_dir: None`）なら env_from_secrets は何も足さない（設定エラーにしない）。
        let merged = merged_env_with_secrets(&base_env, &base_secrets, &row_env, &row_secrets, None);
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"row-env".to_string()), "missing [secrets] falls back to the env layer, not an error");

        // 末尾の改行は読み取り時に落ちる。
        let base_secrets_only = HashMap::from([("K".to_string(), "id-base".to_string())]);
        let merged = merged_env_with_secrets(&empty, &base_secrets_only, &empty, &empty, Some(&secrets_dir));
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"base-secret".to_string()));
    }

    /// `build_adapters` は秘密が無くても設定エラーにせず、そのプロバイダのアダプタを組み立てる（run 自体は
    /// ワーカーの認証エラーで失敗する。ADR-0030 D2）。
    #[test]
    fn build_adapters_does_not_fail_when_a_referenced_secret_is_missing() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let secrets_dir = dir.path().join("secrets");
        std::fs::create_dir_all(&secrets_dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        // `tavily` の秘密ファイルは書かない。

        let text = format!(
            r#"[secrets]
dir = {secrets_dir:?}

[adapters.local_deep_research]
env_from_secrets = {{ LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily" }}

[[providers]]
id = "ldr"
adapter = "local-deep-research"
"#
        );
        let cfg: Config = toml::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        cfg.validate().unwrap_or_else(|e| panic!("{e}"));
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters["ldr"].id(), "local-deep-research");
    }

    /// `Config::load` は `[secrets] dir` を相対パスのまま toml から読むので、絶対化した設定を経由するには
    /// `Config::load` を使う（`toml::from_str` だけのテストでは相対のまま）。
    #[test]
    fn secret_usage_maps_adapter_and_provider_env_from_secrets_to_secret_ids() {
        let text = r#"[secrets]
dir = "secrets"

[adapters.local_deep_research]
env_from_secrets = { LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily", LDR_SEARCH_ENGINE_WEB_EXA_API_KEY = "exa" }

[[providers]]
id = "ldr-tavily"
adapter = "local-deep-research"
env_from_secrets = { LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily" }

[[providers]]
id = "ldr-exa"
adapter = "local-deep-research"
env_from_secrets = { LDR_SEARCH_ENGINE_WEB_EXA_API_KEY = "exa" }
"#;
        let cfg: Config = toml::from_str(text).unwrap_or_else(|e| panic!("{e}"));
        cfg.validate().unwrap_or_else(|e| panic!("{e}"));
        let usage = secret_usage(&cfg);
        assert_eq!(usage.len(), 2);
        let tavily = usage.get("tavily").expect("tavily uses");
        assert_eq!(tavily.len(), 2);
        assert!(tavily.iter().any(|u| u.scope == "adapter" && u.name == "local-deep-research" && u.env == "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"));
        assert!(tavily.iter().any(|u| u.scope == "provider" && u.name == "ldr-tavily" && u.env == "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"));
        let exa = usage.get("exa").expect("exa uses");
        assert_eq!(exa.len(), 2);
        assert!(exa.iter().any(|u| u.scope == "adapter" && u.name == "local-deep-research"));
        assert!(exa.iter().any(|u| u.scope == "provider" && u.name == "ldr-exa"));

        // 未参照の id は現れない。
        assert!(!usage.contains_key("unused"));

        // `env_from_secrets` を書かない設定は空のまま。
        let plain: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap_or_else(|e| panic!("{e}"));
        assert!(secret_usage(&plain).is_empty());
    }

    /// `api_settings` は `[secrets] dir` と `secret_usage` を `ApiSettings` に写す。
    #[test]
    fn api_settings_carries_secrets_dir_and_usage() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let path = dir.path().join("taskd.toml");
        std::fs::write(
            &path,
            "[secrets]\ndir = \"secrets\"\n\n[adapters.local_deep_research]\nenv_from_secrets = { TAVILY = \"tavily\" }\n\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let cfg = Config::load(&path).unwrap_or_else(|e| panic!("{e}"));
        let settings = api_settings(&cfg, "127.0.0.1:7710".parse().unwrap_or_else(|e| panic!("{e}")), None, "i".into(), "t".into(), None);
        assert_eq!(settings.secrets_dir, cfg.secrets.as_ref().map(|s| s.dir.clone()));
        assert!(settings.secret_usage.contains_key("tavily"));
    }

    /// S7: `[accounts]` は reload の対象外。`claude_dir` / `max_runs_per_account` / `check_model` のどれかが
    /// 変わっていたら `reload` はエラー（400 に写る文字列）を返し、稼働中の状態には触れない。
    #[test]
    fn reload_providers_rejects_changes_to_the_accounts_section() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let accounts_dir = dir.path().join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap_or_else(|e| panic!("{e}"));
        let config_path = dir.path().join("taskd.toml");
        let db = dir.path().join("taskd.db");
        let ws = dir.path().join("ws");
        let write_config = |max_runs: u32| {
            std::fs::write(
                &config_path,
                format!(
                    "db = {db:?}\nworkspace_root = {ws:?}\n[accounts]\nclaude_dir = {accounts_dir:?}\nmax_runs_per_account = {max_runs}\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n"
                ),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        };
        write_config(2);
        let config = Config::load(&config_path).unwrap_or_else(|e| panic!("{e}"));
        let mut dispatcher = build_dispatcher(&config, Default::default()).unwrap_or_else(|e| panic!("{e}"));

        // [accounts] が変わっていなければ通る。
        assert!(reload_providers(&mut dispatcher, &config).is_ok());

        // max_runs_per_account を変えると、次の reload はエラーになる。
        write_config(3);
        let err = reload_providers(&mut dispatcher, &config).unwrap_err();
        assert!(err.contains("[accounts]"), "{err}");
        assert!(err.contains("restart"), "{err}");
    }
}
