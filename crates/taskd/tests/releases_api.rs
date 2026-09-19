//! ADR-0040 D6（Phase 48）: `GET /releases` と `POST /releases/{sha12}/promote` を、
//! **本物のリリースディレクトリ**（tempdir）に対して HTTP で確かめる。
//!
//! task-api はファイルの規約を知らない（`taskd::releases::FsReleases` が `ReleaseSource` として
//! 渡る）ので、API の形を確かめるには両方が要る。だからこのテストは taskd 側に置く。
//!
//! 見るもの:
//! - `GET /releases`: 空のディレクトリでも 200、`built_at` の新しい順、`current` / `previous` の印、
//!   壊れた JSON は `problem` 付きで出る（落ちない）、`running` と `instances`。
//! - `POST /releases/{sha12}/promote`: 401（トークン無し）/ 404（知らない sha）/
//!   409 × 3（未検証・既に current・既に昇格中）/ 202（同梱の `scripts/promote.sh` を起こす）。
//!
//! **本番には一切触れない**: `~/taskd/` も systemd も本番プロセスも出てこない。起こすのは tempdir の
//! 中の偽 `promote.sh`（ログに 1 行書いて眠るだけ）。外部ネットワークにも出ない（loopback だけ）。

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use task_core::{DaemonMode, InstanceRole, SharedRole, SqliteStore};
use taskd::Config;

const TOKEN: &str = "releases-test-token";

struct Api {
    _dir: tempfile::TempDir,
    /// `~/taskd` 相当（`current` / `previous` の symlink はここに張る）。
    home: PathBuf,
    /// `<home>/releases`。
    releases: PathBuf,
    base: String,
    /// 落とすと `watch` が閉じるので、テストの間は持ち続ける。
    _daemon_tx: tokio::sync::watch::Sender<Option<task_ops::daemon::DaemonSnapshot>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<Result<(), task_api::ApiError>>>,
}

impl Api {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let home = dir.path().to_path_buf();
        let releases = home.join("releases");
        std::fs::create_dir_all(&releases).unwrap_or_else(|e| panic!("releases: {e}"));
        std::fs::create_dir_all(home.join("ws")).unwrap_or_else(|e| panic!("ws: {e}"));
        std::fs::write(home.join("api.token"), format!("{TOKEN}\n")).unwrap_or_else(|e| panic!("token: {e}"));
        let config_path = home.join("taskd.toml");
        std::fs::write(
            &config_path,
            r#"
db = "taskd.sqlite3"
workspace_root = "ws"
tick_ms = 200

[api]
listen = "127.0.0.1:0"
token_file = "api.token"

[selfdeploy]
releases_dir = "releases"

[[providers]]
id = "p1"
adapter = "fake"
"#,
        )
        .unwrap_or_else(|e| panic!("config: {e}"));

        let config = Config::load(&config_path).unwrap_or_else(|e| panic!("load: {e}"));
        assert_eq!(config.selfdeploy.releases_dir, releases, "relative releases_dir must be config-dir based");
        // マイグレーションを流す（`GET /releases` は `daemon_instances` を読む）。
        let _store = SqliteStore::open(&config.db).unwrap_or_else(|e| panic!("open: {e}"));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let addr: SocketAddr = listener.local_addr().unwrap_or_else(|e| panic!("addr: {e}"));
        let settings = taskd::api_settings(
            &config,
            addr,
            Some(TOKEN.to_string()),
            "01J00000000000000000000000".to_string(),
            "2026-09-19T00:00:00Z".to_string(),
            None,
            "dev".to_string(),
            DaemonMode::Normal,
            SharedRole::new(InstanceRole::Active),
        );
        let (daemon_tx, daemon_rx) = tokio::sync::watch::channel(None);
        let state = task_api::ApiState::new(settings, daemon_rx).unwrap_or_else(|e| panic!("state: {e}"));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(task_api::serve_with_listener(listener, state, async move {
            let _ = stopped.await;
        }));

        Self {
            _dir: dir,
            home,
            releases,
            base: format!("http://{addr}/api/v1"),
            _daemon_tx: daemon_tx,
            stop: Some(stop),
            handle: Some(handle),
        }
    }

    async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
    }

    /// `<releases>/<sha>/` を作り、渡した JSON を置く。
    fn release(&self, sha: &str, manifest: Option<&str>, gate: Option<&str>, verify: Option<&str>) -> PathBuf {
        let dir = self.releases.join(sha);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        for (name, body) in [("manifest.json", manifest), ("gate.json", gate), ("verify.json", verify)] {
            if let Some(body) = body {
                std::fs::write(dir.join(name), body).unwrap_or_else(|e| panic!("write: {e}"));
            }
        }
        dir
    }

    /// 偽の `scripts/promote.sh`（ログに 1 行書いて眠るだけ。本物の昇格は起きない）。
    fn fake_promote_script(&self, sha: &str) {
        let scripts = self.releases.join(sha).join("scripts");
        std::fs::create_dir_all(&scripts).unwrap_or_else(|e| panic!("mkdir: {e}"));
        let script = scripts.join("promote.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'fake promote %s\\n' \"$1\"\nsleep 3\n")
            .unwrap_or_else(|e| panic!("write: {e}"));
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod: {e}"));
    }

    fn link(&self, name: &str, sha: &str) {
        let link = self.home.join(name);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(format!("releases/{sha}"), &link).unwrap_or_else(|e| panic!("symlink: {e}"));
    }

    /// 読み取り（`GET /releases` は**管理系ではない**が、`token_file` を設定した構成では通常の
    /// 認証は通る — `GET /notify` と同じ扱い）。
    async fn get(&self, path: &str) -> (u16, serde_json::Value) {
        self.get_with(path, Some(TOKEN)).await
    }

    async fn get_with(&self, path: &str, token: Option<&str>) -> (u16, serde_json::Value) {
        let mut req = reqwest::Client::new().get(format!("{}{path}", self.base));
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await.unwrap_or_else(|e| panic!("get {path}: {e}"));
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
    }

    async fn post(&self, path: &str, token: Option<&str>) -> (u16, serde_json::Value) {
        let mut req = reqwest::Client::new()
            .post(format!("{}{path}", self.base))
            .header("content-type", "application/json")
            .body("{}");
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let resp = req.send().await.unwrap_or_else(|e| panic!("post {path}: {e}"));
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        (status, serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
    }
}

fn wait_for(path: &Path, needle: &str) -> String {
    for _ in 0..100 {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::read_to_string(path).unwrap_or_default()
}

/// 空の `releases_dir` でも 200。`running` は `GET /health` と同じ値を持つ。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_releases_is_200_on_an_empty_directory() {
    let api = Api::start().await;
    let (status, body) = api.get("/releases").await;
    assert_eq!(status, 200, "{body}");
    assert!(body["current"].is_null());
    assert!(body["previous"].is_null());
    assert_eq!(body["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["instances"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["running"]["release"], "dev");
    assert_eq!(body["running"]["role"], "active");
    assert_eq!(body["running"]["instance_id"], "01J00000000000000000000000");

    let (health_status, health) = api.get("/health").await;
    assert_eq!(health_status, 200);
    assert_eq!(health["release"], body["running"]["release"]);
    assert_eq!(health["role"], body["running"]["role"]);
    api.shutdown().await;
}

/// 一覧は `built_at` の新しい順。`current` / `previous` の印と `verify` の要約が出る。
/// 壊れた JSON のリリースは `problem` 付きで出る（一覧全体は落ちない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_releases_sorts_newest_first_and_tolerates_broken_releases() {
    let api = Api::start().await;
    api.release(
        "aaaaaaaaaaaa",
        Some(r#"{"ref":"main","built_at":"2026-09-18T00:00:00Z","schema_version":11}"#),
        Some(r#"{"ok":true}"#),
        Some(r#"{"ok":true,"live_ok":true,"at":"2026-09-18T01:00:00Z"}"#),
    );
    api.release(
        "bbbbbbbbbbbb",
        Some(r#"{"ref":"self/01M","built_at":"2026-09-19T00:00:00Z","schema_version":11}"#),
        Some(r#"{"ok":true}"#),
        None,
    );
    api.release("cccccccccccc", Some("{ broken"), None, None);
    std::fs::create_dir_all(api.releases.join(".build")).unwrap_or_else(|e| panic!("mkdir: {e}"));
    api.link("current", "aaaaaaaaaaaa");

    let (status, body) = api.get("/releases").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["current"], "aaaaaaaaaaaa");
    assert!(body["previous"].is_null());
    let items = body["items"].as_array().unwrap_or_else(|| panic!("items: {body}"));
    assert_eq!(
        items.iter().map(|i| i["sha12"].as_str().unwrap_or("")).collect::<Vec<_>>(),
        ["bbbbbbbbbbbb", "aaaaaaaaaaaa", "cccccccccccc"],
        "built_at の新しい順（読めなかったものは最後）: {body}"
    );
    assert_eq!(items[0]["ref"], "self/01M");
    assert_eq!(items[0]["schema_version"], 11);
    assert_eq!(items[0]["gate_ok"], true);
    assert!(items[0]["verify"].is_null(), "verify.json が無ければ未検証");
    assert_eq!(items[0]["promoting"], false);
    assert_eq!(items[1]["is_current"], true);
    assert_eq!(items[1]["verify"]["ok"], true);
    assert_eq!(items[1]["verify"]["live_ok"], true);
    assert_eq!(items[1]["verify"]["at"], "2026-09-18T01:00:00Z");
    assert_eq!(items[2]["gate_ok"], false);
    assert!(
        items[2]["problem"].as_str().unwrap_or_default().contains("manifest.json"),
        "{body}"
    );
    // `.build` は一覧に出ない。
    assert!(!items.iter().any(|i| i["sha12"] == ".build"), "{body}");
    api.shutdown().await;
}

/// 昇格は管理系: トークンが無ければ 401（`token_file` を設定していても、していなくても）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promote_requires_a_token() {
    let api = Api::start().await;
    api.release("aaaaaaaaaaaa", Some("{}"), Some(r#"{"ok":true}"#), Some(r#"{"ok":true,"live_ok":true}"#));
    let (status, body) = api.post("/releases/aaaaaaaaaaaa/promote", None).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["code"], "unauthorized");
    api.shutdown().await;
}

/// 知らない sha は 404。sha12 の形でないものも 404（パストラバーサルはここで落ちる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promoting_an_unknown_release_is_404() {
    let api = Api::start().await;
    let (status, body) = api.post("/releases/aaaaaaaaaaaa/promote", Some(TOKEN)).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "release_not_found");
    let (status, body) = api.post("/releases/notahexsha/promote", Some(TOKEN)).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "release_not_found");
    api.shutdown().await;
}

/// 409 × 3: 未検証 / 既に current / 既に昇格中。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promoting_is_409_when_unverified_already_current_or_already_promoting() {
    let api = Api::start().await;

    // (1) verify.json が無い＝未検証。
    api.release("aaaaaaaaaaaa", Some(r#"{"built_at":"2026-09-18T00:00:00Z"}"#), Some(r#"{"ok":true}"#), None);
    api.fake_promote_script("aaaaaaaaaaaa");
    let (status, body) = api.post("/releases/aaaaaaaaaaaa/promote", Some(TOKEN)).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "release_not_promotable");
    assert!(body["detail"].as_str().unwrap_or_default().contains("verify"), "{body}");

    // (2) 既に current。
    std::fs::write(
        api.releases.join("aaaaaaaaaaaa").join("verify.json"),
        r#"{"ok":true,"live_ok":true}"#,
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    api.link("current", "aaaaaaaaaaaa");
    let (status, body) = api.post("/releases/aaaaaaaaaaaa/promote", Some(TOKEN)).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "release_not_promotable");
    assert!(body["detail"].as_str().unwrap_or_default().contains("current"), "{body}");

    // (3) 既に昇格中（生きている pid の promote.lock）。
    api.release(
        "bbbbbbbbbbbb",
        Some(r#"{"built_at":"2026-09-19T00:00:00Z"}"#),
        Some(r#"{"ok":true}"#),
        Some(r#"{"ok":true,"live_ok":true}"#),
    );
    api.fake_promote_script("bbbbbbbbbbbb");
    std::fs::write(
        api.releases.join("bbbbbbbbbbbb").join("promote.lock"),
        format!("{}\n", std::process::id()),
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    let (status, body) = api.post("/releases/bbbbbbbbbbbb/promote", Some(TOKEN)).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "release_not_promotable");
    assert!(body["detail"].as_str().unwrap_or_default().contains("already running"), "{body}");
    api.shutdown().await;
}

/// 202: リリースに同梱された `scripts/promote.sh` を detached で起こし、`promote.log` に出力が流れ、
/// `promote.lock` に pid が入り、一覧の `promoting` が真になる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promoting_a_verified_release_starts_the_bundled_script_and_returns_202() {
    let api = Api::start().await;
    api.release(
        "abcdef123456",
        Some(r#"{"ref":"self/01M","built_at":"2026-09-19T00:00:00Z","schema_version":11}"#),
        Some(r#"{"ok":true}"#),
        Some(r#"{"ok":true,"live_ok":true,"at":"2026-09-19T02:00:00Z"}"#),
    );
    api.fake_promote_script("abcdef123456");

    let (status, body) = api.post("/releases/abcdef123456/promote", Some(TOKEN)).await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["sha12"], "abcdef123456");
    assert!(
        body["log"].as_str().unwrap_or_default().ends_with("abcdef123456/promote.log"),
        "{body}"
    );
    assert!(body["started_at"].as_str().unwrap_or_default().contains('T'), "{body}");

    let log = api.releases.join("abcdef123456").join("promote.log");
    let text = wait_for(&log, "fake promote abcdef123456");
    assert!(text.contains("fake promote abcdef123456"), "{text:?}");

    let pid: u32 = std::fs::read_to_string(api.releases.join("abcdef123456").join("promote.lock"))
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0);
    assert!(pid > 0, "promote.lock must carry the pid");

    // 走っている間は一覧に `promoting = true` で出て、もう一度押しても 409。
    let (status, body) = api.get("/releases").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"][0]["promoting"], true, "{body}");
    let (status, body) = api.post("/releases/abcdef123456/promote", Some(TOKEN)).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "release_not_promotable");
    api.shutdown().await;
}
