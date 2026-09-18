//! 報告（ADR-0033 D3。SPEC §2.4 / §3.5）。
//!
//! 下から上へ流れる「報告」の型・決定的な組み立て・`reports` 表の読み書きをここに置く。
//!
//! - **生成は決定的**（DESIGN 原則 1）。run の終端（`done` / `error` / `question`）から、結果ファイルの
//!   `summary` / `evidence` と成果物の一覧だけで文面を組む。LLM は呼ばない。
//! - **圧縮だけ LLM**。親ノードの報告は「まとめの run」（`Check::Reviewer` と同じ別 run）の `done` から作る。
//!   その run を起こす判断（何件たまったか、何時間たったか）は決定的で、ここの純粋関数が決める。
//! - **悪い知らせは圧縮を待たない**。`BadNews` は生成時にそのまま各祖先へ複製する（SPEC §2.4）。
//!
//! `store.rs` は `TaskStore` の supertrait として `ReportStore` を要求するだけで、実装（SQL）はこのモジュールにある。

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::message::{is_conversation, is_milestone_review};
use crate::model::{Task, TaskId, TaskKind};
use crate::org::{OrgKind, OrgNode, ProjectId};
use crate::store::{SqliteStore, StoreError, format_rfc3339, parse_rfc3339};

/// 圧縮（まとめ）の run に付ける役割名。`[[roles]]` に無くてよい（役割の既定が引けないだけ）。
/// ディスパッチャはこの値で「まとめの run」を見分け、その `done` を親ノードの報告にする。
pub const COMPACTION_ROLE: &str = "report-compressor";

/// GUI 監査 H4（Phase 29）: 裏方タスクの印。`TaskSummary.support` / `ProjectTaskView.support` に写す。
/// 判定は決定的で優先順あり: 途中目標レビュー（対話 + `milestone_id`。ADR-0038 D1）> 対話 >
/// 計画（`kind == plan`。ADR-0038 D1 / Phase 42）> 圧縮（`role == report-compressor`）>
/// 承認（`kind == approval`）> 合成レビュー（`kind == review`）。どれでもなければ `None`
/// （人が見る「仕事の木」の本体）。
///
/// Phase 42（実機 2026-09-18）: 計画 run（`kind = plan`）は裏方。人が見る仕事の木にも
/// `ready_milestones`（`crate::report::support_kind` を使う taskd 側）の件数にも入れない。
/// 入れたままだと、途中目標を分解した直後（計画 run が `done`、子は全部 `draft`）に
/// 「動いているものが無く done が 1 件以上」が成立し、人がまだ何も判定していないのに
/// 途中目標のレビューが再び起きてしまう。
pub fn support_kind(task: &Task) -> Option<&'static str> {
    if is_milestone_review(task) {
        Some("milestone_review")
    } else if is_conversation(task) {
        Some("conversation")
    } else if task.kind == TaskKind::Plan {
        Some("plan")
    } else if task.role.as_deref() == Some(COMPACTION_ROLE) {
        Some("compaction")
    } else if task.kind == TaskKind::Approval {
        Some("approval")
    } else if task.kind == TaskKind::Review {
        Some("review")
    } else {
        None
    }
}

/// `[reports] compress_after` の既定（ADR-0033 D3）。
pub const DEFAULT_COMPRESS_AFTER: usize = 4;
/// `[reports] compress_after_secs` の既定（2 時間。SPEC §3.5「通知は数時間単位」）。
pub const DEFAULT_COMPRESS_AFTER_SECS: u64 = 7_200;
/// 通知の間隔（2 時間）。`BadNews` だけはこれを待たない。
pub const NOTIFY_INTERVAL_SECS: i64 = 7_200;

/// `headline` の最大文字数（文字数で切る。バイトではない）。
pub const HEADLINE_MAX_CHARS: usize = 120;
/// 失敗の見出しに載せる `message` の文字数（ADR-0033 D3: 先頭 80 字）。
pub const ERROR_HEADLINE_MESSAGE_CHARS: usize = 80;

/// 報告の一意識別子（ULID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ReportId(#[schemars(with = "String")] pub Ulid);

impl ReportId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for ReportId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ReportId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ReportId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 報告の種類（ADR-0033 D3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    /// 途中経過。Phase 25 では作らない（run 単位の終端だけを上げる。通知は数時間単位なので run の途中は要らない）。
    Progress,
    /// 結果（run の `done`、および親のまとめ）。
    Result,
    /// 悪い知らせ（SPEC §2.4）。圧縮を待たず祖先へ複製される。
    BadNews,
    /// 提案（「この framing で論文が書けそう」）。Phase 25 では生成しない（まとめの本文に入る）。
    Proposal,
    /// 人への質問（run の `question`）。
    Question,
}

impl ReportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportKind::Progress => "progress",
            ReportKind::Result => "result",
            ReportKind::BadNews => "bad_news",
            ReportKind::Proposal => "proposal",
            ReportKind::Question => "question",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "progress" => Some(ReportKind::Progress),
            "result" => Some(ReportKind::Result),
            "bad_news" => Some(ReportKind::BadNews),
            "proposal" => Some(ReportKind::Proposal),
            "question" => Some(ReportKind::Question),
            _ => None,
        }
    }
}

/// 1 件の報告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    pub id: ReportId,
    /// 案件。`None` は「案件なし」（クラスタの障害など、案件に紐づかない悪い知らせ）。DB でも NULL
    /// （migration 0007 で NOT NULL を外した。ADR-0034 D1 の「将来」の項）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// 報告した組織のノード（`org_nodes.id`）。
    pub node_id: String,
    /// 元になったタスク（まとめの報告では、そのまとめの run のタスク）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    pub kind: ReportKind,
    /// 組織の木の深さ（秘書 = 0）。GUI は `level = 0` を「人が見る報告」として扱う。
    pub level: u32,
    pub headline: String,
    #[serde(default)]
    pub body: String,
    /// 元になった報告の id（まとめなら子の報告、悪い知らせの複製なら 1 段下の報告）。
    #[serde(default)]
    pub sources: Vec<ReportId>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "time::serde::rfc3339::option")]
    #[schemars(with = "Option<String>")]
    pub read_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
}

/// `report_list` の絞り込み（既定は絞り込み無し）。
#[derive(Debug, Clone)]
pub struct ReportFilter {
    pub project_id: Option<ProjectId>,
    pub node_id: Option<String>,
    pub level: Option<u32>,
    /// true なら未読（`read_at IS NULL`）だけ。
    pub unread_only: bool,
    pub limit: usize,
}

impl Default for ReportFilter {
    fn default() -> Self {
        Self {
            project_id: None,
            node_id: None,
            level: None,
            unread_only: false,
            limit: 100,
        }
    }
}

/// 未読の件数（`DaemonSnapshot.reports` と GUI の通知判定に使う観測値。ADR-0033 D3）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportsLive {
    /// 秘書レベル（`level = 0`）の未読の件数。
    pub unread_secretary: u32,
    /// そのうち `bad_news` の件数（0 でなければ即通知）。
    pub unread_bad_news: u32,
    /// 前回 GUI が通知した時刻（RFC 3339）。`POST /reports/notified` が進める。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_notified_at: Option<String>,
    /// 「前回の通知から 2 時間以上経ち未読がある」か「`bad_news` の未読がある」。
    pub notify_now: bool,
}

// ---- 決定的な組み立て（LLM を呼ばない。DESIGN 原則 1）----

/// 文字数で切る（末尾に `…` は付けない。機械可読な見出しにするため）。
pub fn truncate_chars(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect()
}

/// 最初の非空行（見出しに使う）。
pub fn first_line(s: &str) -> &str {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// 組織の木の深さ（秘書 = 0）。知らないノード・壊れた親の連鎖は 0 を返す。
pub fn level_of(org: &[OrgNode], node_id: &str) -> u32 {
    u32::try_from(ancestors_of(org, node_id).len()).unwrap_or(0)
}

/// 直近の親から秘書までの祖先（下から上へ）。知らないノードは空。
pub fn ancestors_of(org: &[OrgNode], node_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(mut current) = org.iter().find(|n| n.id == node_id) else {
        return out;
    };
    while let Some(parent_id) = current.parent_id.as_deref() {
        let Some(parent) = org.iter().find(|n| n.id == parent_id) else {
            break;
        };
        if parent.id == node_id || out.iter().any(|id| id == parent_id) {
            // 壊れたデータ（親の連鎖が閉じている）でも無限に辿らない。
            break;
        }
        out.push(parent.id.clone());
        current = parent;
    }
    out
}

/// 案件に紐づかない悪い知らせ（クラスタが落ちた等）を受けるノード:
/// `kind = department` かつ id が `infra` のノード、無ければ秘書（ADR-0033 D3）。
pub fn infra_node(org: &[OrgNode]) -> Option<&OrgNode> {
    org.iter()
        .find(|n| n.kind == OrgKind::Department && n.id == "infra")
        .or_else(|| org.iter().find(|n| n.kind == OrgKind::Secretary))
}

/// 報告の骨（`created_at` は呼び出し側が決められるようにしてある）。
#[allow(clippy::too_many_arguments)]
fn new_report(
    node_id: &str,
    level: u32,
    project_id: Option<ProjectId>,
    task_id: Option<TaskId>,
    kind: ReportKind,
    headline: String,
    body: String,
    sources: Vec<ReportId>,
    now: OffsetDateTime,
) -> Report {
    Report {
        id: ReportId::new(),
        project_id,
        node_id: node_id.to_string(),
        task_id,
        kind,
        level,
        headline,
        body,
        sources,
        read_at: None,
        created_at: now,
    }
}

/// run が `done` で終わったときの報告（`kind = result`）。
/// 見出しは結果ファイルの `summary` の 1 行目、本文は `summary` + `evidence` + 成果物の一覧。
#[allow(clippy::too_many_arguments)]
pub fn report_for_done(
    node_id: &str,
    level: u32,
    project_id: Option<ProjectId>,
    task_id: TaskId,
    task_title: &str,
    summary: &str,
    evidence: &[String],
    artifacts: &[String],
    sources: Vec<ReportId>,
    now: OffsetDateTime,
) -> Report {
    let head = first_line(summary);
    let headline = if head.is_empty() {
        truncate_chars(&format!("{task_title} が完了"), HEADLINE_MAX_CHARS)
    } else {
        truncate_chars(head, HEADLINE_MAX_CHARS)
    };
    let mut body = format!("タスク: {task_title}\n");
    if !summary.trim().is_empty() {
        body.push('\n');
        body.push_str(summary.trim());
        body.push('\n');
    }
    if !evidence.is_empty() {
        body.push_str("\n確認できた結果:\n");
        for line in evidence {
            body.push_str(&format!("- {line}\n"));
        }
    }
    if !artifacts.is_empty() {
        body.push_str("\n成果物:\n");
        for a in artifacts {
            body.push_str(&format!("- {a}\n"));
        }
    }
    new_report(
        node_id,
        level,
        project_id,
        Some(task_id),
        ReportKind::Result,
        headline,
        body,
        sources,
        now,
    )
}

/// run が `error` で終わったときの報告（`kind = bad_news`）。
#[allow(clippy::too_many_arguments)]
pub fn report_for_error(
    node_id: &str,
    level: u32,
    project_id: Option<ProjectId>,
    task_id: TaskId,
    task_title: &str,
    message: &str,
    retryable: bool,
    now: OffsetDateTime,
) -> Report {
    let headline = format!(
        "{task_title} が失敗: {}",
        truncate_chars(message, ERROR_HEADLINE_MESSAGE_CHARS)
    );
    let body = format!(
        "タスク: {task_title}\n\n理由:\n{}\n\nretryable: {retryable}\n",
        message.trim()
    );
    new_report(
        node_id,
        level,
        project_id,
        Some(task_id),
        ReportKind::BadNews,
        headline,
        body,
        Vec::new(),
        now,
    )
}

/// run が `question` で終わったときの報告（`kind = question`。本文はそのまま）。
pub fn report_for_question(
    node_id: &str,
    level: u32,
    project_id: Option<ProjectId>,
    task_id: TaskId,
    task_title: &str,
    text: &str,
    now: OffsetDateTime,
) -> Report {
    let headline = truncate_chars(first_line(text), HEADLINE_MAX_CHARS);
    let body = format!("タスク: {task_title}\n\n{}\n", text.trim());
    new_report(
        node_id,
        level,
        project_id,
        Some(task_id),
        ReportKind::Question,
        headline,
        body,
        Vec::new(),
        now,
    )
}

/// クラスタに接続できないこと（`Event::ClusterUnavailable`）の報告（`kind = bad_news`、案件なし）。
pub fn report_for_cluster_unavailable(
    node_id: &str,
    level: u32,
    cluster: &str,
    host: &str,
    reason: &str,
    task_id: Option<TaskId>,
    now: OffsetDateTime,
) -> Report {
    let headline = truncate_chars(&format!("{host} に接続できない"), HEADLINE_MAX_CHARS);
    let body = format!(
        "クラスタ {cluster}（{host}）に ssh の多重接続がありません。\n\n理由:\n{}\n\nログインし直してください。\n",
        reason.trim()
    );
    new_report(
        node_id,
        level,
        None,
        task_id,
        ReportKind::BadNews,
        headline,
        body,
        Vec::new(),
        now,
    )
}

/// 悪い知らせを祖先へ複製する（SPEC §2.4「目立つ形で届く」）。
///
/// 各段のコピーの `sources` は**1 段下の報告の id**（最初のコピーは元の報告）。こうすると
/// 「どの `sources` にも入っていない」＝レビュー待ち、という規則がそのまま成り立ち、
/// 悪い知らせが圧縮の対象として二重に上がることがない。
pub fn bad_news_chain(original: &Report, org: &[OrgNode], now: OffsetDateTime) -> Vec<Report> {
    let mut out = Vec::new();
    let mut source = original.id;
    for (i, ancestor) in ancestors_of(org, &original.node_id).into_iter().enumerate() {
        let level = level_of(org, &ancestor);
        let copy = Report {
            id: ReportId::new(),
            project_id: original.project_id,
            node_id: ancestor,
            task_id: original.task_id,
            kind: ReportKind::BadNews,
            level,
            headline: original.headline.clone(),
            body: original.body.clone(),
            sources: vec![source],
            read_at: None,
            // 同じ時刻だと並びが不定になるので、下から上へ 1 秒ずつずらす。
            created_at: now + time::Duration::seconds(i64::try_from(i).unwrap_or(0) + 1),
        };
        source = copy.id;
        out.push(copy);
    }
    out
}

/// 圧縮（まとめの run）を起こすか（ADR-0033 D3）。件数が閾値以上、または最古が `after_secs` 以上経過。
pub fn compaction_due(pending: &[Report], now: OffsetDateTime, after_count: usize, after_secs: u64) -> bool {
    if pending.is_empty() {
        return false;
    }
    if pending.len() >= after_count.max(1) {
        return true;
    }
    match pending.iter().map(|r| r.created_at).min() {
        Some(oldest) => (now - oldest).whole_seconds() >= i64::try_from(after_secs).unwrap_or(i64::MAX),
        None => false,
    }
}

/// まとめの run に渡す `objective`（子の報告を全部並べ、上司としての書き方を指示する）。
/// **決定的な文字列**で、LLM はこれを読んで 1 件にまとめるだけ。
pub fn compaction_objective(node: &OrgNode, pending: &[Report]) -> String {
    let mut out = format!(
        "あなたは「{}」（{}）です。部下から上がってきた次の報告を、上司として 1 件にまとめてください。\n",
        node.name,
        node.kind.as_str()
    );
    if !node.brief.trim().is_empty() {
        out.push_str(&format!("担当: {}\n", node.brief.trim()));
    }
    out.push_str("\n---- 部下からの報告 ----\n");
    for (i, r) in pending.iter().enumerate() {
        out.push_str(&format!(
            "\n[{}] {} / {} / {}\n{}\n",
            i + 1,
            r.node_id,
            r.kind.as_str(),
            r.headline,
            r.body.trim()
        ));
    }
    out.push_str(
        "\n---- まとめ方 ----\n\
         分かったこと・確認できた結果・次にできそうなこと（論文の framing 等）、の順に、3〜8 行で書いてください。\n\
         数字と証拠のある事実を優先し、推測はそうと分かるように書いてください。\n\
         悪い知らせは既に上に個別で届いているので、ここではまとめない（監査 L-2）。\n\
         まとめた文章を `artifacts/result.json` の `summary` に入れて終わってください（この run の成果物はそれだけです）。\n",
    );
    out
}

/// GUI に通知させるか（決定的。ADR-0033 D3 / SPEC §3.5「通知は数時間単位」）。
pub fn notify_now(
    unread_secretary: u32,
    unread_bad_news: u32,
    last_notified_at: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> bool {
    if unread_bad_news > 0 {
        return true;
    }
    if unread_secretary == 0 {
        return false;
    }
    match last_notified_at {
        None => true,
        Some(last) => (now - last).whole_seconds() >= NOTIFY_INTERVAL_SECS,
    }
}

// ---- `reports` 表の読み書き（SQL はここだけ。`store.rs` は supertrait で要求するだけ）----

/// ADR-0033 D3: 報告の追記・一覧・既読・レビュー待ちの取り出し。`TaskStore` の supertrait で、
/// 実装は `SqliteStore` のみ（ディスパッチャは `Arc<dyn TaskStore>` から呼ぶ）。
pub trait ReportStore: Send + Sync {
    /// 1 件追記する（報告は追記専用で、`read_at` 以外は後から変えない）。
    fn report_append(&self, report: &Report) -> Result<(), StoreError>;
    /// 複数件を 1 トランザクションで追記する（悪い知らせの複製を途中で落とさないため）。
    fn report_append_all(&self, reports: &[Report]) -> Result<(), StoreError>;
    fn report_get(&self, id: ReportId) -> Result<Option<Report>, StoreError>;
    /// 新しい順（`created_at` 降順、同値は id 降順）。
    fn report_list(&self, filter: &ReportFilter) -> Result<Vec<Report>, StoreError>;
    /// 既読にする（既に既読のものは触らない）。変えた件数を返す。
    fn report_mark_read(&self, ids: &[ReportId], at: OffsetDateTime) -> Result<usize, StoreError>;
    /// `parent_node_id` の子ノードの報告のうち、まだどの報告の `sources` にも入っていないもの（古い順）。
    fn report_unreviewed_children(&self, parent_node_id: &str) -> Result<Vec<Report>, StoreError>;
    /// `(その level の未読, そのうち bad_news)` の件数。
    fn report_unread_counts(&self, level: u32) -> Result<(u32, u32), StoreError>;
}

fn row_to_report(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Report, StoreError>> {
    let id: String = row.get(0)?;
    let project_id: Option<String> = row.get(1)?;
    let node_id: String = row.get(2)?;
    let task_id: Option<String> = row.get(3)?;
    let kind_col: String = row.get(4)?;
    let level: i64 = row.get(5)?;
    let headline: String = row.get(6)?;
    let body: String = row.get(7)?;
    let sources_col: String = row.get(8)?;
    let read_at: Option<String> = row.get(9)?;
    let created_at: String = row.get(10)?;
    let Ok(id) = id.parse::<ReportId>() else {
        return Ok(Err(StoreError::Invalid(format!("invalid report id: {id}"))));
    };
    let Some(kind) = ReportKind::parse(&kind_col) else {
        return Ok(Err(StoreError::Invalid(format!("invalid report kind: {kind_col}"))));
    };
    // 案件なしは NULL（migration 0007 で NOT NULL を外した。ADR-0034 D1 の「将来」の項）。
    let project_id = match project_id {
        None => None,
        Some(raw) => match raw.parse::<ProjectId>() {
            Ok(v) => Some(v),
            Err(_) => {
                return Ok(Err(StoreError::Invalid(format!("invalid report project_id: {raw}"))));
            }
        },
    };
    let task_id = match task_id {
        None => None,
        Some(raw) => match raw.parse::<TaskId>() {
            Ok(v) => Some(v),
            Err(_) => return Ok(Err(StoreError::Invalid(format!("invalid report task_id: {raw}")))),
        },
    };
    let sources: Vec<ReportId> = match serde_json::from_str::<Vec<String>>(&sources_col) {
        Ok(raw) => {
            let mut out = Vec::with_capacity(raw.len());
            for s in raw {
                match s.parse::<ReportId>() {
                    Ok(v) => out.push(v),
                    Err(_) => return Ok(Err(StoreError::Invalid(format!("invalid report source id: {s}")))),
                }
            }
            out
        }
        Err(e) => return Ok(Err(StoreError::Invalid(format!("invalid report sources: {e}")))),
    };
    Ok((|| {
        Ok(Report {
            id,
            project_id,
            node_id,
            task_id,
            kind,
            level: u32::try_from(level).unwrap_or(0),
            headline,
            body,
            sources,
            read_at: match read_at {
                Some(raw) => Some(parse_rfc3339(&raw)?),
                None => None,
            },
            created_at: parse_rfc3339(&created_at)?,
        })
    })())
}

const SELECT_REPORT: &str = "SELECT id, project_id, node_id, task_id, kind, level, headline, body, sources, \
                             read_at, created_at FROM reports";

fn insert_report_tx(conn: &Connection, report: &Report) -> Result<(), StoreError> {
    let sources: Vec<String> = report.sources.iter().map(|s| s.to_string()).collect();
    conn.execute(
        "INSERT INTO reports (id, project_id, node_id, task_id, kind, level, headline, body, sources, read_at, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            report.id.to_string(),
            report.project_id.map(|p| p.to_string()),
            report.node_id,
            report.task_id.map(|t| t.to_string()),
            report.kind.as_str(),
            i64::from(report.level),
            report.headline,
            report.body,
            serde_json::to_string(&sources)?,
            report.read_at.map(format_rfc3339).transpose()?,
            format_rfc3339(report.created_at)?,
        ],
    )?;
    Ok(())
}

impl ReportStore for SqliteStore {
    fn report_append(&self, report: &Report) -> Result<(), StoreError> {
        self.report_append_all(std::slice::from_ref(report))
    }

    fn report_append_all(&self, reports: &[Report]) -> Result<(), StoreError> {
        if reports.is_empty() {
            return Ok(());
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for report in reports {
            insert_report_tx(&tx, report)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn report_get(&self, id: ReportId) -> Result<Option<Report>, StoreError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                &format!("{SELECT_REPORT} WHERE id = ?1"),
                params![id.to_string()],
                row_to_report,
            )
            .optional()?;
        match row {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    fn report_list(&self, filter: &ReportFilter) -> Result<Vec<Report>, StoreError> {
        let mut where_sql = String::from(" WHERE 1 = 1");
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(project_id) = filter.project_id {
            where_sql.push_str(" AND project_id = ?");
            args.push(SqlValue::Text(project_id.to_string()));
        }
        if let Some(node_id) = filter.node_id.as_deref() {
            where_sql.push_str(" AND node_id = ?");
            args.push(SqlValue::Text(node_id.to_string()));
        }
        if let Some(level) = filter.level {
            where_sql.push_str(" AND level = ?");
            args.push(SqlValue::Integer(i64::from(level)));
        }
        if filter.unread_only {
            where_sql.push_str(" AND read_at IS NULL");
        }
        let limit = filter.limit.clamp(1, 1_000);
        let sql = format!("{SELECT_REPORT}{where_sql} ORDER BY created_at DESC, id DESC LIMIT {limit}");
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), row_to_report)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn report_mark_read(&self, ids: &[ReportId], at: OffsetDateTime) -> Result<usize, StoreError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let ts = format_rfc3339(at)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut changed = 0usize;
        for id in ids {
            changed += tx.execute(
                "UPDATE reports SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL",
                params![id.to_string(), ts],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    fn report_unreviewed_children(&self, parent_node_id: &str) -> Result<Vec<Report>, StoreError> {
        let sql = format!(
            "{SELECT_REPORT} WHERE node_id IN (SELECT id FROM org_nodes WHERE parent_id = ?1) \
             AND id NOT IN (SELECT je.value FROM reports s, json_each(s.sources) je) \
             ORDER BY created_at ASC, id ASC"
        );
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![parent_node_id], row_to_report)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn report_unread_counts(&self, level: u32) -> Result<(u32, u32), StoreError> {
        let conn = self.lock()?;
        let unread: i64 = conn.query_row(
            "SELECT COUNT(*) FROM reports WHERE level = ?1 AND read_at IS NULL",
            params![i64::from(level)],
            |row| row.get(0),
        )?;
        let bad: i64 = conn.query_row(
            "SELECT COUNT(*) FROM reports WHERE level = ?1 AND read_at IS NULL AND kind = 'bad_news'",
            params![i64::from(level)],
            |row| row.get(0),
        )?;
        Ok((
            u32::try_from(unread).unwrap_or(u32::MAX),
            u32::try_from(bad).unwrap_or(u32::MAX),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::OrgKind;

    fn node(id: &str, parent: Option<&str>, kind: OrgKind) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            name: id.to_string(),
            kind,
            genre: None,
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn org() -> Vec<OrgNode> {
        vec![
            node("secretary", None, OrgKind::Secretary),
            node("coding", Some("secretary"), OrgKind::Department),
            node("coding-poc", Some("coding"), OrgKind::Section),
            node("infra", Some("secretary"), OrgKind::Department),
        ]
    }

    #[test]
    fn level_and_ancestors_walk_up_to_the_secretary() {
        let org = org();
        assert_eq!(level_of(&org, "secretary"), 0);
        assert_eq!(level_of(&org, "coding"), 1);
        assert_eq!(level_of(&org, "coding-poc"), 2);
        assert_eq!(ancestors_of(&org, "coding-poc"), vec!["coding".to_string(), "secretary".to_string()]);
        assert!(ancestors_of(&org, "ghost").is_empty());
        assert_eq!(level_of(&org, "ghost"), 0);
    }

    #[test]
    fn infra_node_prefers_the_infra_department_then_the_secretary() {
        let org = org();
        assert_eq!(infra_node(&org).map(|n| n.id.as_str()), Some("infra"));
        let without_infra: Vec<OrgNode> = org.into_iter().filter(|n| n.id != "infra").collect();
        assert_eq!(infra_node(&without_infra).map(|n| n.id.as_str()), Some("secretary"));
    }

    #[test]
    fn done_error_and_question_have_their_own_kinds_and_wording() {
        let now = OffsetDateTime::now_utc();
        let task = TaskId::new();
        let done = report_for_done(
            "coding-poc",
            2,
            None,
            task,
            "PoC を書く",
            "ベンチが 1.8 倍速くなった",
            &["cargo test: exit 0".to_string()],
            &["bench.json (artifacts/bench.json)".to_string()],
            Vec::new(),
            now,
        );
        assert_eq!(done.kind, ReportKind::Result);
        assert_eq!(done.headline, "ベンチが 1.8 倍速くなった");
        assert!(done.body.contains("cargo test: exit 0"), "{}", done.body);
        assert!(done.body.contains("bench.json"), "{}", done.body);

        let long = "x".repeat(200);
        let err = report_for_error("coding-poc", 2, None, task, "PoC を書く", &long, false, now);
        assert_eq!(err.kind, ReportKind::BadNews);
        assert_eq!(err.headline, format!("PoC を書く が失敗: {}", "x".repeat(80)));
        assert!(err.body.contains("retryable: false"), "{}", err.body);

        let q = report_for_question("coding-poc", 2, None, task, "PoC を書く", "どのクラスタを使いますか\n（pegasus か sirius）", now);
        assert_eq!(q.kind, ReportKind::Question);
        assert_eq!(q.headline, "どのクラスタを使いますか");
        assert!(q.body.contains("pegasus"), "{}", q.body);
    }

    #[test]
    fn bad_news_is_copied_to_every_ancestor_up_to_the_secretary() {
        let now = OffsetDateTime::now_utc();
        let original = report_for_error("coding-poc", 2, None, TaskId::new(), "PoC", "落ちた", true, now);
        let copies = bad_news_chain(&original, &org(), now);
        assert_eq!(copies.len(), 2);
        assert_eq!(copies[0].node_id, "coding");
        assert_eq!(copies[0].level, 1);
        assert_eq!(copies[0].sources, vec![original.id]);
        assert_eq!(copies[1].node_id, "secretary");
        assert_eq!(copies[1].level, 0);
        assert_eq!(copies[1].sources, vec![copies[0].id]);
        assert!(copies.iter().all(|c| c.kind == ReportKind::BadNews && c.headline == original.headline));
    }

    #[test]
    fn compaction_is_due_on_the_count_or_on_the_age() {
        let now = OffsetDateTime::now_utc();
        let make = |at: OffsetDateTime| report_for_done("coding-poc", 2, None, TaskId::new(), "t", "s", &[], &[], Vec::new(), at);
        let three: Vec<Report> = (0..3).map(|_| make(now)).collect();
        assert!(!compaction_due(&three, now, 4, 7200));
        let four: Vec<Report> = (0..4).map(|_| make(now)).collect();
        assert!(compaction_due(&four, now, 4, 7200));
        let old = vec![make(now - time::Duration::seconds(7_201))];
        assert!(compaction_due(&old, now, 4, 7200));
        assert!(!compaction_due(&[], now, 4, 7200));
    }

    #[test]
    fn the_compaction_objective_contains_every_child_report() {
        let now = OffsetDateTime::now_utc();
        let a = report_for_done("coding-poc", 2, None, TaskId::new(), "A", "A の結果", &[], &[], Vec::new(), now);
        let b = report_for_question("coding-poc", 2, None, TaskId::new(), "B", "B を聞きたい", now);
        let text = compaction_objective(&node("coding", Some("secretary"), OrgKind::Department), &[a.clone(), b.clone()]);
        assert!(text.contains(&a.headline), "{text}");
        assert!(text.contains(&b.headline), "{text}");
        assert!(text.contains("3〜8 行"), "{text}");
    }

    #[test]
    fn notification_waits_two_hours_unless_there_is_bad_news() {
        let now = OffsetDateTime::now_utc();
        // 2 時間未満で未読 → 通知しない。
        assert!(!notify_now(3, 0, Some(now - time::Duration::minutes(30)), now));
        // 2 時間以上で未読 → 通知する。
        assert!(notify_now(3, 0, Some(now - time::Duration::hours(3)), now));
        // 悪い知らせは即時。
        assert!(notify_now(1, 1, Some(now - time::Duration::minutes(1)), now));
        // 未読が無ければ通知しない。
        assert!(!notify_now(0, 0, None, now));
    }

    // ---- `reports` 表（SQLite）----

    fn store_with_org() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("open");
        for n in org() {
            crate::store::TaskStore::org_upsert(&store, &n).expect("seed org");
        }
        store
    }

    fn sample(node_id: &str, level: u32, kind: ReportKind, project: Option<ProjectId>, at: OffsetDateTime) -> Report {
        Report {
            id: ReportId::new(),
            project_id: project,
            node_id: node_id.to_string(),
            task_id: None,
            kind,
            level,
            headline: format!("{node_id} の報告"),
            body: "body".into(),
            sources: Vec::new(),
            read_at: None,
            created_at: at,
        }
    }

    #[test]
    fn append_round_trips_including_a_report_without_a_project() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let project = ProjectId::new();
        let with_project = sample("coding-poc", 2, ReportKind::Result, Some(project), now);
        let without = sample("infra", 1, ReportKind::BadNews, None, now);
        store.report_append_all(&[with_project.clone(), without.clone()]).expect("append");
        assert_eq!(store.report_get(with_project.id).expect("get"), Some(with_project.clone()));
        let read_back = store.report_get(without.id).expect("get").expect("some");
        assert_eq!(read_back.project_id, None);
        assert_eq!(read_back, without);
        assert_eq!(store.report_get(ReportId::new()).expect("get"), None);
    }

    #[test]
    fn list_filters_by_project_node_level_and_unread_and_is_newest_first() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let project = ProjectId::new();
        let other = ProjectId::new();
        let old = sample("coding-poc", 2, ReportKind::Result, Some(project), now - time::Duration::hours(1));
        let new = sample("coding-poc", 2, ReportKind::Result, Some(project), now);
        let elsewhere = sample("coding", 1, ReportKind::Result, Some(other), now - time::Duration::minutes(30));
        store.report_append_all(&[old.clone(), new.clone(), elsewhere.clone()]).expect("append");

        let all = store.report_list(&ReportFilter::default()).expect("list");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, new.id, "newest first");

        let by_project = store
            .report_list(&ReportFilter { project_id: Some(project), ..ReportFilter::default() })
            .expect("list");
        assert_eq!(by_project.len(), 2);
        let by_node = store
            .report_list(&ReportFilter { node_id: Some("coding".into()), ..ReportFilter::default() })
            .expect("list");
        assert_eq!(by_node.iter().map(|r| r.id).collect::<Vec<_>>(), vec![elsewhere.id]);
        let by_level = store
            .report_list(&ReportFilter { level: Some(1), ..ReportFilter::default() })
            .expect("list");
        assert_eq!(by_level.len(), 1);
        let limited = store
            .report_list(&ReportFilter { limit: 1, ..ReportFilter::default() })
            .expect("list");
        assert_eq!(limited.len(), 1);

        assert_eq!(store.report_mark_read(&[new.id], now).expect("read"), 1);
        // 既読のものをもう一度既読にしても何も変わらない。
        assert_eq!(store.report_mark_read(&[new.id], now).expect("read"), 0);
        let unread = store
            .report_list(&ReportFilter { unread_only: true, ..ReportFilter::default() })
            .expect("list");
        assert_eq!(unread.len(), 2);
        assert!(unread.iter().all(|r| r.id != new.id));
        assert_eq!(store.report_unread_counts(2).expect("counts"), (1, 0));
    }

    #[test]
    fn unreviewed_children_skips_reports_that_are_already_a_source() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let a = sample("coding-poc", 2, ReportKind::Result, None, now - time::Duration::minutes(2));
        let b = sample("coding-poc", 2, ReportKind::Result, None, now - time::Duration::minutes(1));
        store.report_append_all(&[a.clone(), b.clone()]).expect("append");
        let pending = store.report_unreviewed_children("coding").expect("pending");
        assert_eq!(pending.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a.id, b.id], "oldest first");
        // 別の親（secretary）から見ると、coding の報告だけが対象（孫は見ない）。
        assert!(store.report_unreviewed_children("secretary").expect("pending").is_empty());

        // まとめの報告が a を取り込むと、a は次回の対象から外れる。
        let mut summary = sample("coding", 1, ReportKind::Result, None, now);
        summary.sources = vec![a.id];
        store.report_append(&summary).expect("append");
        let pending = store.report_unreviewed_children("coding").expect("pending");
        assert_eq!(pending.iter().map(|r| r.id).collect::<Vec<_>>(), vec![b.id]);
        // まとめ自身は secretary から見たレビュー対象になる。
        let up = store.report_unreviewed_children("secretary").expect("pending");
        assert_eq!(up.iter().map(|r| r.id).collect::<Vec<_>>(), vec![summary.id]);
    }

    #[test]
    fn a_bad_news_chain_leaves_only_the_top_copy_unreviewed() {
        let store = store_with_org();
        let now = OffsetDateTime::now_utc();
        let original = report_for_error("coding-poc", 2, None, TaskId::new(), "PoC", "落ちた", true, now);
        let mut all = vec![original.clone()];
        all.extend(bad_news_chain(&original, &org(), now));
        store.report_append_all(&all).expect("append");
        // 各段のコピーが 1 段下を sources に持つので、圧縮の対象にはならない。
        assert!(store.report_unreviewed_children("coding").expect("pending").is_empty());
        assert!(store.report_unreviewed_children("secretary").expect("pending").is_empty());
        assert_eq!(store.report_unread_counts(0).expect("counts"), (1, 1));
    }

    fn plain_task(kind: TaskKind) -> Task {
        use crate::model::{Budget, Check, Criterion, Status, Tier, WorkerHint, WorkspaceSpec};
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Human }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Draft,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: "ws".into() },
            budget: Budget { max_turns: 1, max_wall_secs: 1, max_retries: 0 },
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

    /// GUI 監査 H4（Phase 29）: 裏方タスクの印の優先順（対話 > 圧縮 > 承認 > 合成レビュー）。
    #[test]
    fn support_kind_classifies_background_tasks_by_a_fixed_priority() {
        let mut plain = plain_task(TaskKind::Execute);
        assert_eq!(support_kind(&plain), None, "人が見る本体の仕事には印を付けない");

        plain.conversation = Some(crate::message::MessageId::new());
        assert_eq!(support_kind(&plain), Some("conversation"));

        let mut compaction = plain_task(TaskKind::Execute);
        compaction.role = Some(COMPACTION_ROLE.to_string());
        assert_eq!(support_kind(&compaction), Some("compaction"));

        let approval = plain_task(TaskKind::Approval);
        assert_eq!(support_kind(&approval), Some("approval"));

        let review = plain_task(TaskKind::Review);
        assert_eq!(support_kind(&review), Some("review"));

        // Phase 42（実機 2026-09-18）: 計画 run は裏方。途中目標の「仕事」に数えない。
        let plan = plain_task(TaskKind::Plan);
        assert_eq!(support_kind(&plan), Some("plan"));

        // 対話が最優先（他の条件と重なっても対話が勝つ）。
        let mut both = plain_task(TaskKind::Approval);
        both.conversation = Some(crate::message::MessageId::new());
        assert_eq!(support_kind(&both), Some("conversation"));
    }
}
