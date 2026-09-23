//! celeris: デーモン本体（DESIGN §3, §5.2, ADR-0005 D7）。設定読込、ログ初期化、tick ループ。
//! 判断ロジックは `task-dispatch` にあり、ここはループと配線だけ。

mod accounts_admin;
mod cluster_admin;
pub mod config;
/// ADR-0065 D3 / D5（Phase 110a）: 背景チェックポイントと定期バックアップ。
pub mod db_maintenance;
pub mod delivery;
/// ADR-0040 D4（Phase 47）: インスタンスの役割（active / standby / draining / verify）とライブ引き継ぎ。
pub mod instance;
/// ADR-0047 D4（Phase 62）: 知識の自動メンテナンス（決定的なトリガと適用。LLM は `langmem` アダプタの中）。
pub mod knowledge_maint;
/// ADR-0037（Phase 39）: 人の判断が要るときだけ Discord に知らせる（判定は決定的、送信は spawn）。
pub mod milestone_review;
pub mod notify;
/// ADR-0040 D6（Phase 48）: `[selfdeploy] releases_dir` を読む／`promote.sh` を起こす。
pub mod releases;
/// ADR-0033 D3（Phase 25）: 報告の圧縮（まとめの run を起こす決定的な判断）。
pub mod reports;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use task_api::types::{
    ApiConfigView, ClusterConfigView, ConfigView, GenreConfigView, ProviderConfigView,
    ReviewerConfigView, RoleConfigView,
};
use task_api::{ApiError, ApiSettings, ApiState};
use task_core::{
    DaemonMode, InstanceRole, SharedRole, SqliteStore, StoreError, StoreOptions, TaskStore,
};
use task_dispatch::{
    DispatchError, Dispatcher, ProviderId, SnapshotPublisher, StaticPolicy, TickReport,
};
use task_ops::daemon::{ProviderCheckView, ProviderLive};
use task_ops::view::ViewContext;
use task_worker::{
    AcpAdapter, AcpConfig, AiderAdapter, AiderConfig, ClaudeCodeAdapter, ClaudeCodeConfig,
    CodexAdapter, CodexConfig, FakeAdapter, LangMemAdapter, LangMemConfig, LdrAdapter, LdrConfig,
    PaperQaAdapter, PaperQaConfig, WorkerAdapter, Workspace,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub use config::{Config, ConfigError, Overrides};
pub use instance::InstanceIdentity;

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
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// idle な tick で exit する（テスト・バッチ用）。
    pub until_idle: bool,
    /// tick 数の上限（0 = 無制限）。
    pub max_ticks: u64,
    /// ADR-0040 D3（Phase 47）: `--mode`。`verify` は本番のデータのコピーに対する検証専用
    /// （dispatch しない、ワーカーを起こさない、tick の裏方を動かさない、`daemon_instances` に書かない）。
    pub mode: DaemonMode,
    /// ADR-0040 D4: `--release <sha12>`。無ければ環境変数 `CELERIS_RELEASE`、それも無ければ `"dev"`。
    pub release: Option<String>,
}

/// ループ終了の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Idle,
    MaxTicks,
    Signal,
    /// ADR-0040 D4: 引き継ぎのために `draining` になり、手元の run が 0 になった（または
    /// `[handoff] drain_timeout_secs` を超えて残りを abort した）。プロセスは exit 0 で終わる
    /// （systemd の `Restart=on-failure` では再起動されない）。
    Drained,
    /// ADR-0040 D4: 同じ `release` の `active` が既に動いていた。何もせず exit 3。
    DuplicateRelease,
}

/// `[adapters.<種別>].env` にプロバイダの `env` を重ねる（同名キーはプロバイダが優先。順序は決定的）。
/// ADR-0030 以降、本体（`build_adapters`）は `merged_env_with_secrets` を使う。これはテストが期待値を
/// 組み立てるのに使う（`env_from_secrets` が空なら `merged_env_with_secrets` と同じ結果になる）。
#[cfg(test)]
fn merged_env(
    base: &HashMap<String, String>,
    provider: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut merged: BTreeMap<String, String> =
        base.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
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
fn apply_env_from_secrets(
    merged: &mut BTreeMap<String, String>,
    mapping: &HashMap<String, String>,
    secrets_dir: Option<&Path>,
) {
    let mut env_keys: Vec<&String> = mapping.keys().collect();
    env_keys.sort();
    for env_key in env_keys {
        let secret_id = &mapping[env_key];
        if let Some(value) = resolve_secret(secrets_dir, secret_id) {
            merged.insert(env_key.clone(), value);
        }
    }
}

/// ADR-0030 D2: 優先順は celeris の環境（プロセス継承。ここでは扱わない）< `[adapters.*].env` <
/// `[adapters.*].env_from_secrets` < 行の `env` < 行の `env_from_secrets`。
fn merged_env_with_secrets(
    base_env: &HashMap<String, String>,
    base_env_from_secrets: &HashMap<String, String>,
    row_env: &HashMap<String, String>,
    row_env_from_secrets: &HashMap<String, String>,
    secrets_dir: Option<&Path>,
) -> Vec<(String, String)> {
    let mut merged: BTreeMap<String, String> = base_env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
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

/// ADR-0012 D1: `[[providers]]` の各行（モデル供給元）ごとにアダプタのインスタンスを作る。`[adapters.<種別>]` を基本設定とし、
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
                    env: merged_env_with_secrets(
                        &base.env,
                        &base.env_from_secrets,
                        &p.env,
                        &p.env_from_secrets,
                        secrets_dir,
                    ),
                    // ADR-0043 D3（Phase 56）: コンテナで走らせるかはタスクごとに決まるので、ここでは常に `None`
                    // （ディスパッチャが `with_container` で包んだ複製を作る）。
                    container: None,
                }))
            }
            CodexAdapter::ID => {
                let base = &config.adapters.codex;
                Arc::new(CodexAdapter::new(CodexConfig {
                    command: base.command.clone(),
                    extra_args: base.extra_args.clone(),
                    model: effective_model(&p.model, &base.model),
                    env: merged_env_with_secrets(
                        &base.env,
                        &base.env_from_secrets,
                        &p.env,
                        &p.env_from_secrets,
                        secrets_dir,
                    ),
                    // ADR-0043 D3（Phase 56）: コンテナで走らせるかはタスクごとに決まるので、ここでは常に `None`
                    // （ディスパッチャが `with_container` で包んだ複製を作る）。
                    container: None,
                    // ADR-0054 D1（Phase 67）: `[adapters.codex] resume_mode`（既定 `exec_resume`）。
                    resume_mode: base.resolved_resume_mode(),
                }))
            }
            AiderAdapter::ID => {
                let base = &config.adapters.aider;
                Arc::new(AiderAdapter::new(AiderConfig {
                    command: base.command.clone(),
                    extra_args: base.extra_args.clone(),
                    model: effective_model(&p.model, &base.model),
                    env: merged_env_with_secrets(
                        &base.env,
                        &base.env_from_secrets,
                        &p.env,
                        &p.env_from_secrets,
                        secrets_dir,
                    ),
                    // ADR-0043 D3（Phase 56）: コンテナで走らせるかはタスクごとに決まるので、ここでは常に `None`
                    // （ディスパッチャが `with_container` で包んだ複製を作る）。
                    container: None,
                }))
            }
            AcpAdapter::ID => {
                let base = &config.adapters.acp;
                Arc::new(AcpAdapter::new(AcpConfig {
                    // ADR-0026 D2: `command`/`args` は行ごとに上書きできる（別の ACP エージェントを同居させる
                    // ため）。`Config::validate` が acp 以外の行での指定を拒否している。
                    command: p.command.clone().unwrap_or_else(|| base.command.clone()),
                    args: p.args.clone().unwrap_or_else(|| base.args.clone()),
                    env: merged_env_with_secrets(
                        &base.env,
                        &base.env_from_secrets,
                        &p.env,
                        &p.env_from_secrets,
                        secrets_dir,
                    ),
                    permission: base.permission,
                    // ADR-0026 D3: `[adapters.acp]` にモデルの既定値は無い（CLI の `--model` フラグではなく
                    // `session/set_config_option` で渡すので、行の `model` が空ならモデル指定なしになるだけ）。
                    model: effective_model(&p.model, &None),
                    model_option_id: base.model_option_id.clone(),
                    startup_timeout: Duration::from_secs(base.startup_timeout_secs),
                    // ADR-0043 D3（Phase 56）: コンテナで走らせるかはタスクごとに決まるので、ここでは常に `None`
                    // （ディスパッチャが `with_container` で包んだ複製を作る）。
                    container: None,
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
                    env: merged_env_with_secrets(
                        &base.env,
                        &base.env_from_secrets,
                        &p.env,
                        &p.env_from_secrets,
                        secrets_dir,
                    ),
                    extra_args: base.extra_args.clone(),
                    // ADR-0035 D1 / D3: 取得と証拠ゲートは行ごとの上書きが無い（他の paperqa 設定と同じ扱い）。
                    acquire: base.acquire.clone(),
                    evidence: base.evidence,
                    // ADR-0063 Phase 109d C3: `max_asks` も行ごとの上書きが無い。
                    max_asks: base.max_asks,
                    // ADR-0063 Phase 109g A: コンテナと同じ `[knowledge] root`（`ContainerPlan.knowledge_root`
                    // と同じ絶対パス）。`paperqa_ask.py` が比較先のページ本文をここから直接読む。
                    knowledge_root: Some(config.knowledge.root.clone()),
                }))
            }
            LdrAdapter::ID => {
                let base = &config.adapters.local_deep_research;
                // ADR-0029 D1: 行ごとの上書きは `model`（`settings` の `llm.model` を上書き）と `env` だけ
                // （`ProviderConfig.settings` は `paperqa` 専用フィールドなので LDR では再利用しない。
                // celeris 側の実装判断。行ごとに調査対象を変えたければ `[[roles]]`/`[[genres]]` で使い分ける）。
                let mut settings: Vec<(String, String)> = base
                    .settings
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                settings.sort();
                Arc::new(LdrAdapter::new(LdrConfig {
                    command: base.command.clone(),
                    mode: base.mode,
                    iterations: base.iterations,
                    questions_per_iteration: base.questions_per_iteration,
                    settings,
                    model: effective_model(&p.model, &None),
                    env: merged_env_with_secrets(
                        &base.env,
                        &base.env_from_secrets,
                        &p.env,
                        &p.env_from_secrets,
                        secrets_dir,
                    ),
                    // ADR-0031 D2: 証拠ゲートの閾値は行ごとの上書きが無い（他の LDR 設定と同じ扱い）。
                    evidence: base.evidence,
                    // ADR-0063 D2: 再挑戦時の mode / iterations も行ごとの上書きは無い。
                    retry_mode: base.retry_mode,
                    retry_iterations: base.retry_iterations,
                    // ADR-0063 Phase 109c B3: 構造化合成の有無も行ごとの上書きは無い。
                    structured_synthesis: base.structured_synthesis,
                }))
            }
            LangMemAdapter::ID => {
                let base = &config.adapters.langmem;
                let llm = &config.knowledge.langmem;
                // ADR-0047 D4: `[adapters.langmem]`（起動コマンド・無出力タイムアウト）と
                // `[knowledge.langmem]`（LLM の接続先）を合わせて 1 つのアダプタ設定にする。
                Arc::new(LangMemAdapter::new(LangMemConfig {
                    command: base.command.clone(),
                    idle_timeout_secs: base.idle_timeout_secs,
                    provider: llm.provider,
                    base_url: llm.base_url.clone(),
                    model: llm.model.clone(),
                    api_key: llm
                        .api_key_secret
                        .as_deref()
                        .and_then(|id| resolve_secret(secrets_dir, id)),
                    env: merged_env_with_secrets(
                        &base.env,
                        &base.env_from_secrets,
                        &p.env,
                        &p.env_from_secrets,
                        secrets_dir,
                    ),
                }))
            }
            // `Config::validate` が fake / claude-code / codex / aider / acp / paperqa /
            // local-deep-research / langmem 以外を拒否している。
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
        adapters.insert(
            p.id.clone(),
            Arc::new(task_worker::tiered::TieredAdapter {
                base: adapter,
                models: p.tier_models.clone(),
                account_id: p.account_id.clone(),
                credential_error: p.env_from_secrets.iter()
                    .find(|(key, id)| task_core::model_routing::CREDENTIAL_KEYS.contains(&key.as_str()) && resolve_secret(secrets_dir, id).is_none())
                    .map(|(_, id)| format!("credential reference {id} is missing or unreadable; configure it in Accounts")),
            }),
        );
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
        map.entry(secret_id.to_string())
            .or_default()
            .push(task_api::types::SecretUse {
                scope: scope.to_string(),
                name: name.to_string(),
                env: env.to_string(),
            });
    }

    let mut map: HashMap<String, Vec<task_api::types::SecretUse>> = HashMap::new();
    let adapters: [(&str, &HashMap<String, String>); 8] = [
        (
            ClaudeCodeAdapter::ID,
            &config.adapters.claude_code.env_from_secrets,
        ),
        (CodexAdapter::ID, &config.adapters.codex.env_from_secrets),
        (AiderAdapter::ID, &config.adapters.aider.env_from_secrets),
        (FakeAdapter::ID, &config.adapters.fake.env_from_secrets),
        (AcpAdapter::ID, &config.adapters.acp.env_from_secrets),
        (
            PaperQaAdapter::ID,
            &config.adapters.paperqa.env_from_secrets,
        ),
        (
            LdrAdapter::ID,
            &config.adapters.local_deep_research.env_from_secrets,
        ),
        (
            LangMemAdapter::ID,
            &config.adapters.langmem.env_from_secrets,
        ),
    ];
    for (name, from_secrets) in adapters {
        let mut env_keys: Vec<&String> = from_secrets.keys().collect();
        env_keys.sort();
        for env_key in env_keys {
            push(&mut map, &from_secrets[env_key], "adapter", name, env_key);
        }
    }
    // ADR-0047 D4: `[knowledge.langmem].api_key_secret`（環境変数の写像ではなく 1 つの LLM 鍵）。
    if let Some(id) = &config.knowledge.langmem.api_key_secret {
        push(&mut map, id, "adapter", LangMemAdapter::ID, "api_key");
    }
    let mut providers: Vec<&config::ProviderConfig> = config.providers.iter().collect();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    for p in providers {
        let mut env_keys: Vec<&String> = p.env_from_secrets.keys().collect();
        env_keys.sort();
        for env_key in env_keys {
            push(
                &mut map,
                &p.env_from_secrets[env_key],
                "provider",
                &p.id,
                env_key,
            );
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
                AiderAdapter::ID => config.adapters.aider.model.clone(),
                // ADR-0026 D3 / ADR-0027 D3: acp / paperqa には `[adapters.<種別>].model` が無い。
                // 行の `model` が空なら `None` になる。
                _ => None,
            };
            (
                p.id.clone(),
                effective_model(&p.model, &adapter_model).unwrap_or_default(),
            )
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
                credential_refs: task_core::model_routing::credential_refs(&p.env_from_secrets),
                tier_models: p.tier_models.clone(),
                account_id: p.account_id.clone(),
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
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "9p",
    "afs",
    "ceph",
    "lustre",
    "gpfs",
    "beegfs",
    "glusterfs",
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
        if target.starts_with(mount_point)
            && best
                .as_ref()
                .is_none_or(|(len, _)| mount_point.len() >= *len)
        {
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
pub fn build_dispatcher(
    config: &Config,
    masters: ClusterMasters,
) -> Result<Dispatcher, DaemonError> {
    config.ensure_accounts_dir()?;
    config.ensure_secrets_dir()?;
    config.ensure_memory_dir()?;
    // ADR-0065 D1/D5: `[db]` の `busy_timeout_ms` を使い、デーモンの書き込み接続は
    // `background_checkpoint` を立てる（別の背景 tick が `PRAGMA wal_checkpoint(PASSIVE)` を打つ）。
    let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_with(
        &config.db.path,
        StoreOptions {
            busy_timeout: config.db.busy_timeout(),
            background_checkpoint: true,
            ..StoreOptions::default()
        },
    )?);
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
    dispatcher.set_cluster_connector(cluster_connector(
        masters.clone(),
        master_launchers(config),
        cluster_keepalive_secs(config),
    ));
    // ADR-0062 A（Phase 107）: master 越しの実通信 probe・死んだ接続の片付け・celeris 保持 master の
    // 終了検出は、ここ（テストからも広く呼ばれる `build_dispatcher`）では配線しない。実 ssh を打つ
    // フックなので、テストの偽 `cluster_liveness_probe`（`alive = true`）だけでは防げず、本物の
    // `ssh -o BatchMode=yes <host> -- true` が飛んでしまう（CLAUDE.md: テストで外部ネットワークに
    // 出ない）。本番の起動経路（`wire_cluster_liveness_hooks`）だけで配線する。
    // ADR-0053 D3（Phase 66）: `[[clusters.forwards]]`（Qwen トンネル等）の(再)確立と生存監視。
    // 設定に forward が無ければ `refresh_cluster_tunnels` 自体が早期に戻るので、挿しても無害。
    let tunnel_forward_children: TunnelForwardChildren =
        Arc::new(std::sync::Mutex::new(TunnelForwardRegistry::default()));
    dispatcher.set_tunnel_forward_ensurer(tunnel_forward_ensurer(tunnel_forward_children));
    // ADR-0053 Phase 85: listener（手元の待ち受け。軽い TCP connect）と target の健康
    // （`/v1/models`。バックオフされる）を別のフックで挿す。
    dispatcher.set_tunnel_listener_probe(tunnel_listener_probe());
    dispatcher.set_tunnel_probe(tunnel_probe());
    // ADR-0043 D3（Phase 56）: 起動時に 1 度だけコンテナ runtime を調べる（`podman info` → `docker info`）。
    // 結果はログと `GET /daemon` の `containers` に出る。使えなければ `run = container` のタスクは
    // dispatch されず `blocked`（「コンテナ runtime が使えません」）になる。
    dispatcher.detect_container_runtime();
    Ok(dispatcher)
}

/// ADR-0033 D1: 組織図の種を蒔く。**`org_nodes` が空のときだけ**書き、それ以外は何もしない
/// （以後の編集は GUI → API → DB。設定は再読込しない）。蒔いた件数を返す。
pub fn seed_org_if_empty(store: &dyn TaskStore, config: &Config) -> Result<usize, DaemonError> {
    if config.org.is_empty() {
        return Ok(0);
    }
    if !store.org_list()?.is_empty() {
        tracing::debug!(
            "org: org_nodes is not empty; the config seed is not applied (the DB wins)"
        );
        return Ok(0);
    }
    let now = OffsetDateTime::now_utc();
    let nodes = config.org_nodes(now);
    // 監査 D-4: 1 トランザクションで蒔く。途中の 1 件が不正でも部分的に書かれた組織が残らない
    // （残ると次回起動時は `org_list` が空でなくなり、二度と補完されない）。
    store.org_seed(&nodes)?;
    tracing::info!(
        count = nodes.len(),
        "org: seeded the organization from the config"
    );
    Ok(nodes.len())
}

/// ADR-0032 D2: celeris が張った ssh master を保持する場所。`ClusterMaster` を落とすと接続も切れるので、
/// **接続を生かしておきたい間はここに置く**（`DELETE /clusters/{id}/connect` はここから取り除く）。
/// 人が `cluster-login.sh` で張った master はこのマップに載らない（celeris の持ち物ではないため）。
pub type ClusterMasters =
    Arc<std::sync::Mutex<HashMap<String, task_worker::cluster_login::ClusterMaster>>>;

/// ADR-0062 A（Phase 107）: クラスタ id ごとの keepalive の秒数（`[[clusters]] keepalive_secs`）。
/// `[[clusters]]` は `POST /reload` で変わらないので、起動時に一度だけ表を作れば十分。
fn cluster_keepalive_secs(config: &Config) -> Arc<HashMap<String, u64>> {
    Arc::new(
        config
            .clusters
            .iter()
            .map(|c| (c.id.clone(), c.keepalive_secs))
            .collect(),
    )
}

/// ADR-0060（Phase 103）: `[[clusters]] master_launcher` を、クラスタ id ごとに実際の起こし方へ解決する。
/// `[[clusters]]` は `POST /reload` で変わらない（起動時に再読込しない）ので、起動時に一度だけ計算すれば
/// 十分（`cluster_connector` の呼び出しごとに `systemd-run` の PATH 検索をやり直さない）。
fn master_launchers(
    config: &Config,
) -> Arc<HashMap<String, task_worker::cluster_login::MasterLauncher>> {
    let has_systemd_run = task_worker::cluster_login::systemd_run_on_path();
    let has_xdg_runtime_dir = task_worker::cluster_login::xdg_runtime_dir_is_set();
    Arc::new(
        config
            .clusters
            .iter()
            .map(|c| {
                let launcher = task_worker::cluster_login::resolve_master_launcher(
                    &c.master_launcher,
                    has_systemd_run,
                    has_xdg_runtime_dir,
                );
                (c.id.clone(), launcher)
            })
            .collect(),
    )
}

/// ADR-0032 D3: `auth = "publickey"` のクラスタを、ディスパッチの直前に 1 回だけ自分で張る。
///
/// **時間の設計**: これはディスパッチループの中から同期で呼ばれる（`control_master_alive_blocking` と
/// 同じ立場）。`-O check` は 1 秒で返るが接続はもっとかかるので、長く待つと tick 全体が止まる。
/// そこで **`AUTO_CONNECT_TIMEOUT` を短く（8 秒）**切る。鍵だけの接続は実測で 1 秒未満なので
/// （ADR-0032 §1 の fern03）、これで足りる。間に合わなければその tick は cooldown に落ち、
/// 次の機会に再試行される（人を待たせるより tick を止めない方を優先する）。
fn cluster_connector(
    masters: ClusterMasters,
    launchers: Arc<HashMap<String, task_worker::cluster_login::MasterLauncher>>,
    keepalives: Arc<HashMap<String, u64>>,
) -> task_dispatch::dispatcher::ClusterConnector {
    /// 自動接続に使う上限。ディスパッチループを止めないために短くしてある（上の説明）。
    const AUTO_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

    Arc::new(move |cluster_id: &str, host: &str| {
        let host = host.to_string();
        let cluster_id = cluster_id.to_string();
        // ADR-0060（Phase 103）: master_launcher は `[[clusters]]` 由来（再読込では変わらない）なので、
        // 起動時に一度だけ解決して渡された表から引く。無ければ安全側（従来どおり inline）。
        let launcher = launchers
            .get(&cluster_id)
            .cloned()
            .unwrap_or(task_worker::cluster_login::MasterLauncher::Inline);
        // ADR-0062 A（Phase 107）。
        let keepalive_secs = keepalives.get(&cluster_id).copied().unwrap_or(0);
        // Phase 66b（本番 2026-09-21 の観測）: このクロージャは非同期の `start_connect` を専用ランタイムで
        // `block_on` する。呼び出し元（`task_dispatch::dispatcher::run_cluster_hooks_off_async`）は
        // tokio の文脈を持たない OS スレッドへ逃がしてから呼ぶ契約になっているが、万一これが破られて
        // tokio ランタイムのワーカースレッドから直接呼ばれると、下の `Builder::new_current_thread().build()`
        // 後の `.block_on()` が「Cannot start a runtime from within a runtime」で panic する
        // （`crates/celeris/src/lib.rs:621` で実際に panic した）。ここで一度だけ確かめ、破られていたら
        // panic ではなくエラーを返す（呼び出し側は cooldown に落とすだけで、デーモンは死なない）。
        if tokio::runtime::Handle::try_current().is_ok() {
            tracing::error!(
                cluster = %cluster_id,
                host = %host,
                "cluster connect must not be called from an async context (Phase 66b guard; \
                 this is a bug in the caller, not in ssh/network)"
            );
            return Err("cluster connect must not be called from an async context".to_string());
        }
        // ディスパッチループは同期なので、非同期の `start_connect` を専用ランタイムで回す。
        // `Handle::current().block_on` は同じランタイムのワーカースレッドを塞いでパニックしうるため使わない。
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("could not build a runtime for the cluster connect: {e}"))?;
        let outcome = runtime.block_on(task_worker::cluster_login::start_connect(
            &["ssh".to_string()],
            &host,
            &cluster_id,
            &launcher,
            false, // publickey のみ。人の入力は要らない（要るクラスタはここに来ない）
            AUTO_CONNECT_TIMEOUT,
            AUTO_CONNECT_TIMEOUT,
            keepalive_secs,
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
                Err(
                    "the host asked for a verification code; set auth = \"totp\" for this cluster"
                        .to_string(),
                )
            }
            Err(e) => Err(format!("{e}")),
        }
    })
}

/// ADR-0062 A（Phase 107）: 実 ssh を打つ 3 つのフック（実通信 probe・死んだ接続の片付け・
/// celeris 保持 master の終了検出）を配線する。**本番の起動経路からだけ**呼ぶこと
/// （`build_dispatcher` は `accounts_admin`/`lib.rs` の多数のテストから呼ばれ、そこでは
/// `cluster_liveness_probe` を偽物にすり替えるだけで済ませているため、実 ssh を打つこの 3 つを
/// そこに混ぜると CLAUDE.md の「テストで外部ネットワークに出ない」を破る）。
pub fn wire_cluster_liveness_hooks(dispatcher: &mut Dispatcher, masters: ClusterMasters) {
    dispatcher.set_cluster_command_probe(Arc::new(
        |ssh_command: &[String], host: &str, timeout: std::time::Duration| {
            task_worker::ssh::control_master_command_probe_blocking(ssh_command, host, timeout)
        },
    ));
    dispatcher.set_cluster_disconnector(Arc::new(move |_cluster_id: &str, host: &str| {
        // `disconnect` は非同期なので、`cluster_connector` と同じく専用ランタイムで回す
        // （tick ループの同期の文脈から呼ばれるため）。
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("could not build a runtime for the cluster disconnect: {e}"))?;
        runtime
            .block_on(task_worker::cluster_login::disconnect(
                &["ssh".to_string()],
                host,
            ))
            .map_err(|e| e.to_string())
    }));
    dispatcher.set_cluster_master_watcher(cluster_master_watcher(masters));
}

/// ADR-0062 A（Phase 107）: `ClusterMasters` から、明示的な切断を経ずに終了した master を集める
/// フック。`try_wait_exit` は非破壊（ブロックしない）なので tick から直接呼んでよい。終了を見つけたら
/// マップから取り除く（同じ終了を二度返さないため。`ClusterMaster::kill` と同じくもう保持する意味が無い）。
fn cluster_master_watcher(masters: ClusterMasters) -> task_dispatch::dispatcher::ClusterMasterWatcher {
    Arc::new(move || {
        let mut exited = Vec::new();
        let Ok(mut held) = masters.lock() else {
            return exited;
        };
        let ids: Vec<String> = held.keys().cloned().collect();
        for id in ids {
            let Some(master) = held.get_mut(&id) else {
                continue;
            };
            if let Some(exit_code) = master.try_wait_exit() {
                let stderr_tail = master.stderr_tail(300);
                held.remove(&id);
                exited.push(task_dispatch::dispatcher::ClusterMasterExit {
                    cluster: id,
                    exit_code,
                    stderr_tail,
                });
            }
        }
        exited
    })
}

/// ADR-0053 D3（Phase 66）: celeris が spawn したフォールバックの `ssh -N -L` 子を保持する場所。
/// `-O forward` が届かないとき（bnode150 が pegasus から直接届かない等）だけここに増える。
/// Drop で全部落とす（celeris の終了とともに閉じる）。**master 本体（`ClusterMaster`）とは違い**、
/// この子は ADR-0060 の対象外（celeris の cgroup の中のまま。滅多に使わないフォールバック経路なので、
/// このフェーズでは触っていない）。
#[derive(Default)]
struct TunnelForwardRegistry(HashMap<String, std::process::Child>);

impl Drop for TunnelForwardRegistry {
    fn drop(&mut self) {
        for (_, mut child) in self.0.drain() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

type TunnelForwardChildren = Arc<std::sync::Mutex<TunnelForwardRegistry>>;

/// ADR-0053 D3: `[[clusters.forwards]]` を(再)確立するフック。
///
/// 1. まず master に `-O forward -L <listen>:<target> <host>` を頼む（軽い。master が `target` に
///    届けば十分。ADR-0053 D3 の本筋）。
/// 2. 届かなければ（`-O forward` が失敗）、master 上で別プロセスとして `ssh -N -L <listen>:<target> <host>`
///    を張る（ADR-0053 D3「bnode150 が直接届かないなら master 上で ssh -N -L を起こす」フォールバック）。
///    この子プロセスは `TunnelForwardChildren` に保持し、生きている間は二重に起こさない。
fn tunnel_forward_ensurer(
    children: TunnelForwardChildren,
) -> task_dispatch::dispatcher::TunnelForwardEnsurer {
    Arc::new(move |host: &str, listen: &str, target: &str| {
        let key = format!("{host}\u{0}{listen}\u{0}{target}");
        {
            let mut guard = children
                .lock()
                .map_err(|_| "the tunnel forward registry is poisoned".to_string())?;
            if let Some(child) = guard.0.get_mut(&key) {
                if matches!(child.try_wait(), Ok(None)) {
                    // まだ立ち上げ中／生きている。二重に起こさない。
                    return Ok(());
                }
                guard.0.remove(&key);
            }
        }
        let spec = format!("{listen}:{target}");
        let status = std::process::Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-O", "forward", "-L", &spec, host])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if matches!(status, Ok(s) if s.success()) {
            tracing::info!(host, listen, target, "tunnel: forward added via -O forward");
            return Ok(());
        }
        tracing::warn!(
            host,
            listen,
            target,
            "tunnel: -O forward failed; falling back to a separate ssh -N -L"
        );
        let mut cmd = std::process::Command::new("ssh");
        cmd.args(["-o", "BatchMode=yes", "-N", "-L", &spec, host])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| format!("could not start the fallback `ssh -N -L`: {e}"))?;
        tracing::info!(
            host,
            listen,
            target,
            "tunnel: forward added via a fallback ssh -N -L child"
        );
        let mut guard = children
            .lock()
            .map_err(|_| "the tunnel forward registry is poisoned".to_string())?;
        guard.0.insert(key, child);
        Ok(())
    })
}

/// ADR-0053 Phase 85: target（先方）の健康 probe の上限。`task_worker::PROBE_TIMEOUT`（3 秒。
/// `[knowledge.langmem]` 等の到達性検査と共有の既定値）より短くしてある: この probe は
/// `refresh_cluster_tunnels`（同期・tick を止めうる経路）から呼ばれるため、先方が応答しないとき
/// tick を長く止めないよう 2 秒で切る（本番観測: forward は張れているのに先方が無応答で、旧実装は
/// 3 秒の probe を毎 tick 行い tick が 6 秒に伸びていた）。
const TUNNEL_TARGET_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// ADR-0053 D3 / Phase 85: forward の target（先方）の健康を `GET http://<listen>/v1/models` で見る
/// （既存の probe をそのまま使うが、タイムアウトは tick 向けに短くしてある）。
fn tunnel_probe() -> task_dispatch::dispatcher::TunnelProbe {
    Arc::new(|listen: &str| {
        let base = format!("http://{listen}/v1");
        // Qwen 側の中継はトンネルの向こう（bnode150）そのものであり、celeris の llm-proxy を経由しない
        // ので bearer は要らない（ADR-0052 の knowledge probe と同じ Qwen エンドポイントに対する既定）。
        matches!(
            task_worker::probe_models(&base, TUNNEL_TARGET_PROBE_TIMEOUT, None),
            task_worker::Reachability::Ok
        )
    })
}

/// ADR-0053 Phase 85: forward の**リスナー**（`-O forward`/`ssh -N -L` が手元の `listen` で実際に
/// 待ち受けているか）を見る。ssh は起こさず、`listen` への軽い TCP connect だけで判定する
/// （届けば「リスナーは有る」。中身の健康は見ない＝`tunnel_probe` の役目と分ける）。
fn tunnel_listener_probe() -> task_dispatch::dispatcher::TunnelListenerProbe {
    /// TCP connect 自体の上限。ローカルの loopback アドレスへの接続なので短くてよい。
    const LISTENER_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
    Arc::new(|listen: &str| {
        let Ok(addr) = listen.parse::<std::net::SocketAddr>() else {
            tracing::warn!(listen, "tunnel: could not parse the forward's listen address");
            return false;
        };
        std::net::TcpStream::connect_timeout(&addr, LISTENER_CONNECT_TIMEOUT).is_ok()
    })
}

/// ADR-0013 / `docs/gui/api.md` §3.21: `GET /api/v1/config` に出す設定の要約。env は**キー名だけ**、トークンとその場所は出さない。
pub fn config_view(config: &Config, listen: SocketAddr) -> ConfigView {
    let models = effective_models(config);
    ConfigView {
        config_path: config
            .source_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        db: config.db.path.display().to_string(),
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
        reviewer: ReviewerConfigView {
            adapter: config.reviewer.adapter.clone(),
            tier: config.reviewer.tier,
        },
        providers: config
            .providers
            .iter()
            .map(|p| {
                let mut env_keys: Vec<String> = p.env.keys().cloned().collect();
                env_keys.sort();
                ProviderConfigView {
                    credential_refs: task_core::model_routing::credential_refs(&p.env_from_secrets),
                    tier_models: p.tier_models.clone(),
                    account_id: p.account_id.clone(),
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
                    // ADR-0059 D6: 設定ファイルの値だけ（DB の上書きは `GET /clusters` の
                    // `ClusterView.work_dir`/`work_dir_source` が持つ）。
                    work_dir: c.work_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    // ADR-0032 D1: 接続の張り方（`manual` / `publickey` / `totp`）。GUI が出し分けに使う。
                    auth: c.auth.clone(),
                    concurrency: c.concurrency,
                    sync: c.sync.clone(),
                    delete_on_push: c.delete_on_push,
                    has_setup: !c.setup.is_empty(),
                    env_keys,
                    rsync_excludes: c.rsync_excludes.clone(),
                    // ADR-0053 D3（Phase 66）。
                    forwards: c
                        .forwards
                        .iter()
                        .map(|f| task_api::ClusterForwardView {
                            listen: f.listen.clone(),
                            target: f.target.clone(),
                            up: None,
                            listener: None,
                            target_healthy: None,
                            last_error: None,
                        })
                        .collect(),
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

/// ADR-0053 D4（Phase 65）: `task_api::LlmSourcesReader` を `llm_proxy::ProxyState` の薄い包みで実装する
/// （task-api は `llm-proxy`/`task-dispatch`/`task-worker` を知らない。ADR-0017 M2 と同じ境界）。
struct LlmSourcesAdapter(Arc<llm_proxy::ProxyState>);

#[async_trait::async_trait]
impl task_api::LlmSourcesReader for LlmSourcesAdapter {
    async fn view(&self, now: i64) -> task_api::LlmSourcesView {
        let view = self.0.sources_view(now).await;
        task_api::LlmSourcesView {
            sources: view
                .sources
                .into_iter()
                .map(|s| task_api::LlmSourceView {
                    id: s.id,
                    kind: s.kind,
                    enabled: s.enabled,
                    reachable: s.reachable,
                    accounts: s
                        .accounts
                        .into_iter()
                        .map(|a| task_api::LlmSourceAccountView {
                            id: a.id,
                            logged_in: a.logged_in,
                            remaining: a.remaining,
                            remaining_short: a.remaining_short,
                            remaining_long: a.remaining_long,
                            cooldown_until: a.cooldown_until,
                            cooldown_reason: a.cooldown_reason,
                        })
                        .collect(),
                    last_hour_requests: s.last_hour_requests,
                    last_hour_prompt_tokens: s.last_hour_prompt_tokens,
                    last_hour_completion_tokens: s.last_hour_completion_tokens,
                })
                .collect(),
            celeris_tiers: view
                .celeris_tiers
                .into_iter()
                .map(|t| task_api::LlmCelerisTierView {
                    tier: t.tier,
                    resolves_to: t.resolves_to,
                })
                .collect(),
        }
    }
}

/// `task_api::ApiSettings` を設定から作る。`instance_id` / `started_at` はディスパッチャのスナップショットと同じ値を渡す。
/// ADR-0040 D3 / D4: `release` / `mode` / `role` は `GET /health` に出て、`role` は standby の 503 にも使う。
/// ADR-0053 D4（Phase 65）: `llm_proxy_state` があれば `GET /llm/sources` を有効にする（`None` は 409）。
#[allow(clippy::too_many_arguments)]
pub fn api_settings(
    config: &Config,
    listen: SocketAddr,
    token: Option<String>,
    instance_id: String,
    started_at: String,
    admin_tx: Option<tokio::sync::mpsc::Sender<task_api::AdminRequest>>,
    release: String,
    mode: DaemonMode,
    role: SharedRole,
    llm_proxy_state: Option<Arc<llm_proxy::ProxyState>>,
) -> ApiSettings {
    ApiSettings {
        listen,
        token,
        allowed_hosts: config.api.allowed_hosts.clone(),
        db_path: config.db.path.clone(),
        busy_timeout: config.db.busy_timeout(),
        // ADR-0065 D5: API もデーモン内の接続なので背景チェックポイント側に回す。
        background_checkpoint: true,
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
        celeris_version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id,
        started_at,
        providers_dir: config.providers_dir.clone(),
        admin_tx,
        accounts_roots: config
            .accounts
            .as_ref()
            .map(|a| a.roots())
            .unwrap_or_default(),
        max_runs_per_account: config
            .accounts
            .as_ref()
            .map(|a| a.max_runs_per_account)
            .unwrap_or(0),
        secrets_dir: config.secrets.as_ref().map(|s| s.dir.clone()),
        secret_usage: secret_usage(config),
        memory_dir: config.memory.as_ref().map(|m| m.dir.clone()),
        notify_secret_id: config.notify.discord_webhook_secret.clone(),
        notify_gui_base_url: config.notify.base_url().map(str::to_string),
        // ADR-0040 D6（Phase 48）: `GET /releases` / `POST /releases/{sha12}/promote` が読む先。
        // task-api はファイルの規約を知らないので、読む係をここで渡す。
        releases: Some(Arc::new(crate::releases::FsReleases::new(
            config.selfdeploy.releases_dir.clone(),
            // ADR-0041 D3: `on_main` を出すためだけに読む作業チェックアウト（書き換えない）。
            config.selfdeploy.repo.clone(),
        ))),
        release,
        mode,
        role,
        // ADR-0043 D5（Phase 54）: 取り込みで使う `gh` の場所と merge の方法。
        github: task_api::GithubSettings {
            gh: config.github.gh.clone(),
            merge_method: config.github.merge_method.clone(),
        },
        // ADR-0044 D7（Phase 57）: 案件に git のリポジトリが無いときに文書リポジトリを作る場所
        // （SPEC §5: 成果物は `~/workspace/` に）。`$HOME` が無ければ作れない（409）。
        docs_repo_root: task_core::home_dir().map(|home| home.join("workspace")),
        // ADR-0047 D1（Phase 61）: 知識ベースの正本（既定 `~/.local/share/celeris/knowledge`）。**API は作らない**。
        knowledge_root: Some(config.knowledge.root.clone()),
        llm_sources: llm_proxy_state.map(|s| Arc::new(LlmSourcesAdapter(s)) as task_api::SharedLlmSourcesReader),
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

/// 動いている LLM source プロキシ（ADR-0053 D1。Phase 65）。`stop` で graceful に止める。
struct RunningLlmProxy {
    stop: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl RunningLlmProxy {
    async fn stop(self) {
        let _ = self.stop.send(());
        match tokio::time::timeout(Duration::from_secs(5), self.handle).await {
            Ok(Ok(Ok(()))) => tracing::info!("llm-proxy stopped"),
            Ok(Ok(Err(e))) => tracing::error!(error = %e, "llm-proxy server failed"),
            Ok(Err(e)) => tracing::error!(error = %e, "llm-proxy task panicked"),
            Err(_) => tracing::warn!("llm-proxy did not stop within 5s"),
        }
    }
}

/// 動いている MCP サーバー（ADR-0056 D1/D5。Phase 78）。口ごとに別の listener を持つので、
/// 止めるときは全部の handle をまとめて待つ。
struct RunningMcp {
    listeners: Vec<(tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<std::io::Result<()>>)>,
}

impl RunningMcp {
    async fn stop(self) {
        for (stop, handle) in self.listeners {
            let _ = stop.send(());
            match tokio::time::timeout(Duration::from_secs(5), handle).await {
                Ok(Ok(Ok(()))) => tracing::info!("mcp listener stopped"),
                Ok(Ok(Err(e))) => tracing::error!(error = %e, "mcp listener failed"),
                Ok(Err(e)) => tracing::error!(error = %e, "mcp listener task panicked"),
                Err(_) => tracing::warn!("mcp listener did not stop within 5s"),
            }
        }
    }
}

/// ADR-0056 D1/D5（Phase 78）: `[mcp]` が有効なら `celeris_mcp::McpState` を組み立てる（bind はまだ
/// しない）。`[mcp]` は独立の DB 接続を持つ（`task-api` の `ApiState` と同じ多重接続の流儀）。
fn build_mcp_state(config: &Config) -> Result<Option<Arc<celeris_mcp::McpState>>, DaemonError> {
    if !config.mcp.effective_enabled() {
        return Ok(None);
    }
    let state = celeris_mcp::McpState::open(
        &config.db.path,
        config.db.busy_timeout(),
        config.mcp.rate_limit_per_min,
        config.role_specs(),
        config.genre_specs(),
        config.conversation_genre_id().to_string(),
        Some(config.knowledge.root.clone()),
        // ADR-0065 D5: MCP もデーモン内の接続なので背景チェックポイント側に回す。
        true,
    )
    .map_err(|e| ApiError::Startup(format!("mcp: could not open the store: {e}")))?;
    Ok(Some(state))
}

/// `state` を `[mcp]`/`[[mcp.listeners]]` の全ての口に bind して動かす（`config.mcp.resolve_listeners()`
/// は `Config::validate` が既に検査済み。ここで失敗するのは bind そのものだけ）。
async fn start_mcp(config: &Config, state: Arc<celeris_mcp::McpState>) -> Result<RunningMcp, DaemonError> {
    let resolved = config
        .mcp
        .resolve_listeners()
        .map_err(|e| ApiError::Startup(format!("mcp: {e}")))?;
    let mut listeners = Vec::with_capacity(resolved.len());
    for listener in resolved {
        let bound = bind_reuseport(listener.listen).map_err(|source| ApiError::Bind {
            addr: listener.listen,
            source,
        })?;
        let addr = bound.local_addr().unwrap_or(listener.listen);
        tracing::info!(%addr, auth = ?listener.auth, "mcp listening");
        let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(celeris_mcp::serve(bound, Arc::clone(&state), listener.auth, async move {
            let _ = stop_rx.await;
        }));
        listeners.push((stop, handle));
    }
    Ok(RunningMcp { listeners })
}

/// ADR-0053 D1（Phase 65）: `[llm_proxy]` が有効なら `llm_proxy::ProxyState` を組み立てる（bind はまだ
/// しない。`GET /llm/sources`（主 API）とプロキシ自身の両方がこの同じ `Arc` を使うため、`run()` が
/// 主 API の起動より前に 1 度だけ呼ぶ）。アカウントプール（cooldown・観測値）はディスパッチャの帳簿を
/// **そのまま共有する**（`crates/task-dispatch/src/dispatcher.rs` の `account_book`。別の写しを作らない）。
/// Bearer は `[api] token_file` と同じ（`docs/llm-source.md`）。
fn build_llm_proxy_state(
    config: &Config,
    dispatcher: &Dispatcher,
    role: SharedRole,
) -> Result<Option<Arc<llm_proxy::ProxyState>>, DaemonError> {
    if !config.llm_proxy.effective_enabled() {
        return Ok(None);
    }
    let token = config.api.read_token()?;
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| ApiError::Startup(format!("llm-proxy: could not build the HTTP client: {e}")))?;
    let claude_book = dispatcher.account_book(task_core::AccountAdapter::ClaudeCode);
    let codex_book = dispatcher.account_book(task_core::AccountAdapter::Codex);
    Ok(Some(llm_proxy::ProxyState::new(
        config.llm_proxy.clone(),
        client,
        claude_book,
        codex_book,
        token,
        role,
        Some(config.db.path.clone()),
        config.db.busy_timeout(),
    )))
}

/// `state` を `[llm_proxy] listen` に bind して動かす。`standby`/`draining` の間はプロキシ自身の
/// `guard` が他の管理 API と同じく 503 を返す（bind/unbind は主 API と同じ `SO_REUSEPORT` のライフサイクル）。
async fn start_llm_proxy(config: &Config, state: Arc<llm_proxy::ProxyState>) -> Result<RunningLlmProxy, DaemonError> {
    let listen = config.llm_proxy.listen;
    let listener = bind_reuseport(listen).map_err(|source| ApiError::Bind { addr: listen, source })?;
    let addr = listener.local_addr().unwrap_or(listen);
    tracing::info!(%addr, "llm-proxy listening");
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(llm_proxy::serve(listener, state, async move {
        let _ = stop_rx.await;
    }));
    Ok(RunningLlmProxy { stop, handle })
}

/// ADR-0040 D4（Phase 47）: `SO_REUSEPORT` で bind する。新しいリリースの `standby` が、動いている
/// `active` と**同じポート**に起動と同時に bind できるようにするため（カーネルが新しい接続を振り分ける。
/// 読み書きは同じ DB なので問題ない）。`SO_REUSEADDR` も立てる（旧 listener の `TIME_WAIT` を跨ぐため）。
pub fn bind_reuseport(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    // `tokio::net::TcpListener::bind` と同じ待ち行列の深さ。
    socket.listen(1024)?;
    tokio::net::TcpListener::from_std(std::net::TcpListener::from(socket))
}

/// ADR-0013 D3 / D4: ディスパッチャにスナップショットの送り口を付け、API 専用の DB 接続を開いて bind する。
/// 開けない・bind できないときは起動を失敗させる（黙って API 無しで動かない）。
/// ADR-0040 D3 / D4: `admin = false`（verify モード）では管理系の委譲チャネルを作らない（tick ループが
/// 動かないので、送っても誰も受け取れない）。bind は `SO_REUSEPORT` で行う。
#[allow(clippy::too_many_arguments)]
async fn start_api(
    config: &Config,
    listen: SocketAddr,
    dispatcher: &mut Dispatcher,
    identity: &InstanceIdentity,
    mode: DaemonMode,
    role: SharedRole,
    admin: bool,
    llm_proxy_state: Option<Arc<llm_proxy::ProxyState>>,
) -> Result<
    (
        RunningApi,
        Option<tokio::sync::mpsc::Receiver<task_api::AdminRequest>>,
    ),
    DaemonError,
> {
    let token = config.api.read_token()?;
    let instance_id = identity.instance_id.clone();
    let started_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
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
    // 境界を守るため）。celeris の tick ループへ委譲するチャネルを作り、送信側だけ API に渡す。
    let (admin_tx, admin_rx) = match admin {
        true => {
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            (Some(tx), Some(rx))
        }
        false => (None, None),
    };
    let settings = api_settings(
        config,
        listen,
        token,
        instance_id,
        started_at,
        admin_tx,
        identity.release.clone(),
        mode,
        role,
        llm_proxy_state,
    );
    let state = tokio::task::spawn_blocking(move || ApiState::new(settings, rx))
        .await
        .map_err(|e| ApiError::Startup(e.to_string()))??;
    let listener = bind_reuseport(listen).map_err(|source| ApiError::Bind {
        addr: listen,
        source,
    })?;
    let addr = listener.local_addr().unwrap_or(listen);
    tracing::info!(%addr, auth_required = config.api.token_file.is_some(), "api listening");
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(task_api::serve_with_listener(listener, state, async move {
        let _ = stop_rx.await;
    }));
    Ok((RunningApi { stop, handle }, admin_rx))
}

/// ADR-0040 D4（Phase 47）: tick ループが役割のために持つもの。API は `draining` になった tick で
/// ここから取り出して閉じる（プロセスは動き続け、手元の run の面倒を見る）。
struct RoleState {
    role: SharedRole,
    /// `--mode verify` では `None`（`daemon_instances` に触れない）。
    supervisor: Option<instance::Supervisor>,
    api: Option<RunningApi>,
    /// ADR-0053 D1（Phase 65）: `[llm_proxy]` が有効なときだけ `Some`。
    llm_proxy: Option<RunningLlmProxy>,
    /// ADR-0056 D1（Phase 78）: `[mcp]` が有効なときだけ `Some`。
    mcp: Option<RunningMcp>,
}

/// デーモン本体。`[api]` があれば同じランタイムで HTTP API も動かし、tick ループの終了時に止める。
/// ADR-0040 D4: 起動時に `daemon_instances` を見て役割を決める（同じ `release` の `active` がいれば
/// 何もせず `Exit::DuplicateRelease`＝ exit 3）。
pub async fn run(config: Config, opts: RunOptions) -> Result<Exit, DaemonError> {
    // Phase 44（実機 2026-09-18）: `POST /reload` が `[[roles]]` / `[[genres]]` / `[delegation]` /
    // `[reports]` / `[notify]` / `[conversation]` の設定値も読み直せるよう、tick ループにはこの
    // `Config` を `&mut` で渡す（`[accounts]` / `[[clusters]]` / `[api]` / `db` / `workspace_root` は
    // 従来どおり再起動が要る。`reload_providers` がそれ以外のフィールドには触れない）。
    let mut config = config;
    let verify = opts.mode == DaemonMode::Verify;
    // ADR-0041 D5（Phase 51）: verify の celeris は `genre = "smoke"` を 1 件だけ流せる。その役割・分野・
    // プロバイダ（すべて偽のアダプタ）は**組み込みで**足す（設定ファイルに同じ id があっても上書きする）。
    if verify {
        config.apply_verify_smoke();
    }
    warn_if_db_on_network_filesystem(&config.db.path);
    // ADR-0047 D3 / D4（P-61-i、Phase 62）: 起動時に索引が無ければ作る（`_inbox` の変化を tick ごとに
    // 見る仕組みは無いが、知識整理 run が `apply_candidates` の後に必ず `reindex` するので、起動後は
    // それで追随する）。`--mode verify` では KB に触れない。
    if !verify && task_ops::knowledge::exists(&config.knowledge.root) {
        let _ = task_ops::knowledge::ensure_index(&config.knowledge.root);
    }
    let identity = InstanceIdentity::new(opts.release.as_deref());
    let cluster_masters: ClusterMasters = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let mut dispatcher = build_dispatcher(&config, Arc::clone(&cluster_masters))?;
    // ADR-0062 A（Phase 107）: 実 ssh を打つフック（実通信 probe・死んだ接続の片付け）は本番の起動経路
    // だけで配線する（`build_dispatcher` はテストからも広く呼ばれるため、そこでは配線しない）。
    wire_cluster_liveness_hooks(&mut dispatcher, Arc::clone(&cluster_masters));
    let role = SharedRole::new(if verify {
        InstanceRole::Verify
    } else {
        InstanceRole::Active
    });
    let supervisor = match verify {
        // ADR-0040 D3: verify は本番の表に触れない（そもそも DB のコピーだが、規約として）。
        true => {
            tracing::info!(
                release = %identity.release, instance_id = %identity.instance_id, smoke = config::SMOKE_ID,
                "verify mode: migrations, the API and the `smoke` genre only (no other dispatch, no background \
                 jobs, no Discord, no daemon_instances row; ADR-0040 D3 / ADR-0041 D5)"
            );
            None
        }
        false => {
            let freshness = instance::freshness_window(config.tick(), config.lease_grace_secs);
            match instance::Supervisor::start(
                dispatcher.store(),
                identity.clone(),
                role.clone(),
                freshness,
                config.drain_timeout(),
                OffsetDateTime::now_utc(),
            )? {
                instance::Started::Duplicate { instance_id, pid } => {
                    tracing::error!(
                        release = %identity.release, active_instance_id = %instance_id, active_pid = pid,
                        "another instance of the same release is already active; exiting 3 (ADR-0040 D4)"
                    );
                    return Ok(Exit::DuplicateRelease);
                }
                instance::Started::Running(supervisor) => Some(supervisor),
            }
        }
    };
    // ADR-0040 D4: `standby` は dispatch も裏方もしない（`tick` そのものを呼ばない）。`active` に
    // なったら `set_accepting_new_work(true)` で始める。
    dispatcher.set_accepting_new_work(verify || role.get() == InstanceRole::Active);
    // ADR-0041 D5: verify が面倒を見てよいのは「組み込みの分野 `smoke` で、アダプタが `fake`」のタスクだけ。
    // アダプタまで見るのは、本物の LLM を呼ぶ経路を**設定ではなく構造で**閉じるため（ADR-0041 §3）。
    // それ以外は ready のまま置かれ、リースの回収もレビューの拾い上げも起きない。
    if verify {
        dispatcher.set_eligible_tasks(Arc::new(|task: &task_core::Task| {
            task.genre.as_deref() == Some(config::SMOKE_ID)
                && task.worker_hint.adapter.as_deref() == Some(FakeAdapter::ID)
        }));
    }
    // ADR-0065 D3/D5（Phase 110a）: 背景チェックポイントと定期バックアップ。`verify` はデータのコピーに
    // 対する検証専用で背景ジョブを持たないので、そこでは起こさない（ADR-0040 D3）。
    let db_checkpoint = (!verify).then(|| {
        db_maintenance::spawn_checkpoint_task(
            config.db.path.clone(),
            config.db.checkpoint_interval(),
            config.db.busy_timeout(),
        )
    });
    let db_backup = if verify {
        None
    } else {
        config.db.backup_dir.clone().map(|backup_dir| {
            db_maintenance::spawn_backup_task(
                config.db.path.clone(),
                backup_dir,
                config.db.backup_interval(),
                config.db.backup_keep,
                config.db.busy_timeout(),
            )
        })
    };
    // ADR-0053 D1/D4（Phase 65）: 主 API（`GET /llm/sources`）とプロキシ自身が同じ `Arc` を使う。
    let llm_proxy_state = build_llm_proxy_state(&config, &dispatcher, role.clone())?;
    let (api, admin_rx) = match config.api.listen {
        Some(listen) => {
            // `standby` も起きてすぐ API を受ける（同じポートに `SO_REUSEPORT` で bind する）。
            let (api, admin_rx) = start_api(
                &config,
                listen,
                &mut dispatcher,
                &identity,
                opts.mode,
                role.clone(),
                !verify,
                llm_proxy_state.clone(),
            )
            .await?;
            (Some(api), admin_rx)
        }
        None => (None, None),
    };
    // ADR-0053 D1（Phase 65）: `standby`/`verify` も起きてすぐプロキシを受ける（主 API と同じ理由）。
    let llm_proxy = match llm_proxy_state {
        Some(state) => Some(start_llm_proxy(&config, state).await?),
        None => None,
    };
    // ADR-0056 D1（Phase 78）: `[mcp]` も同じ理由で `standby`/`verify` から受ける。
    let mcp_state = build_mcp_state(&config)?;
    let mcp = match mcp_state {
        Some(state) => Some(start_mcp(&config, state).await?),
        None => None,
    };
    let mut roles = RoleState {
        role,
        supervisor,
        api,
        llm_proxy,
        mcp,
    };
    let result = tick_loop(
        &mut dispatcher,
        &mut config,
        opts,
        admin_rx,
        cluster_masters,
        &mut roles,
    )
    .await;
    if let Some(api) = roles.api.take() {
        api.stop().await;
    }
    if let Some(llm_proxy) = roles.llm_proxy.take() {
        llm_proxy.stop().await;
    }
    if let Some(mcp) = roles.mcp.take() {
        mcp.stop().await;
    }
    // ADR-0065 D3/D5: 背景チェックポイント・定期バックアップは draining でも動き続けてよい
    // （DB への書き込みではなく、既存の WAL をさばく／バックアップするだけ）ので、プロセスが本当に
    // 終わるここで初めて止める。
    if let Some(t) = db_checkpoint {
        t.stop().await;
    }
    if let Some(t) = db_backup {
        t.stop().await;
    }
    // ADR-0040 D4: 普通に止まったときは自分の行を消す。drain で終わったときは `drained_at` を残したまま
    // にし、新しい active が掃除する（`status.sh` が引き継ぎの結果を見られるように）。
    if let Some(supervisor) = &roles.supervisor
        && !matches!(result, Ok(Exit::Drained))
    {
        supervisor.deregister();
    }
    result
}

/// tick ループ。SIGINT/SIGTERM で停止する。`admin_rx` があれば `POST /api/v1/reload` /
/// `POST /api/v1/providers/{id}/check`（ADR-0017 M2）も同じループで受ける。
async fn tick_loop(
    dispatcher: &mut Dispatcher,
    config: &mut Config,
    opts: RunOptions,
    mut admin_rx: Option<tokio::sync::mpsc::Receiver<task_api::AdminRequest>>,
    // ADR-0032 D2: celeris が張った ssh master の置き場所。ここが持っている間だけ接続が生きる。
    cluster_masters: ClusterMasters,
    // ADR-0040 D4: インスタンスの役割（`active` / `standby` / `draining` / `verify`）。
    roles: &mut RoleState,
) -> Result<Exit, DaemonError> {
    // ADR-0022 D2: `check` は spawn した先で終わるので、結果をここへ戻してスナップショットに載せる。
    let (check_tx, mut check_rx) = tokio::sync::mpsc::channel::<(String, ProviderCheckView)>(16);
    // ADR-0024 D5〜D7: アカウントの確認・ログイン中継も同様に、spawn した先の結果をここへ戻す。
    let (account_tx, mut account_rx) =
        tokio::sync::mpsc::channel::<accounts_admin::AccountAdminEvent>(16);
    let mut codex_usage_checks = accounts_admin::UsageChecks::default();
    let login_sessions = accounts_admin::new_sessions();
    // ADR-0025 D5: codex のログイン中継（別の流儀なので別のマップ）。
    let codex_login_sessions = accounts_admin::new_codex_sessions();
    // ADR-0032 D4: クラスタ接続の中継（進行中のセッションと、celeris が保持している ssh master）。
    let (cluster_tx, mut cluster_rx) =
        tokio::sync::mpsc::channel::<cluster_admin::ClusterConnectPending>(16);
    let cluster_sessions: cluster_admin::ClusterConnectSessions = Default::default();
    // ADR-0037 D3 / B1: 通知。判定はこのループの中で同期に、送信は `tokio::spawn` で（tick を止めない）。
    // 送信の結果は `notify_rx` に戻り、**次の tick の先頭**で `notifications` に書かれる。
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<notify::SendResult>(64);
    let notify_client = notify::client();
    let notify_secrets_dir = config.secrets.as_ref().map(|s| s.dir.clone());
    let notify_interval = Duration::from_secs(config.notify.interval_secs);
    let mut notify_in_flight: HashSet<task_core::NotificationId> = HashSet::new();
    let mut notify_pending: Vec<task_core::Notification> = Vec::new();
    let mut notify_last: Option<std::time::Instant> = None;
    // ADR-0037 D5（Phase 40 / 実機 2026-09-18）: backfill 禁止の基準になる celeris の起動時刻。
    let notify_started_at = OffsetDateTime::now_utc();
    // ADR-0037 D5: 429 が返っている間は次の送信を控える（`Retry-After` 秒）。
    let mut notify_blocked_until: Option<std::time::Instant> = None;
    let tick = config.tick();
    let mut ticks: u64 = 0;
    // ADR-0040 D4: 手元の run とレビューの数（drain の判定に使う。最後の tick の値）。
    let mut in_flight: usize = 0;
    tracing::info!(db = %config.db.path.display(), workspace_root = %config.workspace_root.display(), max_concurrency = config.max_concurrency, tick_ms = config.tick_ms, role = %roles.role.get(), "celeris started");

    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    // ADR-0015 D2: tick の所要時間を測り、遅い tick を警告する（止まっているのがディスパッチャか API かの切り分け用）。
    let slow_tick = std::cmp::max(Duration::from_secs(1), tick * 2);
    loop {
        // ADR-0040 D4: 役割の判断は毎 tick の**いちばん最初**に行う（`active` が引き継ぎを求められたら
        // **同じ tick で** listener を閉じ、dispatch と裏方を止めるため）。DB が一時的に読めなくても
        // デーモンは止めない（次の tick でやり直す）。
        if let Some(supervisor) = roles.supervisor.as_mut() {
            match supervisor.step(OffsetDateTime::now_utc(), in_flight) {
                Ok(instance::Step::Stay) => {}
                Ok(instance::Step::Promoted) => {
                    dispatcher.set_accepting_new_work(true);
                    tracing::info!(
                        ticks,
                        "now active: dispatching and the tick background jobs are on"
                    );
                }
                Ok(instance::Step::Draining) => {
                    // 受け付け済みの要求は完了させてから listener を閉じる（graceful shutdown）。
                    dispatcher.set_accepting_new_work(false);
                    if let Some(api) = roles.api.take() {
                        api.stop().await;
                    }
                    if let Some(llm_proxy) = roles.llm_proxy.take() {
                        llm_proxy.stop().await;
                    }
                    if let Some(mcp) = roles.mcp.take() {
                        mcp.stop().await;
                    }
                    tracing::info!(
                        ticks,
                        in_flight,
                        "draining: the API listener is closed; supervising the runs in hand"
                    );
                }
                Ok(instance::Step::Drained) => return Ok(Exit::Drained),
                Ok(instance::Step::DrainTimedOut) => {
                    let aborted = dispatcher.abort_all_runs();
                    tracing::warn!(
                        ticks,
                        aborted,
                        "drain timeout; the remaining runs were aborted"
                    );
                    return Ok(Exit::Drained);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not update the instance role this tick")
                }
            }
        }
        let role = roles.role.get();
        // ADR-0040 D3 / D4: tick の裏方（報告の圧縮・途中目標レビュー・通知）を動かすのは `active` だけ。
        // `standby` はまだ自分の番ではなく、`draining` は手元の run の面倒だけ見る。`verify` は何もしない。
        if role == InstanceRole::Active {
            codex_usage_checks.poll(config, dispatcher, account_tx.clone());
            // ADR-0024 D7 / B1: 10 分を超えたログイン中継を打ち切る（tick をブロックしない軽い処理）。
            // `expire_stale_logins` はチャネルを使わない（このループ自身が drain するチャネルへ `await` で
            // 送るとデッドロックしうるため）。打ち切った id は戻り値で受け取り、ここで直接反映する。
            for id in
                accounts_admin::expire_stale_logins(&login_sessions, accounts_admin::LOGIN_EXPIRY)
                    .await
            {
                dispatcher.set_account_login_pending(
                    task_core::AccountAdapter::ClaudeCode,
                    &id,
                    false,
                );
            }
            // ADR-0025 D5: codex も同様に 15 分で打ち切り、完了したものはポーリングで検知する（どちらもチャネルを
            // 使わない。B1 と同じ理由）。
            for id in accounts_admin::expire_stale_codex_logins(
                &codex_login_sessions,
                accounts_admin::LOGIN_EXPIRY_CODEX,
            )
            .await
            {
                dispatcher.set_account_login_pending(task_core::AccountAdapter::Codex, &id, false);
            }
            for (id, _ok) in accounts_admin::poll_codex_logins(&codex_login_sessions).await {
                dispatcher.set_account_login_pending(task_core::AccountAdapter::Codex, &id, false);
            }
            // ADR-0032 D4 / B1: 放置されたクラスタ接続のセッションも同じ規約で畳む（チャネルを使わない）。
            for id in cluster_admin::expire_stale_cluster_sessions(
                &cluster_sessions,
                cluster_admin::SESSION_EXPIRY,
            )
            .await
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
                    Err(e) => {
                        tracing::warn!(error = %e, "reports: could not schedule the compaction runs")
                    }
                }
            }
            {
                let store = dispatcher.store();
                if let Err(e) = delivery::tick(store.as_ref(), config, OffsetDateTime::now_utc()) {
                    tracing::warn!(error=%e, "delivery tick failed");
                }
            }
            // ADR-0038 D1 / B1（Phase 41）: 途中目標の仕事が止まったら、秘書の「途中目標レビュー」の対話を
            // 1 回だけ起こす（**通知の前に**）。ここも判断は決定的で、ストアを見て対話用タスクを 1 件作るだけ
            // （LLM もワーカーも起動しない。起動するのは次の tick の dispatch）。
            {
                let store = dispatcher.store();
                match milestone_review::schedule(
                    store.as_ref(),
                    &config.role_specs(),
                    &config.genre_specs(),
                    config.conversation_genre_id(),
                    OffsetDateTime::now_utc(),
                ) {
                    Ok(started) if !started.is_empty() => {
                        tracing::info!(count = started.len(), "milestone review: runs scheduled");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "milestone review: could not evaluate the milestones")
                    }
                }
            }
            // ADR-0047 D4 / B1（Phase 62）: 知識の自動メンテナンス。判断は決定的（ストアと KB のファイルを
            // 見るだけ）で、LLM が動くのは `langmem` アダプタが起こす python プロセスの中だけ。
            // 1. まだ知識整理 run を持たない終端タスクから、1 tick に最大 1 件の支援タスクを作る。
            // 2. `knowledge_runs` が `scheduled` のまま終端になった run を見つけて KB へ適用する。
            {
                let store = dispatcher.store();
                let now = OffsetDateTime::now_utc();
                let memory_dir = config
                    .memory
                    .as_ref()
                    .map(|m| task_worker::MemoryDir::new(&m.dir));
                match knowledge_maint::schedule(
                    store.as_ref(),
                    &config.knowledge.root,
                    config.knowledge.langmem.enabled,
                    notify_started_at,
                    config.knowledge.langmem.max_related_pages,
                    memory_dir.as_ref(),
                    &config.role_specs(),
                    &config.genre_specs(),
                    now,
                ) {
                    Ok(created) if !created.is_empty() => {
                        tracing::info!(
                            count = created.len(),
                            "knowledge: maintenance runs scheduled"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "knowledge: could not schedule the maintenance runs")
                    }
                }
                // ADR-0052 D3（Phase 64）: 失敗した知識整理 run を**一度だけ**作り直す（`retried_at`）。
                // 2 回目が Qwen で走るか cheap の汎用ハーネスで走るかは dispatch 時の到達性の検査が決める。
                match knowledge_maint::retry_failed(
                    store.as_ref(),
                    &config.knowledge.root,
                    config.knowledge.langmem.enabled,
                    config.knowledge.langmem.max_related_pages,
                    memory_dir.as_ref(),
                    &config.role_specs(),
                    &config.genre_specs(),
                    now,
                ) {
                    Ok(retried) if !retried.is_empty() => {
                        tracing::info!(
                            count = retried.len(),
                            "knowledge: failed maintenance runs retried (once)"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "knowledge: could not retry the failed maintenance runs")
                    }
                }
                if config.knowledge.langmem.enabled {
                    match knowledge_maint::apply_finished(
                        store.as_ref(),
                        &config.knowledge.root,
                        &config.workspace_root,
                        now,
                    ) {
                        Ok(applied) if applied > 0 => {
                            tracing::info!(count = applied, "knowledge: maintenance runs applied");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "knowledge: could not apply finished maintenance runs")
                        }
                    }
                }
            }
            // ADR-0037 D1/D3 / B1: 通知。ここもチャネルには送らず、その場で store を見るだけ（LLM もワーカーも
            // 起動しない）。実際の POST だけが `tokio::spawn` の先で走る。
            {
                let store = dispatcher.store();
                let now = OffsetDateTime::now_utc();
                // 1. 前の tick で spawn した送信の結果を書く。429 は attempts に数えず、
                //    `Retry-After` の間だけ次の送信を控える（ADR-0037 D5）。
                while let Ok(result) = notify_rx.try_recv() {
                    for id in &result.ids {
                        notify_in_flight.remove(id);
                    }
                    if let notify::SendOutcome::RateLimited(wait) = &result.outcome {
                        notify_blocked_until = Some(std::time::Instant::now() + *wait);
                    }
                    if let Err(e) = notify::record(store.as_ref(), &notify_pending, &result, now) {
                        tracing::warn!(error = %e, "notify: could not record the send result");
                    }
                }
                // 2. `interval_secs` ごとに判定し、1 tick に最大 1 通だけ送る（ADR-0037 D5）。
                let due = notify_last
                    .map(|t| t.elapsed() >= notify_interval)
                    .unwrap_or(true)
                    && notify_blocked_until
                        .map(|t| std::time::Instant::now() >= t)
                        .unwrap_or(true);
                if due {
                    notify_last = Some(std::time::Instant::now());
                    match notify::schedule(store.as_ref(), &config.notify, notify_started_at, now) {
                        Ok(created) if !created.is_empty() => {
                            tracing::info!(count = created.len(), "notify: new notifications");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "notify: could not evaluate the conditions")
                        }
                    }
                    match store.notification_pending() {
                        Ok(pending) => {
                            let url = notify::webhook_url(
                                notify_secrets_dir.as_deref(),
                                &config.notify.discord_webhook_secret,
                            );
                            match (url, notify_client.as_ref()) {
                                (Some(url), Some(client)) => {
                                    let available: Vec<task_core::Notification> = pending
                                        .iter()
                                        .filter(|n| !notify_in_flight.contains(&n.id))
                                        .cloned()
                                        .collect();
                                    if let Some(batch) = notify::select_batch(&available) {
                                        for id in &batch.ids {
                                            notify_in_flight.insert(*id);
                                        }
                                        notify::spawn_send(
                                            client.clone(),
                                            url.clone(),
                                            &batch,
                                            notify_tx.clone(),
                                        );
                                    }
                                }
                                // ADR-0037 D2: 秘密が無い間は送らず、pending も溜めない。
                                _ => {
                                    let idle: Vec<_> = pending
                                        .iter()
                                        .filter(|n| !notify_in_flight.contains(&n.id))
                                        .cloned()
                                        .collect();
                                    match notify::discard_pending(store.as_ref(), &idle, now) {
                                        Ok(n) if n > 0 => tracing::debug!(
                                            count = n,
                                            "notify: no webhook secret; nothing was sent"
                                        ),
                                        Ok(_) => {}
                                        Err(e) => {
                                            tracing::warn!(error = %e, "notify: could not discard the pending rows")
                                        }
                                    }
                                }
                            }
                            notify_pending = pending;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "notify: could not read the pending rows")
                        }
                    }
                }
            }
        }
        // ADR-0040 D3 / D4: `active` と `draining` が `Dispatcher::tick` を回す（`draining` は
        // `set_accepting_new_work(false)` により新しい run を起こさず、手元の run の完了・リース更新・
        // 後処理だけを行う）。`standby` はディスパッチャを一切回さない（＝ ready なタスクを拾わない、
        // ワーカーを起こさない、リースを奪わない）。ADR-0041 D5: `verify` は回すが、面倒を見るのは
        // `genre = "smoke"` かつアダプタが `fake` のタスクだけ（`set_eligible_tasks`）。
        let tick_started = std::time::Instant::now();
        let report: TickReport = match role {
            // ADR-0041 D5: `verify` も tick を回すが、`set_eligible_tasks` で `smoke` の煙試験だけに
            // 絞られている（他の ready なタスクは拾わない・リースも奪わない・レビューもしない）。
            InstanceRole::Active | InstanceRole::Draining | InstanceRole::Verify => {
                dispatcher.tick()?
            }
            InstanceRole::Standby => TickReport::default(),
        };
        in_flight = report.in_flight;
        let tick_elapsed = tick_started.elapsed();
        ticks += 1;
        if tick_elapsed >= slow_tick {
            tracing::warn!(
                ticks,
                duration_ms = tick_elapsed.as_millis() as u64,
                ?report,
                "slow tick"
            );
        }
        if report.reclaimed + report.dispatched + report.finished + report.reviewed > 0 {
            tracing::info!(ticks, ?report, "tick");
        } else {
            tracing::debug!(ticks, %role, ?report, "tick");
        }
        // `standby` は `report.idle` を計算していないので `until_idle` では止まらない。
        if opts.until_idle
            && report.idle
            && matches!(
                role,
                InstanceRole::Active | InstanceRole::Draining | InstanceRole::Verify
            )
        {
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
    config: &mut Config,
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
                // ADR-0022 D2: 確認できたときだけ記録する（設定エラー・celeris 側の都合は「確認の結果」ではない）。
                if let Ok(outcome) = &outcome {
                    let check = ProviderCheckView {
                        at: OffsetDateTime::now_utc()
                            .format(&Rfc3339)
                            .unwrap_or_default(),
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
            accounts_admin::spawn_login_start(
                config,
                login_sessions,
                codex_login_sessions,
                adapter,
                id,
                account_tx,
                reply,
            );
        }
        task_api::AdminRequest::AccountLoginCode { id, code, reply } => {
            accounts_admin::spawn_login_code(login_sessions, id, code, account_tx, reply);
        }
        task_api::AdminRequest::AccountLoginCancel { adapter, id, reply } => {
            accounts_admin::spawn_login_cancel(
                login_sessions,
                codex_login_sessions,
                adapter,
                id,
                account_tx,
                reply,
            );
        }
        // ADR-0032 D5: クラスタ接続の中継。どれも `tokio::spawn` するので tick を止めない。
        task_api::AdminRequest::ClusterConnectStart { id, reply } => {
            cluster_admin::spawn_connect_start(
                config,
                cluster_sessions,
                cluster_masters,
                id,
                cluster_tx,
                reply,
            );
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
            cluster_admin::spawn_connect_cancel(
                config,
                cluster_sessions,
                cluster_masters,
                id,
                cluster_tx,
                reply,
            );
        }
        // S2+S8: cheap な fs 操作（ディレクトリの rename）だけなので spawn せず、ここで直接（同期的に）行う。
        // ディスパッチャの権威ある `account_in_use` を使うため `&mut Dispatcher` が要る。
        task_api::AdminRequest::AccountRemove { adapter, id, reply } => {
            let result = accounts_admin::remove_account(
                config,
                dispatcher,
                &login_sessions,
                &codex_login_sessions,
                adapter,
                &id,
            )
            .await;
            if result.is_ok() {
                tracing::info!(who = "admin", op = "account_remove", account_id = %id, %adapter, "admin: account removed");
            } else {
                tracing::warn!(who = "admin", op = "account_remove", account_id = %id, %adapter, ?result, "account remove rejected");
            }
            let _ = reply.send(result);
        }
        // ADR-0037 D4: テスト送信。秘密を読むのも POST するのも celeris 側（task-api は URL を知らない）。
        // `spawn` するので tick は止まらない（B1）。
        task_api::AdminRequest::NotifyTest { reply } => {
            let secrets_dir = config.secrets.as_ref().map(|s| s.dir.clone());
            let secret_id = config.notify.discord_webhook_secret.clone();
            let client = notify::client();
            tokio::spawn(async move {
                let result =
                    notify::send_test(client.as_ref(), secrets_dir.as_deref(), &secret_id).await;
                let _ = reply.send(notify_test_outcome(result));
            });
        }
    }
}

/// ADR-0037 D4: `notify::send_test` の結果を API の型へ写す（秘密が無ければ 409 になる `Unavailable`）。
fn notify_test_outcome(
    result: notify::TestSend,
) -> Result<task_api::NotifyTestOutcome, task_api::NotifyAdminError> {
    match result {
        notify::TestSend::Sent => Ok(task_api::NotifyTestOutcome {
            ok: true,
            detail: Some("the test message was delivered".to_string()),
        }),
        notify::TestSend::Failed(detail) => Ok(task_api::NotifyTestOutcome {
            ok: false,
            detail: Some(detail),
        }),
        notify::TestSend::NotConfigured(detail) => {
            Err(task_api::NotifyAdminError::Unavailable(detail))
        }
    }
}

/// `Config::load` を読み直し、稼働中のプロバイダ選定・アダプタ一式・次 tick のスナップショット提供元、
/// および `[[roles]]` / `[[genres]]` / `[delegation]` / `[reports]` / `[notify]` / `[conversation]` の
/// 設定値を差し替える（Phase 44、実機 2026-09-18: `[[roles]] implementer` の `max_turns` を変えて
/// reload しても、以前はプロバイダ・アダプタ・モデルしか差し替えなかったため、委譲された子の budget が
/// 古い値のままだった）。失敗したら稼働中の状態には触れない（古い設定のまま動き続ける）。
///
/// S7: `[accounts]` は reload の対象外（`Dispatcher::accounts` はプロセス起動時に固定され、`AccountBook` の
/// 保存先もそこから決まる）。`claude_dir` / `max_runs_per_account` / `check_model` のどれかが変わっていたら、
/// 反映されない値のまま動き続けるより、エラーにしてタスクを止めずに知らせる（400。再起動が必要と伝える）。
/// `[[clusters]]` / `[api]` / `db` / `workspace_root` も同様に再起動が要る（reload では触れない。従来どおり）。
fn reload_providers(dispatcher: &mut Dispatcher, config: &mut Config) -> Result<(), String> {
    let path = config
        .source_path
        .clone()
        .ok_or_else(|| "config was not loaded from a file; cannot reload".to_string())?;
    let new_config = Config::load(&path).map_err(|e| e.to_string())?;
    if accounts_section_changed(&config.accounts, &new_config.accounts) {
        return Err(
            "[accounts] changed (claude_dir / max_runs_per_account / check_model); \
             this section is not reloaded, restart celeris to apply the change"
                .to_string(),
        );
    }
    let policy = StaticPolicy::new(
        new_config.provider_specs(),
        Duration::from_secs(new_config.error_cooldown_secs),
    );
    let adapters = build_adapters(&new_config);
    let models = effective_models(&new_config);
    dispatcher.reload_providers(
        Box::new(policy),
        models,
        adapters,
        new_config.account_pool_providers(),
    );
    dispatcher.set_snapshot_providers(provider_lives(&new_config));
    // Phase 44: 役割・分野・委譲設定はディスパッチャ側（次に起動する run から効く）。
    dispatcher.reload_config(
        new_config.role_specs(),
        new_config.genre_specs(),
        new_config.delegation_limits(),
    );
    // Phase 44: `[reports]` / `[notify]` / `[conversation]` は celeris の tick ループが直接読むので、
    // ここで `config` 自身を更新する（次 tick から効く）。他のフィールド（`[accounts]` / `[[clusters]]` /
    // `[api]` / `db` / `workspace_root` 等）には触れない。
    config.roles = new_config.roles;
    config.genres = new_config.genres;
    config.delegation = new_config.delegation;
    config.reports = new_config.reports;
    config.notify = new_config.notify;
    config.conversation = new_config.conversation;
    config.selfdeploy.delivery_projects = new_config.selfdeploy.delivery_projects;
    dispatcher.set_delivery_policy(task_ops::delivery::DeliveryPolicy {
        projects: config.selfdeploy.delivery_projects.clone(),
        repo: config.selfdeploy.repo.clone(),
    });
    Ok(())
}

/// S7: `claude_dir` / `max_runs_per_account` / `check_model` のどれかが変わっていれば `true`
/// （`None` ⇔ `Some` の変化も含む）。
fn accounts_section_changed(
    old: &Option<config::AccountsConfig>,
    new: &Option<config::AccountsConfig>,
) -> bool {
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
    let path = config_path.ok_or_else(|| {
        task_api::CheckError::Unavailable("config was not loaded from a file; cannot check".into())
    })?;
    let config =
        Config::load(&path).map_err(|e| task_api::CheckError::ConfigInvalid(e.to_string()))?;
    if !config.providers.iter().any(|p| p.id == provider_id) {
        return Err(task_api::CheckError::NotFound);
    }
    let adapters = build_adapters(&config);
    let adapter = adapters
        .get(&provider_id)
        .ok_or(task_api::CheckError::NotFound)?
        .clone();

    let dir = std::env::temp_dir().join(format!("celeris-provider-check-{}", ulid::Ulid::new()));
    let now = OffsetDateTime::now_utc();
    let task = task_core::Task {
        repos: Vec::new(),
        id: task_core::TaskId::new(),
        parent_id: None,
        kind: task_core::TaskKind::Execute,
        title: "provider check".into(),
        objective: "Reply with a short confirmation that you are ready. Do not change any files."
            .into(),
        acceptance: vec![task_core::Criterion {
            text: "reply".into(),
            check: task_core::Check::Human,
        }],
        inputs: vec![],
        depends_on: vec![],
        status: task_core::Status::Ready,
        priority: 0,
        worker_hint: task_core::WorkerHint {
            tier: task_core::Tier::Standard,
            adapter: None,
        },
        workspace: task_core::WorkspaceSpec::Local {
            path: dir.clone(),
            mode: None,
        },
        // ADR-0022 M1: 1 ターンではワーカープロトコル（`artifacts/result.json` を書く）を完了できず、
        // 健全なアカウントでも `error_max_turns` になる。人が読む信号にするため少しだけ余裕を持たせる。
        budget: task_core::Budget {
            max_turns: 3,
            max_wall_secs: 30,
            max_retries: 0,
        },
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
        skills: Vec::new(),
        mode: task_core::TaskMode::default(),
        labels: Vec::new(),
        category: Default::default(),
    };
    let prepared = task_worker::LocalWorkspace::new(dir.clone())
        .prepare(&task)
        .await
        .map_err(|e| {
            task_api::CheckError::Unavailable(format!("failed to prepare check workspace: {e}"))
        })?;
    let run_id = task_core::TaskId::new().to_string();
    // ADR-0036 D1: 疎通確認用の単独タスク（親なし）なので従来どおり `<workspace>/artifacts`。
    let artifacts_dir = task_core::artifacts::artifacts_dir_for(&task, &prepared);
    let req = task_worker::RunRequest {
        protocol: task_worker::PROTOCOL_VERSION,
        task,
        workspace: prepared,
        // 疎通確認は celeris 自身が作った使い捨てのディレクトリで動かす（worktree は関係しない）。
        work_dir: None,
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
    let result = adapter
        .run(req, &run_id, limits, &task_worker::adapter::NullSink)
        .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    // ADR-0022 M1（実機確認で修正）: 見ているのは「このアカウントで CLI が起動して応答するか」だけ。
    // ワーカープロトコル上のエラー（`Terminal::Error`。1 ターンでは result.json を書けない等）は
    // **アカウントの問題ではない**ので `ok` とし、理由を `detail` に残す。起動できない・認証切れ・
    // 枯渇は `AdapterError` 側で分かる。
    Ok(match result {
        Ok(outcome) => match outcome.terminal {
            task_worker::Terminal::Done { summary, .. } => {
                (task_api::ProviderCheckResult::Ok, Some(summary))
            }
            task_worker::Terminal::Question { text } => {
                (task_api::ProviderCheckResult::Ok, Some(text))
            }
            task_worker::Terminal::Error { message, .. } => {
                (task_api::ProviderCheckResult::Ok, Some(message))
            }
        },
        Err(e @ task_worker::AdapterError::AuthFailed(_)) => (
            task_api::ProviderCheckResult::AuthFailed,
            Some(e.to_string()),
        ),
        Err(
            e @ (task_worker::AdapterError::Throttled { .. }
            | task_worker::AdapterError::Exhausted(_)),
        ) => (
            task_api::ProviderCheckResult::Throttled,
            Some(e.to_string()),
        ),
        Err(e) => (
            task_api::ProviderCheckResult::SpawnFailed,
            Some(e.to_string()),
        ),
    })
    .map(|(result, detail)| task_api::ProviderCheckOutcome {
        result,
        detail: detail.map(|d| truncate_detail(&d)),
    })
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
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!(
                "db = \"celeris.sqlite3\"\nworkspace_root = \"ws\"\norg_include = \"org.toml\"\n{}",
                r#"
[[providers]]
id = "x"
adapter = "fake"

[[roles]]
id = "implementer"

[[roles]]
id = "literature-reader"

[[roles]]
id = "cos-role"

[[genres]]
id = "conversation"
description = "人と話す"
default_role = "cos-role"
roles = ["cos-role"]

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

[[roles]]
id = "data-analyst"

[[genres]]
id = "data-analysis"
description = "データを整える"
default_role = "data-analyst"
roles = ["data-analyst"]

[[roles]]
id = "writer"

[[genres]]
id = "writing"
description = "書く"
default_role = "writer"
roles = ["writer"]

[conversation]
genre = "conversation"
"#
            ),
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        let store = SqliteStore::open(&config.db.path).unwrap();

        assert_eq!(seed_org_if_empty(&store, &config).unwrap(), 13);
        let nodes = store.org_list().unwrap();
        assert_eq!(nodes.len(), 13);
        let cos = nodes.iter().find(|n| n.id == "cos").unwrap();
        assert_eq!(cos.kind, task_core::OrgKind::Secretary);
        assert_eq!(cos.parent_id, None);
        assert_eq!(
            nodes
                .iter()
                .find(|n| n.id == "software-engineering")
                .unwrap()
                .genre
                .as_deref(),
            Some("coding")
        );

        // 人が GUI で名前を変えても、次の起動で設定に戻されない。
        let mut renamed = cos.clone();
        renamed.name = "本人".into();
        store.org_upsert(&renamed).unwrap();
        assert_eq!(seed_org_if_empty(&store, &config).unwrap(), 0);
        assert_eq!(store.org_get("cos").unwrap().unwrap().name, "本人");
        assert_eq!(store.org_list().unwrap().len(), 13);
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
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "db = \"celeris.sqlite3\"\nworkspace_root = \"ws\"\norg_include = \"org.toml\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        let store = SqliteStore::open(&config.db.path).unwrap();

        let err = seed_org_if_empty(&store, &config).unwrap_err();
        assert!(err.to_string().contains("placed under"), "{err}");
        assert!(
            store.org_list().unwrap().is_empty(),
            "nothing is written on failure"
        );
    }

    /// `org_include` が無い設定では何も蒔かない。
    #[test]
    fn without_org_include_nothing_is_seeded() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("celeris.sqlite3");
        let config: Config = toml::from_str(&format!(
            "db = \"{}\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
            db.display()
        ))
        .unwrap();
        let store = SqliteStore::open(&config.db.path).unwrap();
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
        assert_eq!(
            a,
            vec![
                ("CLAUDE_CONFIG_DIR".into(), "/accounts/a".into()),
                ("SHARED".into(), "base".into())
            ]
        );
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
        assert_eq!(
            (
                lives[0].adapter.as_str(),
                lives[0].concurrency,
                lives[0].in_use
            ),
            ("claude-code", 1, 0)
        );
        assert!(!hostname().is_empty());

        // ADR-0015 D3: マウント点の最長一致でファイルシステム種別を引く。
        let mountinfo = "\
25 30 0:24 / / rw,relatime shared:1 - ext4 /dev/mapper/root rw
26 25 0:52 / /home rw,relatime shared:2 - nfs4 server:/home rw,vers=4.2
27 26 0:53 / /home/u/local rw,relatime shared:3 - ext4 /dev/sdb1 rw";
        assert_eq!(
            filesystem_type_in(mountinfo, Path::new("/var/lib/celeris")).as_deref(),
            Some("ext4")
        );
        assert_eq!(
            filesystem_type_in(mountinfo, Path::new("/home/u/workspace")).as_deref(),
            Some("nfs4")
        );
        // 同じマウント点に autofs と実体が並ぶ場合は後の行（実体）を採る。
        let autofs_first = "\
25 30 0:24 / / rw,relatime shared:1 - ext4 /dev/mapper/root rw
26 25 0:51 / /home rw,relatime shared:2 - autofs systemd-1 rw
27 25 0:52 / /home rw,relatime shared:3 - nfs4 server:/home rw,vers=4.2";
        assert_eq!(
            filesystem_type_in(autofs_first, Path::new("/home/u/x")).as_deref(),
            Some("nfs4")
        );
        assert_eq!(
            filesystem_type_in(mountinfo, Path::new("/home/u/local/db")).as_deref(),
            Some("ext4")
        );
        assert_eq!(filesystem_type_in("garbage", Path::new("/home")), None);

        // ADR-0013 D11: /config の要約には env のキー名だけが載り、値は載らない。
        let view = config_view(&cfg, "127.0.0.1:7710".parse().unwrap());
        assert_eq!(view.providers[0].env_keys, ["CLAUDE_CONFIG_DIR"]);
        assert_eq!(
            view.providers[1].model.as_deref(),
            Some("adapter-default-model")
        );
        assert_eq!(
            (view.api.bind.as_str(), view.api.auth_required),
            ("127.0.0.1:7710", false)
        );
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
output_artifacts = ["answer.md: 引用付きの答え", "citations.json"]
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
        assert_eq!(
            view.genres[0].capabilities,
            vec![
                "academic literature search".to_string(),
                "citation graph traversal".to_string()
            ]
        );
        assert_eq!(
            view.genres[0].input_artifacts,
            vec!["question".to_string(), "pdf".to_string()]
        );
        // Phase 38（ADR-0028 追記）: `名前: 説明` を書いても `GenreConfigView` の型は変わらず、値の文字列に
        // 説明が付くだけ（GUI は `:` の前を名前として扱う。`docs/gui/api.md`）。
        assert_eq!(
            view.genres[0].output_artifacts,
            vec![
                "answer.md: 引用付きの答え".to_string(),
                "citations.json".to_string()
            ]
        );
        let json = serde_json::to_value(&view.genres[0]).unwrap();
        assert_eq!(json["capabilities"][0], "academic literature search");
        assert_eq!(json["output_artifacts"][0], "answer.md: 引用付きの答え");
        assert_eq!(json["output_artifacts"][1], "citations.json");
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
                (
                    "OPENCODE_DISABLE_PROJECT_CONFIG".to_string(),
                    "1".to_string()
                ),
                ("SHARED".to_string(), "base".to_string()),
            ]
        );

        // 行の command/args が [adapters.acp] の既定（opencode/["acp"]）を上書きする。
        assert_eq!(cfg.providers[1].command.as_deref(), Some("goose"));
        assert_eq!(
            cfg.providers[1].args.as_deref(),
            Some(&["acp".to_string()][..])
        );
    }

    /// ADR-0061: `aider` プロバイダの行は `[adapters.aider]` の env に重ね、`model` は行の値が
    /// `[adapters.aider]` の既定を上書きする（`codex` と同じ規則）。
    #[test]
    fn build_adapters_wires_an_aider_provider_with_merged_env_and_row_model() {
        let text = r#"
[adapters.aider]
env = { SHARED = "base" }
model = "anthropic/claude-haiku-4"

[[providers]]
id = "aider-1"
adapter = "aider"
tiers = ["standard"]
model = "anthropic/claude-sonnet-5"
env = { ANTHROPIC_API_KEY = "sk-x" }
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters["aider-1"].id(), "aider");

        let models = effective_models(&cfg);
        assert_eq!(models["aider-1"], "anthropic/claude-sonnet-5");

        let merged = merged_env(&cfg.adapters.aider.env, &cfg.providers[0].env);
        assert_eq!(
            merged,
            vec![
                ("ANTHROPIC_API_KEY".to_string(), "sk-x".to_string()),
                ("SHARED".to_string(), "base".to_string()),
            ]
        );
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
                (
                    "OPENAI_BASE_URL".to_string(),
                    "http://127.0.0.1:18000/v1".to_string()
                ),
                ("SHARED".to_string(), "base".to_string()),
            ]
        );

        // 行の settings が [adapters.paperqa] の既定を上書きする。上書きしない行は共通設定のまま。
        assert_eq!(
            cfg.providers[0].settings.as_deref(),
            Some("/opt/paperqa/settings/qwen-local")
        );
        assert!(cfg.providers[1].settings.is_none());
        assert_eq!(
            cfg.adapters.paperqa.settings.as_deref(),
            Some("/opt/paperqa/settings/base")
        );
    }

    /// ADR-0029 D1: `local-deep-research` プロバイダの行は `[adapters.local_deep_research]` の env に重ね、
    /// `model` は行の値がそのまま（`[adapters.local_deep_research]` にモデルの既定値は無い。`acp`/`paperqa`
    /// と同じ理由）。行ごとの `settings` の上書きは無い（celeris 側の実装判断。`ProviderConfig.settings` は
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
                (
                    "OPENAI_BASE_URL".to_string(),
                    "http://127.0.0.1:18000/v1".to_string()
                ),
                ("SHARED".to_string(), "base".to_string()),
            ]
        );

        assert_eq!(
            cfg.adapters
                .local_deep_research
                .settings
                .get("llm.provider")
                .map(String::as_str),
            Some("openai_endpoint")
        );
        assert_eq!(
            cfg.adapters.local_deep_research.mode,
            task_worker::LdrMode::Detailed
        );
        assert_eq!(cfg.adapters.local_deep_research.iterations, Some(3));
    }

    /// ADR-0047 D4: `[adapters.langmem]`（起動コマンド）と `[knowledge.langmem]`（LLM の接続先）を
    /// 合わせて 1 つの `LangMemConfig` にする。`api_key_secret` は `[secrets] dir` から解決される。
    #[test]
    fn build_adapters_wires_a_langmem_provider_from_both_config_sections() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let secrets_dir = dir.path().join("secrets");
        std::fs::create_dir_all(&secrets_dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(secrets_dir.join("langmem-key"), "sk-test-value\n")
            .unwrap_or_else(|e| panic!("write: {e}"));
        let text = format!(
            r#"
[secrets]
dir = "{secrets}"

[adapters.langmem]
command = "/opt/langmem/.venv/bin/python"
idle_timeout_secs = 120
env = {{ SHARED = "base" }}

[knowledge.langmem]
enabled = true
provider = "openai-compatible"
base_url = "http://bnode150:18000/v1"
model = "qwen3.8-27b"
api_key_secret = "langmem-key"
max_related_pages = 5

[[providers]]
id = "langmem-main"
adapter = "langmem"
tiers = ["cheap", "standard"]
"#,
            secrets = secrets_dir.display()
        );
        let cfg: Config = toml::from_str(&text).unwrap_or_else(|e| panic!("parse: {e}"));
        cfg.validate().unwrap_or_else(|e| panic!("validate: {e}"));
        assert!(cfg.knowledge.langmem.enabled);
        assert_eq!(cfg.knowledge.langmem.max_related_pages, 5);
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters["langmem-main"].id(), "langmem");

        let usage = secret_usage(&cfg);
        assert!(
            usage["langmem-key"]
                .iter()
                .any(|u| u.scope == "adapter" && u.name == "langmem" && u.env == "api_key"),
            "{usage:?}"
        );
    }

    /// `[knowledge.langmem]` の既定は無効（`enabled = false`）で、`base_url`/`model`/`api_key_secret` は
    /// 無い（ADR-0047 D4 §6: 明示的に有効化するまで知識整理 run は起きない）。
    #[test]
    fn langmem_knowledge_config_defaults_to_disabled() {
        let cfg: Config =
            toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n").unwrap();
        assert!(!cfg.knowledge.langmem.enabled);
        assert_eq!(
            cfg.knowledge.langmem.provider,
            task_worker::LangMemProvider::OpenaiCompatible
        );
        assert!(cfg.knowledge.langmem.base_url.is_none());
        assert!(cfg.knowledge.langmem.model.is_none());
        assert!(cfg.knowledge.langmem.api_key_secret.is_none());
        assert_eq!(cfg.knowledge.langmem.max_related_pages, 10);
        assert_eq!(cfg.adapters.langmem.command, "python3");
        assert!(cfg.adapters.langmem.idle_timeout_secs.is_none());
    }

    /// 未知のアダプタは `langmem` を含めた既知の一覧で拒否される。
    #[test]
    fn unknown_adapter_message_lists_langmem() {
        let cfg: Config =
            toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"bogus\"\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("langmem"), "{err}");
    }

    // ---- ADR-0030 D2: `env_from_secrets` の優先順と欠落時の扱い ----

    /// 優先順は `[adapters.*].env` < `[adapters.*].env_from_secrets` < 行の `env` < 行の `env_from_secrets`
    /// （celeris 自身の環境はプロセス継承なのでここでは扱わない）。秘密が見つからない層はそのキーに触れず、
    /// 下の層の値が残る。
    #[test]
    fn merged_env_with_secrets_follows_the_precedence_order_and_falls_back_when_a_secret_is_missing()
     {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let secrets_dir = dir.path().join("secrets");
        std::fs::create_dir_all(&secrets_dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(secrets_dir.join("id-base"), "base-secret\n")
            .unwrap_or_else(|e| panic!("write: {e}"));
        std::fs::write(secrets_dir.join("id-row"), "row-secret")
            .unwrap_or_else(|e| panic!("write: {e}"));
        // `id-row-missing` はわざと作らない（欠落を再現する）。

        let base_env = HashMap::from([
            ("K".to_string(), "base-env".to_string()),
            ("ONLY_BASE".to_string(), "b".to_string()),
        ]);
        let base_secrets = HashMap::from([("K".to_string(), "id-base".to_string())]);
        let row_env = HashMap::from([("K".to_string(), "row-env".to_string())]);
        let row_secrets = HashMap::from([("K".to_string(), "id-row".to_string())]);

        // 全層が揃っていれば行の env_from_secrets が勝つ。
        let merged = merged_env_with_secrets(
            &base_env,
            &base_secrets,
            &row_env,
            &row_secrets,
            Some(&secrets_dir),
        );
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"row-secret".to_string()));
        assert_eq!(map.get("ONLY_BASE"), Some(&"b".to_string()));

        // 行の env_from_secrets の秘密が無ければ、そのキーには触れず 1 段下（行の env）が残る。
        let row_secrets_missing = HashMap::from([("K".to_string(), "id-row-missing".to_string())]);
        let merged = merged_env_with_secrets(
            &base_env,
            &base_secrets,
            &row_env,
            &row_secrets_missing,
            Some(&secrets_dir),
        );
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"row-env".to_string()));

        // 行の env も無ければ、その下（`[adapters.*].env_from_secrets`）が残る。
        let empty: HashMap<String, String> = HashMap::new();
        let merged = merged_env_with_secrets(
            &base_env,
            &base_secrets,
            &empty,
            &row_secrets_missing,
            Some(&secrets_dir),
        );
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(map.get("K"), Some(&"base-secret".to_string()));

        // `[secrets]` 自体が未設定（`secrets_dir: None`）なら env_from_secrets は何も足さない（設定エラーにしない）。
        let merged =
            merged_env_with_secrets(&base_env, &base_secrets, &row_env, &row_secrets, None);
        let map: HashMap<String, String> = merged.into_iter().collect();
        assert_eq!(
            map.get("K"),
            Some(&"row-env".to_string()),
            "missing [secrets] falls back to the env layer, not an error"
        );

        // 末尾の改行は読み取り時に落ちる。
        let base_secrets_only = HashMap::from([("K".to_string(), "id-base".to_string())]);
        let merged = merged_env_with_secrets(
            &empty,
            &base_secrets_only,
            &empty,
            &empty,
            Some(&secrets_dir),
        );
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
        assert!(tavily.iter().any(|u| u.scope == "adapter"
            && u.name == "local-deep-research"
            && u.env == "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"));
        assert!(tavily.iter().any(|u| u.scope == "provider"
            && u.name == "ldr-tavily"
            && u.env == "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"));
        let exa = usage.get("exa").expect("exa uses");
        assert_eq!(exa.len(), 2);
        assert!(
            exa.iter()
                .any(|u| u.scope == "adapter" && u.name == "local-deep-research")
        );
        assert!(
            exa.iter()
                .any(|u| u.scope == "provider" && u.name == "ldr-exa")
        );

        // 未参照の id は現れない。
        assert!(!usage.contains_key("unused"));

        // `env_from_secrets` を書かない設定は空のまま。
        let plain: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"fake\"\n")
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(secret_usage(&plain).is_empty());
    }

    /// `api_settings` は `[secrets] dir` と `secret_usage` を `ApiSettings` に写す。
    #[test]
    fn api_settings_carries_secrets_dir_and_usage() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[secrets]\ndir = \"secrets\"\n\n[adapters.local_deep_research]\nenv_from_secrets = { TAVILY = \"tavily\" }\n\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let cfg = Config::load(&path).unwrap_or_else(|e| panic!("{e}"));
        let settings = api_settings(
            &cfg,
            "127.0.0.1:7710".parse().unwrap_or_else(|e| panic!("{e}")),
            None,
            "i".into(),
            "t".into(),
            None,
            "sha12sha12ab".into(),
            DaemonMode::Verify,
            SharedRole::new(InstanceRole::Standby),
            None,
        );
        assert_eq!(
            settings.secrets_dir,
            cfg.secrets.as_ref().map(|s| s.dir.clone())
        );
        assert!(settings.secret_usage.contains_key("tavily"));
        // ADR-0040 D3 / D4: `release` / `mode` / `role` はそのまま API へ渡る（`GET /health` に出る）。
        assert_eq!(settings.release, "sha12sha12ab");
        assert_eq!(settings.mode, DaemonMode::Verify);
        assert_eq!(settings.role.get(), InstanceRole::Standby);
    }

    /// S7: `[accounts]` は reload の対象外。`claude_dir` / `max_runs_per_account` / `check_model` のどれかが
    /// 変わっていたら `reload` はエラー（400 に写る文字列）を返し、稼働中の状態には触れない。
    #[test]
    fn reload_providers_rejects_changes_to_the_accounts_section() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let accounts_dir = dir.path().join("accounts");
        std::fs::create_dir_all(&accounts_dir).unwrap_or_else(|e| panic!("{e}"));
        let config_path = dir.path().join("config.toml");
        let db = dir.path().join("celeris.db");
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
        let mut config = Config::load(&config_path).unwrap_or_else(|e| panic!("{e}"));
        let mut dispatcher =
            build_dispatcher(&config, Default::default()).unwrap_or_else(|e| panic!("{e}"));

        // [accounts] が変わっていなければ通る。
        assert!(reload_providers(&mut dispatcher, &mut config).is_ok());

        // max_runs_per_account を変えると、次の reload はエラーになる。
        write_config(3);
        let err = reload_providers(&mut dispatcher, &mut config).unwrap_err();
        assert!(err.contains("[accounts]"), "{err}");
        assert!(err.contains("restart"), "{err}");
    }

    /// Phase 44（実機 2026-09-18）: `[[roles]]` の `max_turns` を変えて `reload` すると、次に作られる子の
    /// budget が新しい値になる（`Dispatcher::config().roles` に反映される。委譲の子は `spawn_worker` の
    /// 時点でこの写しを使う）。`[reports]` / `[notify]` / `[conversation]` も同様に `Config` 自身へ反映する。
    #[test]
    fn reload_rereads_roles_genres_delegation_and_reports_notify_conversation() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let config_path = dir.path().join("config.toml");
        let db = dir.path().join("celeris.db");
        let ws = dir.path().join("ws");
        let write_config = |max_turns: u32, notify_interval: u64| {
            std::fs::write(
                &config_path,
                format!(
                    "db = {db:?}\nworkspace_root = {ws:?}\n\
                     [[providers]]\nid = \"x\"\nadapter = \"fake\"\n\
                     [[roles]]\nid = \"implementer\"\nmax_turns = {max_turns}\n\
                     [delegation]\nmax_delegate_per_run = 3\n\
                     [notify]\ninterval_secs = {notify_interval}\n"
                ),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        };
        write_config(20, 30);
        let mut config = Config::load(&config_path).unwrap_or_else(|e| panic!("{e}"));
        let mut dispatcher =
            build_dispatcher(&config, Default::default()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dispatcher.config().roles[0].max_turns, Some(20));
        assert_eq!(dispatcher.config().delegation.max_delegate_per_run, 3);

        // `[[roles]] implementer` の `max_turns` を 20 → 60、`[delegation]` と `[notify]` も変える。
        write_config(60, 90);
        assert!(reload_providers(&mut dispatcher, &mut config).is_ok());

        // ディスパッチャ側（次に作られる子の budget が読む先）。
        assert_eq!(dispatcher.config().roles[0].max_turns, Some(60));
        assert_eq!(config.role_specs()[0].max_turns, Some(60));
        // celeris の tick ループが直接読む `[notify]`。
        assert_eq!(config.notify.interval_secs, 90);
    }
    // ---- ADR-0053 D3 追記 / Phase 66b（本番 2026-09-21 の観測）: `[[clusters.forwards]]` を持つ ----
    // ---- クラスタが設定されていても、tick がマルチスレッド tokio ランタイムの中から panic しないこと ----

    /// 本番で観測した panic（`crates/celeris/src/lib.rs:621`、「Cannot start a runtime from within a
    /// runtime」）の再現・回帰テスト。`main`/`--mode verify` と同じ配線（`build_dispatcher`）で
    /// `auth = "totp"` かつ `[[clusters.forwards]]` を持つクラスタを 1 つ作り、master が死んでいる状態
    /// （`cluster_connected` が空）で `dispatcher.tick()` を **`#[tokio::test(flavor = "multi_thread")]`**
    /// （celeris の実行時と同じマルチスレッド・ランタイム）の中から直接呼ぶ。
    ///
    /// 実 ssh は起こさない: `cluster_connector` だけ、実物（`cluster_connector` 関数）と**同じ形**
    /// （現在のスレッドで新しいネストした current_thread ランタイムを作って `block_on` する）の偽物に
    /// 差し替える。これは Phase 66b の修正前なら panic した形そのものなので、この形が panic しなくなった
    /// ことが「呼び出し元が async ワーカーから逃がしている」ことの直接の証拠になる（`tunnel_forward_ensurer`
    /// / `tunnel_probe` は ssh・HTTP を呼ぶだけで元々 panic しないので偽物で十分）。
    ///
    /// Phase 84b: 「master が死んでいる」ことをテストの前提にするため、`set_cluster_liveness_probe` で
    /// `ssh -O check` を偽物（常に false）に差し替える。以前はここを本物の `control_master_alive_blocking`
    /// に任せていたため、テストを動かすマシン自身が（人の別作業で）`pegasus` へ実際に ssh ControlMaster
    /// を張っていると「master 生存」と誤判定され、`cluster_connector` が一度も呼ばれずに落ちた
    /// （観測: 2026-09-21 21:07 UTC の release gate）。クラスタの id/host も、`~/.ssh/config` に実在
    /// しうる名前（`pegasus`/`sirius`/`fern03`）を避け、テスト専用の `test-cluster` にした。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        std::fs::create_dir_all(dir.path().join("ws")).unwrap_or_else(|e| panic!("ws: {e}"));
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
db = "celeris.sqlite3"
workspace_root = "ws"

[[providers]]
id = "x"
adapter = "fake"

[[clusters]]
id = "test-cluster"
host = "test-cluster"
auth = "totp"

[[clusters.forwards]]
listen = "127.0.0.1:0"
target = "bnode150:18000"
"#,
        )
        .unwrap_or_else(|e| panic!("config: {e}"));
        let config = Config::load(&config_path).unwrap_or_else(|e| panic!("{e}"));
        let masters: ClusterMasters = Default::default();
        let mut dispatcher = build_dispatcher(&config, masters)
            .unwrap_or_else(|e| panic!("build_dispatcher: {e}"));

        // Phase 84b: 実機の ssh 状態に依存しないよう、master は常に死んでいる扱いにする。
        dispatcher.set_cluster_liveness_probe(Arc::new(|_ssh_command: &[String], _host: &str| false));

        let connector_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connector_calls_hook = connector_calls.clone();
        dispatcher.set_cluster_connector(Arc::new(move |_cluster_id: &str, _host: &str| {
            connector_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Phase 66b の契約: ここに来た時点で、呼び出し元はすでに tokio の文脈を持たない OS
            // スレッドへ逃がしているはず（さもなければこのテストの意味が無い）。
            assert!(
                tokio::runtime::Handle::try_current().is_err(),
                "the cluster connector hook must run off any tokio runtime context (Phase 66b)"
            );
            // 実物の `cluster_connector`（本ファイルの上のほう）と同じ形: ネストした current_thread
            // ランタイムを作って `block_on` する。修正前はこの形が「Cannot start a runtime from within
            // a runtime」で panic した。実 ssh は起こさない。
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|e| panic!("nested runtime: {e}"));
            rt.block_on(async { Err::<(), String>("test: no real ssh".to_string()) })
        }));
        dispatcher.set_tunnel_forward_ensurer(Arc::new(|_host: &str, _listen: &str, _target: &str| {
            Ok(())
        }));
        dispatcher.set_tunnel_probe(Arc::new(|_listen: &str| false));
        dispatcher.set_accepting_new_work(true);

        // celeris の tick ループ（`tick_loop`）がしているのと同じこと: マルチスレッド tokio ランタイムの
        // 中から、同期の `dispatcher.tick()` を直接呼ぶ。修正前はここで panic した。
        let report = dispatcher.tick();
        assert!(
            report.is_ok(),
            "the first tick must complete without panicking: {report:?}"
        );
        assert!(
            connector_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the totp cluster with a forward must still reach the cluster connector (key auth \
             before TOTP, ADR-0053 D3)"
        );
    }

    // ---- ADR-0053 Phase 66b の未解決事項 / Phase 81: `try_auto_connect_cluster`（`auth =
    // "publickey"`、`dispatch_ready` の中から呼ぶ）も同じ形で async ワーカーから退避させたことの回帰
    // テスト ----

    /// `a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b` と同じ配線
    /// （`build_dispatcher`、`#[tokio::test(flavor = "multi_thread")]`、実物と同じ形（ネストした
    /// current_thread ランタイム + `block_on`）の偽の `cluster_connector`）だが、`auth = "publickey"`
    /// のクラスタに ready なタスクを 1 件置き、`dispatch_ready` から `try_auto_connect_cluster` を
    /// 実際に通す（66b の時点では「本番に publickey クラスタが無いので未検証」として scope 外に
    /// されていた経路）。
    ///
    /// 偽の `cluster_connector` は `Err` を返す: 成功させると `SshWorkspace::prepare` が実際の
    /// ssh/rsync を試みてテストが外部ネットワークに出てしまうため（CLAUDE.md の禁止事項）。自動接続が
    /// 失敗する経路でも、`try_auto_connect_cluster` 自身が tokio の文脈を持たない OS スレッドの中から
    /// 呼ばれることと、tick がパニックしないことは変わらず検証できる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_publickey_cluster_with_a_ready_task_does_not_panic_the_first_tick_phase_81() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        std::fs::create_dir_all(dir.path().join("ws")).unwrap_or_else(|e| panic!("ws: {e}"));
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
db = "celeris.sqlite3"
workspace_root = "ws"

[[providers]]
id = "x"
adapter = "fake"

[[clusters]]
id = "auto"
host = "celeris-no-such-host-for-tests-auto-phase81"
auth = "publickey"
"#,
        )
        .unwrap_or_else(|e| panic!("config: {e}"));
        let config = Config::load(&config_path).unwrap_or_else(|e| panic!("{e}"));
        let store = SqliteStore::open(&config.db.path).unwrap_or_else(|e| panic!("open store: {e}"));
        let now = OffsetDateTime::now_utc();
        let task = task_core::Task {
            repos: Vec::new(),
            id: task_core::TaskId::new(),
            parent_id: None,
            kind: task_core::TaskKind::Execute,
            title: "phase 81 publickey auto-connect".into(),
            objective: "no-op".into(),
            acceptance: vec![task_core::Criterion {
                text: "ok".into(),
                check: task_core::Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![],
            depends_on: vec![],
            status: task_core::Status::Ready,
            priority: 0,
            worker_hint: task_core::WorkerHint {
                tier: task_core::Tier::Standard,
                adapter: None,
            },
            workspace: task_core::WorkspaceSpec::Remote {
                cluster: "auto".into(),
                path: PathBuf::from("/remote/project"),
                mode: None,
            },
            budget: task_core::Budget {
                max_turns: 3,
                max_wall_secs: 30,
                max_retries: 0,
            },
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
            skills: Vec::new(),
            mode: task_core::TaskMode::default(),
            labels: Vec::new(),
            category: Default::default(),
        };
        store.insert(&task).unwrap_or_else(|e| panic!("insert: {e}"));

        let masters: ClusterMasters = Default::default();
        let mut dispatcher = build_dispatcher(&config, masters)
            .unwrap_or_else(|e| panic!("build_dispatcher: {e}"));

        // Phase 84b: 実機の ssh 状態に依存しないよう、master は常に死んでいる扱いにする
        // （`try_auto_connect_cluster` は `cluster_connected` を見ないが、`refresh_cluster_liveness`
        // が同じ tick で先に呼ばれるので、ここも決定的な偽物に揃えておく）。
        dispatcher.set_cluster_liveness_probe(Arc::new(|_ssh_command: &[String], _host: &str| false));

        let connector_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connector_calls_hook = connector_calls.clone();
        dispatcher.set_cluster_connector(Arc::new(move |_cluster_id: &str, _host: &str| {
            connector_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Phase 81 の契約: `try_auto_connect_cluster` も `run_cluster_hooks_off_async` 経由で
            // 呼ばれ、ここに来た時点で呼び出し元はすでに tokio の文脈を持たない OS スレッドへ
            // 逃がしているはず（さもなければこのテストの意味が無い）。
            assert!(
                tokio::runtime::Handle::try_current().is_err(),
                "the cluster connector hook must run off any tokio runtime context (Phase 81)"
            );
            // 実物の `cluster_connector`（本ファイルの上のほう）と同じ形: ネストした current_thread
            // ランタイムを作って `block_on` する。修正前ならこの形は「Cannot start a runtime from
            // within a runtime」で panic した経路。実 ssh は起こさない。
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|e| panic!("nested runtime: {e}"));
            rt.block_on(async { Err::<(), String>("test: no real ssh".to_string()) })
        }));
        dispatcher.set_accepting_new_work(true);

        // celeris の tick ループがしているのと同じこと: マルチスレッド tokio ランタイムの中から、
        // 同期の `dispatcher.tick()` を直接呼ぶ。退避していなければここで panic した。
        let report = dispatcher.tick();
        assert!(
            report.is_ok(),
            "the first tick must complete without panicking: {report:?}"
        );
        assert_eq!(
            connector_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the publickey cluster must reach the cluster connector exactly once (ADR-0032 D3)"
        );
        // 自動接続が失敗したので cooldown に落ち、この tick では dispatch されない（実
        // ssh/rsync には一切触れていない）。
        assert_eq!(report.unwrap().dispatched, 0);
    }

    /// ADR-0053 D3 Phase 84b 追記: `a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b`
    /// と同じ配線（`build_dispatcher`、`[[clusters.forwards]]` を持つ `auth = "totp"` クラスタ）だが、
    /// `set_cluster_liveness_probe` が「master 生存」を返す点だけが違う。D3 の設計どおり、master が
    /// 生きていれば `cluster_connector`（鍵認証での再接続）は要らず、`tunnel_forward_ensurer`
    /// （forward の(再)確立）だけが呼ばれることを確認する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_totp_cluster_with_a_live_master_skips_the_connector_but_ensures_the_forward_phase_84b()
     {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        std::fs::create_dir_all(dir.path().join("ws")).unwrap_or_else(|e| panic!("ws: {e}"));
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
db = "celeris.sqlite3"
workspace_root = "ws"

[[providers]]
id = "x"
adapter = "fake"

[[clusters]]
id = "test-cluster"
host = "test-cluster"
auth = "totp"

[[clusters.forwards]]
listen = "127.0.0.1:0"
target = "bnode150:18000"
"#,
        )
        .unwrap_or_else(|e| panic!("config: {e}"));
        let config = Config::load(&config_path).unwrap_or_else(|e| panic!("{e}"));
        let masters: ClusterMasters = Default::default();
        let mut dispatcher = build_dispatcher(&config, masters)
            .unwrap_or_else(|e| panic!("build_dispatcher: {e}"));

        // master は常に生存している扱い（実機の ssh 状態には依存しない偽物）。
        dispatcher.set_cluster_liveness_probe(Arc::new(|_ssh_command: &[String], _host: &str| true));

        let connector_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connector_calls_hook = connector_calls.clone();
        dispatcher.set_cluster_connector(Arc::new(move |_cluster_id: &str, _host: &str| {
            connector_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<(), String>("must not be called while the master is alive (Phase 84b)".to_string())
        }));
        let ensure_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ensure_calls_hook = ensure_calls.clone();
        dispatcher.set_tunnel_forward_ensurer(Arc::new(move |_host: &str, _listen: &str, _target: &str| {
            ensure_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        dispatcher.set_tunnel_probe(Arc::new(|_listen: &str| false));
        dispatcher.set_accepting_new_work(true);

        let report = dispatcher.tick();
        assert!(
            report.is_ok(),
            "the first tick must complete without panicking: {report:?}"
        );
        assert_eq!(
            connector_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the cluster connector must not run while the ssh master is alive (ADR-0053 D3)"
        );
        assert!(
            ensure_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the forward must still be (re-)established while the master is alive (ADR-0053 D3)"
        );
    }

    #[test]
    fn explicit_credential_reference_missing_blocks_instead_of_using_inherited_auth() {
        let cfg: Config = toml::from_str(
            r#"[[providers]]
id = "gpt"
adapter = "codex"
[providers.env_from_secrets]
OPENAI_API_KEY = "missing-key"
[providers.tier_models.standard]
name = "sol"
model_id = "explicit-id"
"#,
        )
        .unwrap();
        let adapters = build_adapters(&cfg);
        assert!(
            adapters["gpt"]
                .model_for_tier(task_core::Tier::Standard)
                .unwrap_err()
                .contains("missing-key")
        );
    }
}
