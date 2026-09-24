//! ADR-0048 D1/D2（Phase 60a）: Console の一本の流れを組み立てる**決定的な**部品。
//!
//! ここにあるのは「並べる・束ねる・1 行にする」だけで、I/O も LLM も無い（DESIGN 原則 1）。
//! ストアを引くのは `task-api` の `console` 側（`GET /console` / `GET /console/stream`）で、
//! この module は引いてきた行を受け取って写すだけである。
//!
//! - `ConsoleCursor`: 時刻（ナノ秒）+ 同時刻の並びを決める `tie` + イベントの読み進み位置（`event_id`）。
//!   ブロックの出どころ（イベント・対話・認可・報告・途中目標）がばらばらなので、**どれにも付けられる
//!   1 本の順序**が要る。文字列に符号化して GUI へ渡し、`since` で戻ってくる。
//! - `group_progress`: `Event::WorkerProgress` を **run ごとに 1 件**へ束ねる（ADR-0048 D1 の表の
//!   `progress`）。見出しに要る数（件数・道具の回数・最後の `status`・始めと終わり）だけを持ち、
//!   全行は `GET /tasks/{id}/runs/{run}/events` で取る。
//! - `task_line`: `Event::Transitioned` を Console の 1 行（`task` ブロック）に写す。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{Event, EventRow, ProgressKind, Status, Task, TaskId};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// 折り畳んだ `progress` の見出しに出す「始めの数行」。
pub const PROGRESS_HEAD_LINES: usize = 3;
/// 同じく「終わりの数行」。
pub const PROGRESS_TAIL_LINES: usize = 3;

// ---- カーソル ----

/// Console の 1 本の順序（ADR-0048 D1）。
///
/// - `at_nanos`: ブロックの時刻（Unix エポックからのナノ秒）。並びはこれが第 1 キー。
/// - `tie`: 同時刻のときの並びを決める、出どころごとに一意な短い文字列（`e12` / `m<ULID>` など）。
/// - `event_id`: `events` テーブルをどこまで読んだか。対話や報告から作ったブロックでも
///   **そのページで返したイベントの最大 id** を運ぶ（次のページがイベントを読み直さないため）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConsoleCursor {
    pub at_nanos: i128,
    pub tie: String,
    pub event_id: u64,
}

impl ConsoleCursor {
    pub fn new(at_nanos: i128, tie: impl Into<String>, event_id: u64) -> Self {
        Self {
            at_nanos,
            tie: tie.into(),
            event_id,
        }
    }

    /// 並びのキー（`event_id` は読み進みの覚えなので並びには使わない）。
    pub fn order_key(&self) -> (i128, &str) {
        (self.at_nanos, self.tie.as_str())
    }

    /// 文字列表現（GUI にはただの不透明な文字列として渡す）。`at_nanos` は 0 詰め 20 桁。
    pub fn encode(&self) -> String {
        format!(
            "{:020}.{}.{}",
            self.at_nanos.max(0),
            self.event_id,
            self.tie
        )
    }

    /// `encode` の逆。形が違えば `None`（API は 400 にする）。
    pub fn decode(s: &str) -> Option<Self> {
        let (at, rest) = s.split_once('.')?;
        let (event_id, tie) = rest.split_once('.')?;
        Some(Self {
            at_nanos: at.parse().ok()?,
            tie: tie.to_string(),
            event_id: event_id.parse().ok()?,
        })
    }
}

/// RFC 3339 の時刻 → Unix エポックからのナノ秒。読めなければ `0`（いちばん古い扱い。
/// タイムラインの `sort_items` と同じ方針）。
pub fn at_nanos(at: &str) -> i128 {
    OffsetDateTime::parse(at, &Rfc3339)
        .map(|t| t.unix_timestamp_nanos())
        .unwrap_or(0)
}

/// `OffsetDateTime` → RFC 3339（書式に失敗したら `Display`）。
pub fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| t.to_string())
}

// ---- progress を run ごとに束ねる ----

/// 折り畳んだ `progress` の中の 1 行（ADR-0048 D2）。本文（`detail`）は載せない
/// （初期表示を軽くするため。全行は `GET /tasks/{id}/runs/{run}/events`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConsoleProgressLine {
    /// RFC 3339。
    pub at: String,
    /// そのタスクの中での `events.seq`（`GET /tasks/{id}/events` と突き合わせられる）。
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProgressKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// 人が読む 1 行（`summary` があればそれ、無ければ `msg`）。
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}

/// run 1 つ分の進行（ADR-0048 D1 の `progress` ブロックの中身）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConsoleProgress {
    pub task_id: TaskId,
    pub run_id: String,
    /// この run の進行の件数。
    pub count: usize,
    /// そのうち `tool_use` の回数（折り畳みの見出しの「tool 12 回」）。
    pub tool_count: usize,
    /// 最後に見た `status`（アダプタの節目）の 1 行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    /// 最初の進行の時刻（RFC 3339）。ブロックの `at` でもある。
    pub started_at: String,
    /// 最後の進行の時刻（RFC 3339）。
    pub updated_at: String,
    pub first: Vec<ConsoleProgressLine>,
    pub last: Vec<ConsoleProgressLine>,
    /// `first` と `last` の間に出していない行がある（全行は `GET /tasks/{id}/runs/{run}/events`）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// `Event::WorkerProgress` を **run ごとに 1 件**へ束ねる（出現順を保つ）。
///
/// 同じ run の行が離れて来ても 1 件にまとまる（Reviewer run の進行は対象 run の `run_id` に
/// 混ざって入るため。ADR-0007 D5）。`WorkerProgress` 以外の行は無視する。
pub fn group_progress<'a>(rows: impl IntoIterator<Item = &'a EventRow>) -> Vec<ConsoleProgress> {
    let mut order: Vec<(TaskId, String)> = Vec::new();
    let mut groups: std::collections::HashMap<(TaskId, String), ConsoleProgress> =
        std::collections::HashMap::new();
    for row in rows {
        let Event::WorkerProgress { run_id, msg, .. } = &row.event else {
            continue;
        };
        let fields = row.event.progress_fields().unwrap_or_default();
        let line = ConsoleProgressLine {
            at: row.ts.clone(),
            seq: row.seq,
            kind: fields.kind,
            tool: fields.tool.clone(),
            text: fields.summary.clone().unwrap_or_else(|| msg.clone()),
            error: fields.error,
        };
        let key = (row.task_id, run_id.clone());
        let group = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            ConsoleProgress {
                task_id: row.task_id,
                run_id: run_id.clone(),
                count: 0,
                tool_count: 0,
                last_status: None,
                started_at: row.ts.clone(),
                updated_at: row.ts.clone(),
                first: Vec::new(),
                last: Vec::new(),
                truncated: false,
            }
        });
        group.count += 1;
        group.updated_at = row.ts.clone();
        if fields.kind == Some(ProgressKind::ToolUse) {
            group.tool_count += 1;
        }
        if fields.kind == Some(ProgressKind::Status) {
            group.last_status = Some(line.text.clone());
        }
        if group.first.len() < PROGRESS_HEAD_LINES {
            group.first.push(line);
        } else {
            group.last.push(line);
            if group.last.len() > PROGRESS_TAIL_LINES {
                group.last.remove(0);
                group.truncated = true;
            }
        }
    }
    order
        .into_iter()
        .filter_map(|key| groups.remove(&key))
        .collect()
}

/// この run の進行を指す `tie`（カーソルの同時刻の並びを決める文字列）。
pub fn progress_tie(task_id: TaskId, run_id: &str) -> String {
    format!("p{task_id}:{run_id}")
}

// ---- 対話 run の「育つ返事」（ADR-0054 D2。Phase 68）----

/// `reply` ブロックの状態（Phase 68）。`streaming` は run 中（`text` はここまでの積み上げ）、`done` は
/// `messages` に確定した返事（従来どおり）。`Default` は `Done`（過去の `messages` 由来の返事や
/// このフィールドを知らないテスト・クライアントが黙って「確定済み」を読めるようにするため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleReplyState {
    Streaming,
    #[default]
    Done,
}

/// 育つ返事の中の 1 手（`tool_use` / `tool_result` だけ。ADR-0054 D2: 「tool_use は tool + summary を
/// 1 行、tool_result は折り畳み」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConsoleReplyStep {
    pub kind: ProgressKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}

/// 対話 run 1 本ぶんの「育つ返事」の積み上げ（`group_progress` の対話版）。`group_progress` と違い、
/// 先頭・末尾で切らない（対話 run は `CONVERSATION_MAX_TURNS` で予算が小さく、際限なく伸びない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConsoleReplyAccum {
    pub task_id: TaskId,
    pub run_id: String,
    pub started_at: String,
    pub updated_at: String,
    /// 最新の `thinking`（ADR-0054 D2: 「thinking は要約 1 行」＝置き換え、積み上げない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ConsoleReplyStep>,
    /// `text` 種の行をそのままつなげたもの（ADR-0054 D2: 「text は本文をそのまま追記」）。
    #[serde(default)]
    pub text: String,
}

/// `Event::WorkerProgress` を**対話 run ごと**に「育つ返事」へ折りたたむ（`group_progress` の対話版）。
/// `thinking` は最後の 1 行に置き換え、`text` は連結、`tool_use`/`tool_result` は `steps` に順番どおり積む。
pub fn group_conversation_progress<'a>(
    rows: impl IntoIterator<Item = &'a EventRow>,
) -> Vec<ConsoleReplyAccum> {
    let mut order: Vec<(TaskId, String)> = Vec::new();
    let mut groups: std::collections::HashMap<(TaskId, String), ConsoleReplyAccum> =
        std::collections::HashMap::new();
    for row in rows {
        let Event::WorkerProgress { run_id, msg, .. } = &row.event else {
            continue;
        };
        let fields = row.event.progress_fields().unwrap_or_default();
        let text = fields.summary.clone().unwrap_or_else(|| msg.clone());
        let key = (row.task_id, run_id.clone());
        let accum = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            ConsoleReplyAccum {
                task_id: row.task_id,
                run_id: run_id.clone(),
                started_at: row.ts.clone(),
                updated_at: row.ts.clone(),
                thinking: None,
                steps: Vec::new(),
                text: String::new(),
            }
        });
        accum.updated_at = row.ts.clone();
        match fields.kind {
            Some(ProgressKind::Thinking) => accum.thinking = Some(text),
            Some(ProgressKind::Text) => accum.text.push_str(&text),
            Some(kind @ (ProgressKind::ToolUse | ProgressKind::ToolResult)) => {
                accum.steps.push(ConsoleReplyStep {
                    kind,
                    tool: fields.tool.clone(),
                    text,
                    error: fields.error,
                });
            }
            // `status`（節目）は「育つ返事」には出さない（今は `thinking`/`text`/`tool_*` だけを見せる。
            // ADR-0054 D2 の一覧どおり）。構造化されていない行（`kind` 無し）も同様に無視する。
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|key| groups.remove(&key))
        .collect()
}

/// `acc` に続きの積み上げ（`next`）を足す（SSE が同じ run の更新を 1 件にまとめるため。`merge_progress`
/// の対話版。ここは先頭・末尾で切らない＝取りこぼしが無い）。
pub fn merge_conversation_reply(acc: &mut ConsoleReplyAccum, next: &ConsoleReplyAccum) {
    if next.thinking.is_some() {
        acc.thinking = next.thinking.clone();
    }
    acc.steps.extend(next.steps.iter().cloned());
    acc.text.push_str(&next.text);
    acc.updated_at = next.updated_at.clone();
}

/// `acc` に続きの束（`next`）を足す（SSE が同じ run の更新を 1 件にまとめるため。ADR-0048 D1）。
///
/// 件数と道具の回数は足し、最後の `status` と `updated_at` は新しい方で置き換え、
/// 行は「頭 `PROGRESS_HEAD_LINES` 件 + 尻 `PROGRESS_TAIL_LINES` 件」を保つ。
pub fn merge_progress(acc: &mut ConsoleProgress, next: &ConsoleProgress) {
    acc.count += next.count;
    acc.tool_count += next.tool_count;
    if next.last_status.is_some() {
        acc.last_status = next.last_status.clone();
    }
    acc.updated_at = next.updated_at.clone();
    acc.truncated = acc.truncated || next.truncated;
    for line in next.first.iter().chain(next.last.iter()) {
        if acc.first.len() < PROGRESS_HEAD_LINES {
            acc.first.push(line.clone());
        } else {
            acc.last.push(line.clone());
            if acc.last.len() > PROGRESS_TAIL_LINES {
                acc.last.remove(0);
                acc.truncated = true;
            }
        }
    }
}

// ---- 遷移を 1 行にする ----

/// `task` ブロックの中身（ADR-0048 D1: 開始・終了・失敗・中止・割り込みを 1 行で）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConsoleTaskLine {
    pub task_id: TaskId,
    pub title: String,
    pub from: Status,
    pub to: Status,
    /// 遷移の理由（`worker_done` / `cancel` / `comment` …）。
    pub reason: String,
    /// 担当（組織のノード id）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// ハーネス（`worker_hint.adapter`。未指定なら役割・分野から決まるので `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub tier: task_core::Tier,
    /// 作業場所の使い方（`shared` / `worktree`。未指定なら `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<task_core::ProjectId>,
    /// タスクが作られてからこの遷移までの秒数（時刻が読めなければ `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<u64>,
}

/// `Event::Transitioned` → Console の 1 行。`Transitioned` でなければ `None`。
pub fn task_line(row: &EventRow, task: &Task) -> Option<ConsoleTaskLine> {
    let Event::Transitioned { from, to, reason } = &row.event else {
        return None;
    };
    let created = task.created_at.unix_timestamp_nanos();
    let elapsed_secs = match at_nanos(&row.ts) {
        0 => None,
        at => u64::try_from((at - created).max(0) / 1_000_000_000).ok(),
    };
    Some(ConsoleTaskLine {
        task_id: task.id,
        title: task.title.clone(),
        from: *from,
        to: *to,
        reason: reason.clone(),
        assignee: task.assignee.clone(),
        harness: task.worker_hint.adapter.clone(),
        tier: task.worker_hint.tier,
        mode: workspace_mode(task),
        project_id: task.project_id,
        elapsed_secs,
    })
}

/// 作業場所の使い方（`WorkspaceSpec::Local { mode }`）。クラスタのタスクは `None`。
fn workspace_mode(task: &Task) -> Option<String> {
    match &task.workspace {
        task_core::WorkspaceSpec::Local { mode, .. } => mode.map(|m| match m {
            task_core::WorkspaceMode::Worktree => "worktree".to_string(),
            task_core::WorkspaceMode::Shared => "shared".to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{ProgressFields, ProgressKind};

    fn row(id: u64, seq: u64, task_id: TaskId, ts: &str, event: Event) -> EventRow {
        EventRow {
            id,
            task_id,
            seq,
            ts: ts.to_string(),
            event,
        }
    }

    /// カーソルは文字列に往復でき、並びは（時刻, tie）で決まる。
    #[test]
    fn cursors_round_trip_and_order_by_time_then_tie() {
        let c = ConsoleCursor::new(1_700_000_000_000_000_000, "e12", 12);
        let text = c.encode();
        assert_eq!(ConsoleCursor::decode(&text), Some(c.clone()));
        assert!(ConsoleCursor::decode("nope").is_none());
        assert!(ConsoleCursor::decode("1.2").is_none());
        // 同時刻は tie の順、時刻が違えば時刻の順（`event_id` は並びに効かない）。
        let older = ConsoleCursor::new(1, "z", 9999);
        let newer = ConsoleCursor::new(2, "a", 0);
        assert!(older.order_key() < newer.order_key());
        let a = ConsoleCursor::new(2, "a", 0);
        let b = ConsoleCursor::new(2, "b", 0);
        assert!(a.order_key() < b.order_key());
        // 小数秒のある RFC 3339 も時刻として比べる（文字列比較では逆になる）。
        assert!(at_nanos("2026-09-20T01:00:00Z") < at_nanos("2026-09-20T01:00:00.5Z"));
        assert_eq!(at_nanos("読めない"), 0);
    }

    /// run ごとに 1 件へ束ね、件数・道具の回数・最後の `status`・始めと終わりを持つ。
    #[test]
    fn progress_is_grouped_per_run_with_counts_and_head_and_tail() {
        let t1 = TaskId::new();
        let t2 = TaskId::new();
        let mut rows = Vec::new();
        let mut id = 0;
        let mut push = |rows: &mut Vec<EventRow>,
                        task: TaskId,
                        run: &str,
                        secs: u32,
                        fields: ProgressFields| {
            id += 1;
            let ts = format!("2026-09-20T01:00:{secs:02}Z");
            rows.push(row(
                id,
                id,
                task,
                &ts,
                Event::worker_progress_with(run, format!("msg {id}"), fields),
            ));
        };
        push(
            &mut rows,
            t1,
            "r1",
            0,
            ProgressFields::of(ProgressKind::Status).with_summary("starting"),
        );
        for i in 1..=8u32 {
            push(
                &mut rows,
                t1,
                "r1",
                i,
                ProgressFields::of(ProgressKind::ToolUse)
                    .with_tool("Bash")
                    .with_summary(format!("cmd {i}")),
            );
        }
        // 別のタスク・別の run は別の束。間に挟まっても 1 件にまとまる。
        push(&mut rows, t2, "r2", 3, ProgressFields::default());
        push(
            &mut rows,
            t1,
            "r1",
            9,
            ProgressFields::of(ProgressKind::Status).with_summary("finishing"),
        );

        let groups = group_progress(&rows);
        assert_eq!(groups.len(), 2);
        let g = &groups[0];
        assert_eq!(g.task_id, t1);
        assert_eq!(g.run_id, "r1");
        assert_eq!(g.count, 10);
        assert_eq!(g.tool_count, 8);
        assert_eq!(g.last_status.as_deref(), Some("finishing"));
        assert_eq!(g.started_at, "2026-09-20T01:00:00Z");
        assert_eq!(g.updated_at, "2026-09-20T01:00:09Z");
        assert_eq!(g.first.len(), PROGRESS_HEAD_LINES);
        assert_eq!(g.last.len(), PROGRESS_TAIL_LINES);
        assert!(g.truncated, "10 件は頭 3 + 尻 3 に収まらない");
        assert_eq!(g.first[0].text, "starting");
        assert_eq!(g.first[0].kind, Some(ProgressKind::Status));
        assert_eq!(g.last[2].text, "finishing");
        // 構造化されていない進行は `msg` がそのまま 1 行になる。
        let g2 = &groups[1];
        assert_eq!(g2.count, 1);
        assert_eq!(g2.tool_count, 0);
        assert!(!g2.truncated);
        assert!(g2.first[0].text.starts_with("msg "));
        assert!(g2.last.is_empty());
        // `WorkerProgress` 以外は無視する。
        let other = vec![row(
            99,
            0,
            t1,
            "2026-09-20T02:00:00Z",
            Event::ApprovalRequested,
        )];
        assert!(group_progress(&other).is_empty());
    }

    /// ADR-0054 D2（Phase 68）: 対話 run の「育つ返事」は thinking を置き換え、text をつなげ、
    /// tool_use/tool_result を順番どおり積む（先頭・末尾で切らない）。
    #[test]
    fn conversation_progress_replaces_thinking_appends_text_and_orders_steps() {
        let t1 = TaskId::new();
        let mut rows = Vec::new();
        let mut id = 0u64;
        let mut push = |rows: &mut Vec<EventRow>, secs: u32, fields: ProgressFields, msg: &str| {
            id += 1;
            let ts = format!("2026-09-21T01:00:{secs:02}Z");
            rows.push(row(
                id,
                id,
                t1,
                &ts,
                Event::worker_progress_with("run-1", msg.to_string(), fields),
            ));
        };
        push(
            &mut rows,
            0,
            ProgressFields::of(ProgressKind::Thinking).with_summary("考え中…"),
            "thinking",
        );
        push(
            &mut rows,
            1,
            ProgressFields::of(ProgressKind::ToolUse)
                .with_tool("celerisctl")
                .with_summary("knowledge search rust"),
            "tool",
        );
        push(
            &mut rows,
            2,
            ProgressFields::of(ProgressKind::ToolResult).with_summary("3 件"),
            "tool result",
        );
        push(
            &mut rows,
            3,
            ProgressFields::of(ProgressKind::Thinking).with_summary("まとめ中…"),
            "thinking2",
        );
        push(
            &mut rows,
            4,
            ProgressFields::of(ProgressKind::Text).with_summary("承知しま"),
            "text1",
        );
        push(
            &mut rows,
            5,
            ProgressFields::of(ProgressKind::Text).with_summary("した。"),
            "text2",
        );
        push(
            &mut rows,
            6,
            ProgressFields::of(ProgressKind::Status).with_summary("節目"),
            "status",
        );

        let groups = group_conversation_progress(&rows);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.task_id, t1);
        assert_eq!(g.run_id, "run-1");
        // thinking は最後の 1 行に置き換わる（積み上げない）。
        assert_eq!(g.thinking.as_deref(), Some("まとめ中…"));
        // text はそのまま連結される。
        assert_eq!(g.text, "承知しました。");
        // tool_use / tool_result は順番どおり積まれる（status は積まれない）。
        assert_eq!(g.steps.len(), 2);
        assert_eq!(g.steps[0].kind, ProgressKind::ToolUse);
        assert_eq!(g.steps[0].tool.as_deref(), Some("celerisctl"));
        assert_eq!(g.steps[0].text, "knowledge search rust");
        assert_eq!(g.steps[1].kind, ProgressKind::ToolResult);
        assert_eq!(g.steps[1].text, "3 件");
        assert_eq!(g.started_at, "2026-09-21T01:00:00Z");
        assert_eq!(g.updated_at, "2026-09-21T01:00:06Z");
    }

    /// SSE の積み上げ（`merge_conversation_reply`）は取りこぼしが無い（`merge_progress` と違い、
    /// 先頭・末尾で切らない）。
    #[test]
    fn merge_conversation_reply_accumulates_without_truncation() {
        let t1 = TaskId::new();
        let mut acc = ConsoleReplyAccum {
            task_id: t1,
            run_id: "run-1".into(),
            started_at: "2026-09-21T01:00:00Z".into(),
            updated_at: "2026-09-21T01:00:00Z".into(),
            thinking: Some("考え中…".into()),
            steps: vec![ConsoleReplyStep {
                kind: ProgressKind::ToolUse,
                tool: Some("celerisctl".into()),
                text: "knowledge search rust".into(),
                error: false,
            }],
            text: "承知しま".into(),
        };
        let next = ConsoleReplyAccum {
            task_id: t1,
            run_id: "run-1".into(),
            started_at: "2026-09-21T01:00:05Z".into(),
            updated_at: "2026-09-21T01:00:05Z".into(),
            thinking: None,
            steps: vec![ConsoleReplyStep {
                kind: ProgressKind::ToolResult,
                tool: None,
                text: "3 件".into(),
                error: false,
            }],
            text: "した。".into(),
        };
        merge_conversation_reply(&mut acc, &next);
        assert_eq!(
            acc.thinking.as_deref(),
            Some("考え中…"),
            "空なら置き換えない"
        );
        assert_eq!(acc.text, "承知しました。");
        assert_eq!(acc.steps.len(), 2);
        assert_eq!(acc.updated_at, "2026-09-21T01:00:05Z");
    }
}
