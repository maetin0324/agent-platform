//! 人の判断が要るときだけ Discord に知らせる（ADR-0037。Phase 39）。
//!
//! ここには 2 つのことしか無い:
//!
//! 1. **判定**（`scan` / `schedule`）— DB を読んで「人の手が要る」5 種の条件を**決定的に**見つけ、
//!    `notifications` にまだ無い `(kind, key)` を pending として 1 件だけ作る。LLM は関与しない。
//!    `tick_loop` の中から同期で呼ばれる（B1: チャネルに送らず、その場で store を見る）。
//! 2. **送信**（`spawn_send` / `post_webhook`）— pending を Discord の webhook へ POST する。
//!    tick をブロックしないよう `tokio::spawn` で送り、**結果は次の tick で** `notification_mark` する
//!    （送信結果は `mpsc` でループへ戻る。ADR-0022 D2 の `check` と同じ形）。
//!
//! 秘密（webhook URL）の規律（ADR-0037 D3）: **URL はログにもエラー文にも応答にも出さない**。
//! `reqwest::Error` の `Display` は URL を含むので、そのまま文字列にしてはいけない（`safe_error` を使う）。

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use task_core::message::MessageRole;
use task_core::notify::{MAX_NOTIFY_ATTEMPTS, NotificationId, NotificationKind};
use task_core::report::{ReportFilter, ReportKind, support_kind};
use task_core::{
    ListFilter, ListOrder, MilestoneStatus, Notification, ProjectStatus, Status, StoreError, TaskStore,
};
use time::OffsetDateTime;

/// `[notify] interval_secs` の既定（ADR-0037 D3）。
pub const DEFAULT_INTERVAL_SECS: u64 = 30;
/// webhook への POST のタイムアウト（ADR-0037 D3）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Discord の 1 通の上限（2000 字）より少し手前で切る。
pub const CONTENT_MAX_CHARS: usize = 1900;
/// 文面に載せる質問・見出しの字数（ADR-0037 D1「先頭 120 字」）。
pub const EXCERPT_CHARS: usize = 120;
/// 1 tick で見る案件の上限（案件はそう多くない。暴走しないための上限）。
const PROJECT_SCAN: usize = 200;
/// 1 案件あたりに見るタスクの上限。
const TASK_SCAN: usize = 1_000;
/// 悪い知らせを探すときに見る報告の件数。
const REPORT_SCAN: usize = 200;
/// 秘書の返事を探すときに読む対話の件数。
const MESSAGE_SCAN: usize = 50;
/// 送信の本文に付ける名前（ADR-0037 D3）。
const WEBHOOK_USERNAME: &str = "taskd";

/// `[notify]`（ADR-0037 D2 / D3）。秘密の id と間隔とリンクの根だけを持つ。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotifyConfig {
    /// `[secrets]` の中の webhook URL の id。無ければ何も送らない（エラーにしない）。
    #[serde(default = "default_webhook_secret")]
    pub discord_webhook_secret: String,
    /// 判定と送信を行う間隔（秒）。
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// 例: `"http://192.168.1.103:7700"`。無ければ文面にリンクを入れない。
    #[serde(default)]
    pub gui_base_url: Option<String>,
}

fn default_webhook_secret() -> String {
    task_core::notify::DEFAULT_WEBHOOK_SECRET_ID.to_string()
}

fn default_interval_secs() -> u64 {
    DEFAULT_INTERVAL_SECS
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            discord_webhook_secret: default_webhook_secret(),
            interval_secs: default_interval_secs(),
            gui_base_url: None,
        }
    }
}

impl NotifyConfig {
    /// 末尾の `/` を落とした GUI の根（空文字列は「無い」とみなす）。
    pub fn base_url(&self) -> Option<&str> {
        self.gui_base_url
            .as_deref()
            .map(|u| u.trim_end_matches('/'))
            .filter(|u| !u.is_empty())
    }
}

/// 判定が見つけた 1 件（まだ DB には入っていない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: NotificationKind,
    pub key: String,
    pub body: String,
}

/// 送信の結果（spawn した先から tick ループへ戻る）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendResult {
    pub id: NotificationId,
    /// 送れたか。
    pub ok: bool,
    /// 失敗の理由（**URL・ホスト名は含まない**）。
    pub error: Option<String>,
}

// ---- 文面の部品（決定的。LLM は呼ばない）----

/// 文字数で切って `…` を付ける（バイトではない）。
fn excerpt(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max).collect();
    format!("{head}…")
}

/// Discord の 1 通に収める。
fn clamp_content(text: &str) -> String {
    if text.chars().count() <= CONTENT_MAX_CHARS {
        return text.to_string();
    }
    text.chars().take(CONTENT_MAX_CHARS).collect()
}

fn link(base: Option<&str>, path: &str) -> String {
    match base {
        Some(base) => format!("\n{base}{path}"),
        None => String::new(),
    }
}

/// 組織のノードの表示名（知らない id はそのまま）。
fn node_name(org: &[task_core::OrgNode], id: &str) -> String {
    org.iter()
        .find(|n| n.id == id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| id.to_string())
}

// ---- 判定（ADR-0037 D1 の 5 種。決定的）----

/// tick ごとに DB を読み、「人の判断が要る」条件に当たるものを全部返す（重複排除はまだしない）。
/// 並びは `NotificationKind::ALL` の順、同じ種の中は id の昇順で決定的。
pub fn scan(store: &dyn TaskStore, base_url: Option<&str>) -> Result<Vec<Candidate>, StoreError> {
    let org = store.org_list()?;
    let mut out = Vec::new();
    out.extend(scan_milestone_ready(store, base_url)?);
    out.extend(scan_approval_pending(store, &org, base_url)?);
    out.extend(scan_question_blocked(store, &org, base_url)?);
    out.extend(scan_bad_news(store, base_url)?);
    out.extend(scan_secretary_reply(store, &org, base_url)?);
    Ok(out)
}

/// 途中目標に属する仕事（裏方を除く）が 1 件以上あり、すべて終端で、途中目標がまだ `reached` でない。
fn scan_milestone_ready(store: &dyn TaskStore, base_url: Option<&str>) -> Result<Vec<Candidate>, StoreError> {
    let mut out = Vec::new();
    let mut projects = store.project_list()?;
    projects.sort_by_key(|a| a.id);
    for project in projects.into_iter().take(PROJECT_SCAN) {
        let filter = ListFilter {
            project_id: Some(project.id),
            ..ListFilter::default()
        };
        let page = store.list_page(&filter, ListOrder::CreatedDesc, None, TASK_SCAN)?;
        let mut milestones = store.milestone_list(project.id)?;
        milestones.sort_by_key(|a| a.id);
        for milestone in milestones {
            if milestone.status == MilestoneStatus::Reached {
                continue;
            }
            let tasks: Vec<_> = page
                .items
                .iter()
                .filter(|t| t.milestone_id == Some(milestone.id) && support_kind(t).is_none())
                .collect();
            if tasks.is_empty() || !tasks.iter().all(|t| t.status.is_terminal()) {
                continue;
            }
            let done = tasks.iter().filter(|t| t.status == Status::Done).count();
            let failed = tasks.iter().filter(|t| t.status == Status::Failed).count();
            let body = format!(
                "途中目標『{}』の仕事が終わりました（done {done} / failed {failed}）。達成の判定と次の Go をお願いします。{}",
                milestone.title,
                link(base_url, &format!("/projects/{}", project.id))
            );
            out.push(Candidate {
                kind: NotificationKind::MilestoneReady,
                key: milestone.id.to_string(),
                body,
            });
        }
    }
    Ok(out)
}

/// 未決の認可（`decision IS NULL`）。
fn scan_approval_pending(
    store: &dyn TaskStore,
    org: &[task_core::OrgNode],
    base_url: Option<&str>,
) -> Result<Vec<Candidate>, StoreError> {
    let mut out = Vec::new();
    let mut pending = store.approval_list(Some(true), None, None)?;
    pending.sort_by_key(|a| a.id);
    for approval in pending {
        let body = format!(
            "認可の要求: {}『{}』{}",
            node_name(org, &approval.node_id),
            excerpt(&approval.question, EXCERPT_CHARS),
            link(base_url, "/approvals")
        );
        out.push(Candidate {
            kind: NotificationKind::ApprovalPending,
            key: approval.id.to_string(),
            body,
        });
    }
    Ok(out)
}

/// `blocked`（人への質問）のタスク。同じ `task_id` の認可があるものは `approval_pending` に任せる。
fn scan_question_blocked(
    store: &dyn TaskStore,
    org: &[task_core::OrgNode],
    base_url: Option<&str>,
) -> Result<Vec<Candidate>, StoreError> {
    let with_approval: HashSet<String> = store
        .approval_list(None, None, None)?
        .into_iter()
        .filter_map(|a| a.task_id.map(|t| t.to_string()))
        .collect();
    let filter = ListFilter {
        statuses: vec![Status::Blocked],
        ..ListFilter::default()
    };
    let page = store.list_page(&filter, ListOrder::CreatedDesc, None, TASK_SCAN)?;
    let mut tasks = page.items;
    tasks.sort_by_key(|a| a.id);
    let mut out = Vec::new();
    for task in tasks {
        if with_approval.contains(&task.id.to_string()) {
            continue;
        }
        let who = task
            .assignee
            .as_deref()
            .map(|id| node_name(org, id))
            .unwrap_or_else(|| "担当".to_string());
        let where_to = match task.project_id {
            Some(project_id) => link(base_url, &format!("/projects/{project_id}")),
            None => String::new(),
        };
        let body = format!(
            "{who} が質問で止まっています: {}{where_to}",
            excerpt(&task.title, EXCERPT_CHARS)
        );
        out.push(Candidate {
            kind: NotificationKind::QuestionBlocked,
            key: task.id.to_string(),
            body,
        });
    }
    Ok(out)
}

/// 秘書レベル（level 0）の `bad_news` 報告。
fn scan_bad_news(store: &dyn TaskStore, base_url: Option<&str>) -> Result<Vec<Candidate>, StoreError> {
    let filter = ReportFilter {
        level: Some(0),
        limit: REPORT_SCAN,
        ..ReportFilter::default()
    };
    let mut reports: Vec<_> = store
        .report_list(&filter)?
        .into_iter()
        .filter(|r| r.kind == ReportKind::BadNews)
        .collect();
    reports.sort_by_key(|a| a.id);
    Ok(reports
        .into_iter()
        .map(|report| Candidate {
            kind: NotificationKind::BadNews,
            key: report.id.to_string(),
            body: format!(
                "悪い知らせ: {}{}",
                excerpt(&report.headline, EXCERPT_CHARS),
                link(base_url, "/reports")
            ),
        })
        .collect())
}

/// `proposed` の案件に、組織のノードの返事（`role = node`）が付いた（人の返事待ち）。
fn scan_secretary_reply(
    store: &dyn TaskStore,
    org: &[task_core::OrgNode],
    base_url: Option<&str>,
) -> Result<Vec<Candidate>, StoreError> {
    let mut projects: Vec<_> = store
        .project_list()?
        .into_iter()
        .filter(|p| p.status == ProjectStatus::Proposed)
        .collect();
    projects.sort_by_key(|a| a.id);
    let mut out = Vec::new();
    for project in projects.into_iter().take(PROJECT_SCAN) {
        let mut replied = false;
        for node in org {
            let messages = store.message_list(&node.id, Some(project.id), MESSAGE_SCAN)?;
            if messages.iter().any(|m| m.role == MessageRole::Node) {
                replied = true;
                break;
            }
        }
        if !replied {
            continue;
        }
        out.push(Candidate {
            kind: NotificationKind::SecretaryReply,
            key: project.id.to_string(),
            body: format!(
                "秘書から『{}』の方針の提案が届きました。返事をお願いします。{}",
                excerpt(&project.title, EXCERPT_CHARS),
                link(base_url, &format!("/projects/{}", project.id))
            ),
        });
    }
    Ok(out)
}

/// 判定して、まだ知らせていない `(kind, key)` を pending として登録する。作った行を返す
/// （**2 回目の tick では何も作らない**: 重複排除は `notifications` の `UNIQUE(kind, key)`）。
pub fn schedule(
    store: &dyn TaskStore,
    config: &NotifyConfig,
    now: OffsetDateTime,
) -> Result<Vec<Notification>, StoreError> {
    let mut created = Vec::new();
    for candidate in scan(store, config.base_url())? {
        if let Some(row) =
            store.notification_upsert_pending(candidate.kind, &candidate.key, &candidate.body, now)?
        {
            created.push(row);
        }
    }
    Ok(created)
}

// ---- 送信（ADR-0037 D3）----

/// ADR-0030 D1 の流儀で `[secrets] dir` から webhook の URL を読む。無ければ `None`
/// （**値もパスもログに出さない**。秘密が無いのは異常ではないので警告も出さない）。
pub fn webhook_url(secrets_dir: Option<&Path>, secret_id: &str) -> Option<String> {
    let dir = secrets_dir?;
    let text = std::fs::read_to_string(dir.join(secret_id)).ok()?;
    let value = text.trim_end_matches(['\n', '\r']).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// `reqwest::Error` の `Display` は URL を含むので使わない。種別だけの短い文にする。
fn safe_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else if e.is_body() || e.is_decode() {
        "bad response body".to_string()
    } else if e.is_request() {
        "invalid request".to_string()
    } else {
        "request failed".to_string()
    }
}

/// webhook の URL へ 1 通 POST する。返すのは**種別だけ**の短いエラー（URL・ホスト名は入らない）。
pub async fn post_webhook(client: &reqwest::Client, url: &str, content: &str) -> Result<(), String> {
    let payload = serde_json::json!({
        "content": clamp_content(content),
        "username": WEBHOOK_USERNAME,
    });
    let response = client
        .post(url)
        .json(&payload)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| safe_error(&e))?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("http status {}", status.as_u16()))
    }
}

/// tick をブロックせずに 1 件送る（B1）。結果は `tx` に流れ、**次の tick** で `notification_mark` される。
pub fn spawn_send(
    client: reqwest::Client,
    url: String,
    notification: &Notification,
    tx: tokio::sync::mpsc::Sender<SendResult>,
) {
    let id = notification.id;
    let content = notification.body.clone();
    tokio::spawn(async move {
        let result = post_webhook(&client, &url, &content).await;
        let send_result = match result {
            Ok(()) => SendResult { id, ok: true, error: None },
            Err(error) => SendResult { id, ok: false, error: Some(error) },
        };
        let _ = tx.send(send_result).await;
    });
}

/// 送信の結果 1 件を台帳に書く（`tick_loop` が次の tick の先頭で呼ぶ）。
/// 3 回目の失敗で諦める（`ok = false`）。
pub fn record(
    store: &dyn TaskStore,
    pending: &[Notification],
    result: &SendResult,
    now: OffsetDateTime,
) -> Result<(), StoreError> {
    if result.ok {
        store.notification_mark(result.id, Some(true), None, now)?;
        return Ok(());
    }
    // 直前に読んだ pending の `attempts` で「これが最後の試行か」を決める（決定的）。
    let attempts_before = pending.iter().find(|n| n.id == result.id).map(|n| n.attempts).unwrap_or(0);
    let give_up = attempts_before + 1 >= MAX_NOTIFY_ATTEMPTS;
    let error = result.error.as_deref();
    store.notification_mark(result.id, None, error, now)?;
    if give_up {
        let reason = match error {
            Some(e) => format!("gave up after {MAX_NOTIFY_ATTEMPTS} attempts: {e}"),
            None => format!("gave up after {MAX_NOTIFY_ATTEMPTS} attempts"),
        };
        store.notification_mark(result.id, Some(false), Some(&reason), now)?;
    }
    Ok(())
}

/// `POST /notify/test` で送る定型文（ADR-0037 D4）。
pub const TEST_CONTENT: &str = "taskd のテスト送信です。ここに「人の判断が要るとき」だけ通知が届きます。";

/// `POST /notify/test` の結果（ADR-0037 D4）。**どの枝にも URL・ホスト名は入らない**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestSend {
    /// 送れた。
    Sent,
    /// 送り先には届いたが失敗した（種別だけの短い理由）。
    Failed(String),
    /// 秘密が無い・HTTP クライアントが無い（API は 409 `notify_unavailable`）。
    NotConfigured(String),
}

/// テスト送信 1 回（`AdminRequest::NotifyTest` の中身）。秘密を読むのも POST するのも taskd 側。
pub async fn send_test(
    client: Option<&reqwest::Client>,
    secrets_dir: Option<&Path>,
    secret_id: &str,
) -> TestSend {
    let Some(client) = client else {
        return TestSend::NotConfigured(
            "the HTTP client could not be created, so nothing can be sent".to_string(),
        );
    };
    let Some(url) = webhook_url(secrets_dir, secret_id) else {
        return TestSend::NotConfigured(format!(
            "no Discord webhook is registered; add it as the secret `{secret_id}`"
        ));
    };
    match post_webhook(client, &url, TEST_CONTENT).await {
        Ok(()) => TestSend::Sent,
        Err(detail) => TestSend::Failed(detail),
    }
}

/// 秘密が無いときに pending を畳む理由（GUI の「直近の送信」に出る。URL は含まない）。
pub const NOT_CONFIGURED: &str = "discord webhook is not configured";

/// 秘密が無い間は**送らず、pending も溜めない**（ADR-0037 D2）。判定はしたが送れなかったことを
/// 台帳に残して畳む（後から秘密を登録しても、その間の出来事は蒸し返さない）。
pub fn discard_pending(
    store: &dyn TaskStore,
    pending: &[Notification],
    now: OffsetDateTime,
) -> Result<usize, StoreError> {
    let mut discarded = 0;
    for row in pending {
        if store.notification_mark(row.id, Some(false), Some(NOT_CONFIGURED), now)? {
            discarded += 1;
        }
    }
    Ok(discarded)
}

/// 10 秒のタイムアウトを持つ HTTP クライアント（rustls。ADR-0037 D3）。組み立てに失敗したら
/// `None`（通知だけが止まり、taskd は動き続ける）。
pub fn client() -> Option<reqwest::Client> {
    match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("taskd/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => Some(client),
        Err(e) => {
            tracing::warn!(error = %e, "notify: cannot build the HTTP client; notifications are disabled");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_cuts_by_characters_and_flattens_newlines() {
        assert_eq!(excerpt("あいうえお", 3), "あいう…");
        assert_eq!(excerpt("あいう", 3), "あいう");
        assert_eq!(excerpt("a\n b", 10), "a b");
    }

    #[test]
    fn links_are_omitted_without_a_base_url() {
        assert_eq!(link(None, "/approvals"), "");
        assert_eq!(link(Some("http://h:7700"), "/approvals"), "\nhttp://h:7700/approvals");
    }

    #[test]
    fn base_url_drops_the_trailing_slash_and_treats_empty_as_absent() {
        let config = NotifyConfig {
            gui_base_url: Some("http://h:7700/".into()),
            ..NotifyConfig::default()
        };
        assert_eq!(config.base_url(), Some("http://h:7700"));
        let empty = NotifyConfig {
            gui_base_url: Some(String::new()),
            ..NotifyConfig::default()
        };
        assert_eq!(empty.base_url(), None);
        assert_eq!(NotifyConfig::default().base_url(), None);
    }

    #[test]
    fn defaults_match_the_adr() {
        let config = NotifyConfig::default();
        assert_eq!(config.discord_webhook_secret, "discord-webhook");
        assert_eq!(config.interval_secs, 30);
        assert!(config.gui_base_url.is_none());
    }

    #[test]
    fn content_is_clamped_to_one_discord_message() {
        let long = "あ".repeat(5_000);
        assert_eq!(clamp_content(&long).chars().count(), CONTENT_MAX_CHARS);
    }

    #[test]
    fn webhook_url_is_none_without_a_secrets_dir_or_file() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        assert_eq!(webhook_url(None, "discord-webhook"), None);
        assert_eq!(webhook_url(Some(dir.path()), "discord-webhook"), None);
        std::fs::write(dir.path().join("discord-webhook"), "https://example.invalid/hook\n")
            .unwrap_or_else(|e| panic!("write: {e}"));
        assert_eq!(
            webhook_url(Some(dir.path()), "discord-webhook").as_deref(),
            Some("https://example.invalid/hook")
        );
        // 空の秘密は「無い」と同じ扱い。
        std::fs::write(dir.path().join("empty"), "\n").unwrap_or_else(|e| panic!("write: {e}"));
        assert_eq!(webhook_url(Some(dir.path()), "empty"), None);
    }
}
