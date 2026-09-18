//! task-api の結合テストの共通部品: tempfile の SQLite、`ApiState`、oneshot の要求、SSE の読み取り。
//! ネットワークは loopback だけ（多くは TCP を使わず `tower::ServiceExt::oneshot`）。

#![allow(dead_code)]

use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, BodyDataStream};
use axum::http::{HeaderMap, Request, StatusCode};
use futures_util::StreamExt;
use serde_json::Value;
use task_api::{
    AdminRequest, ApiConfigView, ApiSettings, ApiState, ClusterConfigView, ConfigView, GenreConfigView,
    ProviderConfigView, ReviewerConfigView, RoleConfigView,
};
use task_core::{
    Budget, Check, Criterion, Event, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier, WorkerHint,
    WorkspaceSpec,
};
use task_ops::daemon::{ClusterLive, CooldownView, DaemonSnapshot, InFlight, InFlightKind, ProviderLive};
use task_ops::view::ViewContext;
use time::OffsetDateTime;
use tokio::sync::{mpsc, watch};
use tower::ServiceExt;

pub const HOST: &str = "127.0.0.1:7710";
pub const TOKEN: &str = "s3cret-token-value";

pub struct EnvOptions {
    pub token: Option<String>,
    pub allowed_hosts: Vec<String>,
    /// ADR-0016 D1: `POST /tasks` の省略値を埋める `[[roles]]`。
    pub roles: Vec<task_core::RoleSpec>,
    /// ADR-0027 D1: `POST /tasks` の `genre` の検証・既定解決に使う `[[genres]]`。
    pub genres: Vec<task_core::GenreSpec>,
    /// Phase 30（ADR-0033 D4 追記）: 対話は常にこの分野で走る。既定は `Default` impl で
    /// `task_core::CONVERSATION_GENRE`。
    pub conversation_genre: String,
    /// ADR-0017 M1: `providers.d/` の書き込み先。`None` なら管理系の作成/変更/削除は使えない。
    pub providers_dir: Option<PathBuf>,
    /// ADR-0017 M2: `reload`/`check` を受け取るチャネルの送信側。`None` ならどちらも使えない。
    pub admin_tx: Option<mpsc::Sender<AdminRequest>>,
    /// ADR-0024 D1: `[accounts] claude_dir`。`None` なら claude-code のプールは無し。
    pub accounts_root: Option<PathBuf>,
    /// ADR-0025 D1: `[accounts] codex_dir`。`None` なら codex のプールは無し。
    pub codex_accounts_root: Option<PathBuf>,
    pub max_runs_per_account: usize,
    /// ADR-0030 D1: `[secrets] dir`。`None` なら秘密の管理系は 409 `secrets_unavailable`。
    pub secrets_dir: Option<PathBuf>,
    /// ADR-0030 D3: 秘密 id → `used_by`（`GET /secrets` の `used_by`）。
    pub secret_usage: std::collections::HashMap<String, Vec<task_api::SecretUse>>,
    /// ADR-0033 D6（GUI 監査対応 Phase 29）: `[memory] dir`。`None` なら `GET /org/{id}/memory` は 409。
    pub memory_dir: Option<PathBuf>,
}

impl Default for EnvOptions {
    fn default() -> Self {
        Self {
            token: None,
            allowed_hosts: Vec::new(),
            roles: Vec::new(),
            genres: Vec::new(),
            // Phase 30: 既定の対話用分野は他の既定値と同じく `task_core::CONVERSATION_GENRE`。
            conversation_genre: task_core::CONVERSATION_GENRE.to_string(),
            providers_dir: None,
            admin_tx: None,
            accounts_root: None,
            codex_accounts_root: None,
            max_runs_per_account: 0,
            secrets_dir: None,
            secret_usage: std::collections::HashMap::new(),
            memory_dir: None,
        }
    }
}

pub struct TestEnv {
    pub dir: tempfile::TempDir,
    pub db_path: PathBuf,
    pub workspace_root: PathBuf,
    /// テストが書き込みに使う別接続（taskctl / ディスパッチャ相当）。
    pub store: SqliteStore,
    pub state: ApiState,
    pub daemon_tx: watch::Sender<Option<DaemonSnapshot>>,
}

impl TestEnv {
    pub fn new() -> Self {
        Self::with(EnvOptions::default())
    }

    pub fn with(options: EnvOptions) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("taskd.db");
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).expect("workspace root");
        let store = SqliteStore::open(&db_path).expect("open store");
        let (daemon_tx, daemon_rx) = watch::channel(None);
        let settings = settings(&db_path, &workspace_root, options);
        let state = ApiState::new(settings, daemon_rx).expect("api state");
        Self {
            dir,
            db_path,
            workspace_root,
            store,
            state,
            daemon_tx,
        }
    }

    pub fn router(&self) -> Router {
        task_api::router(self.state.clone())
    }

    pub fn view_context(&self) -> ViewContext {
        view_context(&self.workspace_root)
    }

    /// `task` を `create_task`（`Created` 付き）で挿入し、ワークスペースのディレクトリを作る。
    pub fn seed(&self, task: &Task) {
        self.seed_with(task, vec![]);
    }

    pub fn seed_with(&self, task: &Task, extra: Vec<Event>) {
        self.store.create_task(task, extra).expect("create task");
        std::fs::create_dir_all(self.workspace(task)).expect("workspace dir");
    }

    pub fn workspace(&self, task: &Task) -> PathBuf {
        self.workspace_root.join(task.id.to_string())
    }

    pub fn status_of(&self, id: TaskId) -> Status {
        self.store.get(id).expect("get").expect("task exists").status
    }
}

pub fn view_context(workspace_root: &std::path::Path) -> ViewContext {
    ViewContext {
        workspace_root: workspace_root.to_path_buf(),
        retry_backoff_base: Duration::from_secs(30),
        retry_backoff_max: Duration::from_secs(600),
        max_requeues: 5,
        clusters: Default::default(),
    }
}

pub fn config_view() -> ConfigView {
    ConfigView {
        config_path: "/etc/taskd/taskd.toml".into(),
        db: "/var/lib/taskd/taskd.db".into(),
        workspace_root: "/var/lib/taskd/workspaces".into(),
        tick_ms: 2000,
        max_concurrency: 4,
        lease_grace_secs: 60,
        idle_timeout_secs: 300,
        kill_grace_secs: 5,
        review_timeout_secs: 600,
        error_cooldown_secs: 60,
        retry_backoff_base_secs: 30,
        retry_backoff_max_secs: 600,
        max_requeues: 5,
        plan_auto_accept: false,
        reviewer: ReviewerConfigView {
            adapter: Some("claude-code".into()),
            tier: Tier::Standard,
        },
        providers: vec![
            ProviderConfigView {
                id: "claude-a".into(),
                adapter: "claude-code".into(),
                tiers: vec![Tier::Frontier, Tier::Standard],
                concurrency: 2,
                model: Some("claude-sonnet-5".into()),
                env_keys: vec!["CLAUDE_CONFIG_DIR".into()],
                account_pool: false,
            },
            ProviderConfigView {
                id: "claude-b".into(),
                adapter: "claude-code".into(),
                tiers: vec![Tier::Frontier],
                concurrency: 1,
                model: None,
                env_keys: vec!["CLAUDE_CONFIG_DIR".into()],
                account_pool: false,
            },
        ],
        clusters: vec![ClusterConfigView {
            id: "pegasus".into(),
            host: "pegasus".into(),
            concurrency: 2,
            sync: "rsync".into(),
            delete_on_push: false,
            has_setup: true,
            env_keys: vec!["OMP_NUM_THREADS".into()],
            rsync_excludes: vec![".git/".into()],
            auth: "manual".into(),
        }],
        roles: vec![RoleConfigView {
            id: "lead".into(),
            tier: Some(Tier::Frontier),
            adapter: None,
            max_turns: Some(40),
            max_wall_secs: None,
            has_instructions: true,
        }],
        genres: vec![GenreConfigView {
            id: "coding".into(),
            description: "write and fix code".into(),
            capabilities: vec![],
            input_artifacts: vec![],
            output_artifacts: vec![],
            default_role: Some("lead".into()),
            roles: vec!["lead".into()],
        }],
        delegation: task_core::DelegationLimits::default(),
        api: ApiConfigView {
            bind: HOST.into(),
            auth_required: false,
            allowed_hosts: vec![],
        },
    }
}

pub fn settings(db_path: &std::path::Path, workspace_root: &std::path::Path, options: EnvOptions) -> ApiSettings {
    ApiSettings {
        listen: HOST.parse().expect("listen"),
        token: options.token,
        allowed_hosts: options.allowed_hosts,
        db_path: db_path.to_path_buf(),
        busy_timeout: Duration::from_millis(5000),
        view: view_context(workspace_root),
        config_view: config_view(),
        roles: options.roles,
        genres: options.genres,
        conversation_genre: options.conversation_genre,
        taskd_version: "0.9.0-test".into(),
        instance_id: "01J9ZX5T3K8Q7W6V5R4P3N2M1H".into(),
        started_at: "2026-09-14T00:00:00Z".into(),
        providers_dir: options.providers_dir,
        admin_tx: options.admin_tx,
        accounts_roots: {
            let mut roots = std::collections::HashMap::new();
            if let Some(dir) = options.accounts_root {
                roots.insert(task_core::AccountAdapter::ClaudeCode, dir);
            }
            if let Some(dir) = options.codex_accounts_root {
                roots.insert(task_core::AccountAdapter::Codex, dir);
            }
            roots
        },
        max_runs_per_account: options.max_runs_per_account,
        secrets_dir: options.secrets_dir,
        secret_usage: options.secret_usage,
        memory_dir: options.memory_dir,
    }
}

pub fn new_task(kind: TaskKind, status: Status) -> Task {
    let id = TaskId::new();
    let now = OffsetDateTime::now_utc();
    Task {
        id,
        parent_id: None,
        kind,
        title: format!("{kind:?} task"),
        objective: "make it work".into(),
        acceptance: vec![Criterion {
            text: "a human is happy".into(),
            check: Check::Human,
        }],
        inputs: vec![],
        depends_on: vec![],
        status,
        priority: 0,
        worker_hint: WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        },
        workspace: WorkspaceSpec::Local {
            path: PathBuf::from(id.to_string()),
        },
        budget: Budget {
            max_turns: 10,
            max_wall_secs: 600,
            max_retries: 2,
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
    }
}

pub fn snapshot(ticks: u64) -> DaemonSnapshot {
    DaemonSnapshot {
        instance_id: "01J9ZX5T3K8Q7W6V5R4P3N2M1H".into(),
        pid: 1234,
        hostname: "lab-01".into(),
        started_at: "2026-09-14T00:00:00Z".into(),
        last_tick_at: "2026-09-14T00:00:02Z".into(),
        ticks,
        tick_ms: 2000,
        in_flight: vec![InFlight {
            task_id: TaskId::new(),
            run_id: "01J9ZX5T3K8Q7W6V5R4P3N2M1J".into(),
            provider: "claude-a".into(),
            kind: InFlightKind::Worker,
            since: "2026-09-14T00:00:01Z".into(),
        }],
        cooldowns: vec![CooldownView {
            provider: "claude-b".into(),
            until: "2026-09-14T00:05:00Z".into(),
            reason: "throttled".into(),
        }],
        awaiting_human: vec![],
        awaiting_children: vec![],
        unroutable: vec![],
        reports: None,
        approvals_pending: 0,
        accounts_root: None,
        max_runs_per_account: None,
        accounts_roots: std::collections::HashMap::new(),
        accounts: vec![],
        clusters: vec![ClusterLive {
            id: "pegasus".into(),
            host: "pegasus".into(),
            concurrency: 2,
            in_use: 1,
            connected: false,
            cooldown_until: Some("2099-01-01T00:00:00Z".into()),
            auth: "manual".into(),
            connect_pending: false,
        }],
        providers: vec![
            ProviderLive {
                id: "claude-a".into(),
                adapter: "claude-code".into(),
                tiers: vec![Tier::Frontier, Tier::Standard],
                concurrency: 2,
                model: Some("claude-sonnet-5".into()),
                env_keys: vec!["CLAUDE_CONFIG_DIR".into()],
                in_use: 1,
                // ADR-0022 D2: 一度 check した後のスナップショット（`GET /providers` の last_check に出る）。
                last_check: Some(task_ops::daemon::ProviderCheckView {
                    at: "2026-09-16T01:00:00Z".into(),
                    result: "ok".into(),
                    detail: Some("ready".into()),
                }),
                account_pool: false,
            },
            ProviderLive {
                id: "claude-b".into(),
                adapter: "claude-code".into(),
                tiers: vec![Tier::Frontier],
                concurrency: 1,
                model: None,
                env_keys: vec![],
                in_use: 0,
                last_check: None,
                account_pool: false,
            },
        ],
    }
}

// ---- 要求と応答 ----

pub fn get(path: &str) -> Request<Body> {
    Request::get(path).header("host", HOST).body(Body::empty()).expect("request")
}

/// `get` にヘッダを足す（同名のヘッダは置き換える。`host` を渡すと既定の Host を差し替える）。
pub fn get_with(path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = get(path);
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            axum::http::HeaderValue::from_str(value).expect("header value"),
        );
    }
    request
}

pub fn post_json(path: &str, body: &Value) -> Request<Body> {
    Request::post(path)
        .header("host", HOST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// `post_json` にヘッダを足す（`authorization` を渡す admin エンドポイントのテスト用）。
pub fn post_json_with(path: &str, body: &Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = post_json(path, body);
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            axum::http::HeaderValue::from_str(value).expect("header value"),
        );
    }
    request
}

pub fn patch_json_with(path: &str, body: &Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::patch(path)
        .header("host", HOST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            axum::http::HeaderValue::from_str(value).expect("header value"),
        );
    }
    request
}

pub fn put_json_with(path: &str, body: &Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::put(path)
        .header("host", HOST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            axum::http::HeaderValue::from_str(value).expect("header value"),
        );
    }
    request
}

pub fn delete_with(path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::delete(path).header("host", HOST).body(Body::empty()).expect("request");
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            axum::http::HeaderValue::from_str(value).expect("header value"),
        );
    }
    request
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

pub async fn send(app: &Router, request: Request<Body>) -> Resp {
    let response = app.clone().oneshot(request).await.expect("infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body")
        .to_vec();
    Resp { status, headers, body }
}

/// problem+json の共通部分（`type` / `code` / `status` / `instance` = `X-Request-Id`）を確かめて本体を返す。
#[track_caller]
pub fn assert_problem(resp: &Resp, status: u16, code: &str) -> Value {
    assert_eq!(resp.status.as_u16(), status, "unexpected status; body: {}", resp.text());
    assert_eq!(resp.header("content-type"), Some("application/problem+json"));
    let problem = resp.json();
    assert_eq!(problem["code"], code, "{problem}");
    assert_eq!(problem["status"], status);
    assert_eq!(problem["type"], format!("urn:taskd:problem:{code}"));
    let request_id = resp.header("x-request-id").expect("x-request-id");
    assert_eq!(problem["instance"], format!("urn:taskd:request:{request_id}"));
    assert!(problem["title"].is_string() && problem["detail"].is_string());
    problem
}

// ---- SSE ----

#[derive(Debug, Clone)]
pub struct Frame {
    pub event: String,
    pub id: Option<u64>,
    pub data: Value,
}

pub struct Sse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    stream: BodyDataStream,
    buf: String,
}

pub async fn open_stream(app: &Router, request: Request<Body>) -> Sse {
    let response = app.clone().oneshot(request).await.expect("infallible");
    Sse {
        status: response.status(),
        headers: response.headers().clone(),
        stream: response.into_body().into_data_stream(),
        buf: String::new(),
    }
}

impl Sse {
    /// 次のフレーム。`within` 以内に来なければ、またはストリームが終われば `None`。
    pub async fn next_frame(&mut self, within: Duration) -> Option<Frame> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if let Some(pos) = self.buf.find("\n\n") {
                let raw: String = self.buf.drain(..pos + 2).collect();
                return Some(parse_frame(&raw));
            }
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Ok(Some(Ok(bytes))) => self.buf.push_str(std::str::from_utf8(&bytes).expect("utf-8 frame")),
                _ => return None,
            }
        }
    }

    /// `name` のフレームが来るまで他（heartbeat / daemon 等）を読み飛ばす。
    pub async fn next_named(&mut self, name: &str, within: Duration) -> Option<Frame> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let frame = self.next_frame(remaining).await?;
            if frame.event == name {
                return Some(frame);
            }
        }
    }

    /// エラー応答（503 等）の本体。
    pub async fn into_body_json(mut self) -> Value {
        let mut bytes = Vec::new();
        while let Some(Ok(chunk)) = self.stream.next().await {
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).expect("problem JSON")
    }
}

fn parse_frame(raw: &str) -> Frame {
    let mut event = String::new();
    let mut id = None;
    let mut data = Value::Null;
    for line in raw.lines() {
        if let Some(v) = line.strip_prefix("event: ") {
            event = v.to_string();
        } else if let Some(v) = line.strip_prefix("id: ") {
            id = Some(v.parse().expect("numeric id"));
        } else if let Some(v) = line.strip_prefix("data: ") {
            data = serde_json::from_str(v).expect("JSON data line");
        }
    }
    Frame { event, id, data }
}

/// `cond` が真になるまで待つ（最大 `within`）。
pub async fn eventually(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if cond() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn progress(msg: &str) -> Event {
    Event::WorkerProgress {
        run_id: "01J9ZX5T3K8Q7W6V5R4P3N2M1J".into(),
        msg: msg.into(),
    }
}
