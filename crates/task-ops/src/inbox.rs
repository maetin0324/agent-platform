//! 受信箱（`docs/gui/api.md` §3.2 / §5.1 / §6.2）。原則 5「人間は承認待ちキューだけを見ればよい」の画面の元データ。

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::Serialize;
use task_core::{ArtifactRef, Event, Status, Task, TaskId, TaskKind, TaskStore, WorkerHint, WorkspaceSpec};
use time::OffsetDateTime;

use crate::daemon::DaemonSnapshot;
use crate::derive::{self, AnswerNote};
use crate::error::OpsError;
use crate::view::{self, ApprovalDecisionView, RunOutcomeKind, RunSummary, TaskRef, TaskSummary, VerdictView, ViewContext};

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Inbox {
    pub approvals: Vec<ApprovalItem>,
    pub questions: Vec<QuestionItem>,
    pub drafts: Vec<DraftGroup>,
    pub attention: Vec<AttentionItem>,
    pub counts: InboxCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct InboxCounts {
    pub approvals: u32,
    pub questions: u32,
    pub drafts: u32,
    pub attention: u32,
    /// status 名 → 件数（DB 全体）。
    pub by_status: std::collections::BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ApprovalItem {
    pub approval: TaskRef,
    pub parent: Option<TaskRef>,
    pub criterion_text: String,
    pub criterion_idx: Option<usize>,
    pub attempt: Option<u32>,
    pub requested_at: String,
    pub last_run: Option<RunSummary>,
    pub evidence: Vec<EvidenceView>,
    pub other_verdicts: Vec<VerdictView>,
    pub artifacts: Vec<ArtifactRef>,
    pub previous_decisions: Vec<ApprovalDecisionView>,
}

/// `task_worker::Evidence` と同じ形。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct EvidenceView {
    pub criterion: usize,
    pub command: Option<String>,
    pub exit: Option<i32>,
    pub stdout_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct QuestionItem {
    pub task: TaskRef,
    pub question: String,
    pub asked_at: Option<String>,
    pub run_id: Option<String>,
    pub previous: Vec<AnswerNote>,
    /// GUI 監査対応 Phase 29: 対応する未決の `approvals` の id（`POST /approvals/{id}/decide` へ
    /// GUI が直接リンクできるように）。無ければ `null`（`approvals` の行がまだ無い、または既に決定済み）。
    pub approval_id: Option<task_core::approval::ApprovalId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DraftGroup {
    pub parent: Option<TaskRef>,
    pub plan_summary: Option<String>,
    pub drafts: Vec<TaskSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttentionItem {
    Failed { task: TaskRef, reason: String, at: String },
    RequeueLimitNear { task: TaskRef, count: u32, max: u32, at: String },
    Unroutable { task: TaskRef, hint: WorkerHint, at: String },
    /// ADR-0018 D2: 直近 24 時間に `ClusterUnavailable` があったクラスタ（人がログインし直すまで用件が続く）。
    ClusterUnavailable { cluster: String, host: String, at: String, tasks: u32 },
}

fn attention_at(item: &AttentionItem) -> &str {
    match item {
        AttentionItem::Failed { at, .. } => at,
        AttentionItem::RequeueLimitNear { at, .. } => at,
        AttentionItem::Unroutable { at, .. } => at,
        AttentionItem::ClusterUnavailable { at, .. } => at,
    }
}

/// `kind == Approval && status == Ready` のタスク（Human check の Approval 子）を集める。
fn build_approvals(
    store: &dyn TaskStore,
    all_tasks: &[Task],
    by_id: &HashMap<TaskId, Task>,
    evidence: &dyn Fn(&Task, &str) -> Vec<EvidenceView>,
) -> Result<Vec<ApprovalItem>, OpsError> {
    let mut items = Vec::new();

    for t in all_tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Approval && t.status == Status::Ready)
    {
        let parent = t.parent_id.and_then(|pid| by_id.get(&pid));
        let parsed = view::parse_human_approval_title(&t.title);
        let criterion_idx = parsed.map(|(i, _)| i);
        let attempt = parsed.map(|(_, a)| a);

        let criterion_text = match (parent, criterion_idx) {
            (Some(p), Some(idx)) => p
                .acceptance
                .get(idx)
                .map(|c| c.text.clone())
                .unwrap_or_else(|| t.objective.clone()),
            _ => t.objective.clone(),
        };

        let own_rows = store.event_rows_for(t.id, None, view::ALL_EVENTS)?;
        let requested_at = own_rows
            .iter()
            .find_map(|r| match &r.event {
                Event::ApprovalRequested => Some(r.ts.clone()),
                _ => None,
            })
            .unwrap_or_else(|| view::to_rfc3339(t.created_at));

        let mut last_run: Option<RunSummary> = None;
        let mut artifacts: Vec<ArtifactRef> = Vec::new();
        let mut other_verdicts: Vec<VerdictView> = Vec::new();
        let mut evidence_items: Vec<EvidenceView> = Vec::new();

        if let Some(parent_task) = parent {
            let parent_rows = store.event_rows_for(parent_task.id, None, view::ALL_EVENTS)?;
            let parent_events = view::seq_pairs(&parent_rows);
            if let Some(run_id) = derive::last_run_id(&parent_events) {
                let run_summaries = view::runs(&parent_rows);
                last_run = run_summaries.into_iter().find(|r| r.run_id == run_id);
                artifacts = derive::artifacts_for_run(&parent_events, &run_id);
                for r in &parent_rows {
                    if let Event::ReviewVerdict {
                        run_id: rid,
                        criterion_idx: c_idx,
                        pass,
                        reason,
                    } = &r.event
                        && rid == &run_id
                    {
                        other_verdicts.push(VerdictView {
                            run_id: rid.clone(),
                            criterion_idx: *c_idx,
                            pass: *pass,
                            reason: reason.clone(),
                            ts: r.ts.clone(),
                        });
                    }
                }
                let is_done = matches!(last_run.as_ref().and_then(|r| r.outcome), Some(RunOutcomeKind::Done));
                if is_done {
                    evidence_items = evidence(parent_task, &run_id);
                }
            }
        }

        let mut previous_decisions: Vec<ApprovalDecisionView> = Vec::new();
        if let (Some(parent_task), Some(idx)) = (parent, criterion_idx) {
            for sibling in all_tasks
                .iter()
                .filter(|s| s.parent_id == Some(parent_task.id) && s.kind == TaskKind::Approval && s.id != t.id)
            {
                if view::parse_human_approval_title(&sibling.title).map(|(i, _)| i) != Some(idx) {
                    continue;
                }
                let sib_rows = store.event_rows_for(sibling.id, None, view::ALL_EVENTS)?;
                if let Some(decided) = sib_rows.iter().rev().find_map(|r| match &r.event {
                    Event::ApprovalDecided { by, approved, note } => Some(ApprovalDecisionView {
                        by: by.clone(),
                        approved: *approved,
                        note: note.clone(),
                        ts: r.ts.clone(),
                    }),
                    _ => None,
                }) {
                    previous_decisions.push(decided);
                }
            }
            previous_decisions.sort_by(|a, b| a.ts.cmp(&b.ts));
        }

        items.push(ApprovalItem {
            approval: view::task_ref(t),
            parent: parent.map(view::task_ref),
            criterion_text,
            criterion_idx,
            attempt,
            requested_at,
            last_run,
            evidence: evidence_items,
            other_verdicts,
            artifacts,
            previous_decisions,
        });
    }

    items.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
    Ok(items)
}

/// `status == Blocked` のタスク。
fn build_questions(store: &dyn TaskStore, all_tasks: &[Task]) -> Result<Vec<QuestionItem>, OpsError> {
    // GUI 監査対応 Phase 29: 未決の approvals を task_id で引けるように 1 回だけ読む。
    let pending_approval_by_task: HashMap<TaskId, task_core::approval::ApprovalId> = store
        .approval_list(Some(true), None, None)?
        .into_iter()
        .filter_map(|a| a.task_id.map(|task_id| (task_id, a.id)))
        .collect();

    let mut items = Vec::new();
    for t in all_tasks.iter().filter(|t| t.status == Status::Blocked) {
        let rows = store.event_rows_for(t.id, None, view::ALL_EVENTS)?;
        let events = view::seq_pairs(&rows);
        let question = derive::latest_question(&events);

        let mut asked_at: Option<String> = None;
        let mut run_id: Option<String> = None;
        for r in rows.iter().rev() {
            match &r.event {
                Event::WorkerFinished { run_id: rid, outcome, role, .. }
                    if !derive::is_reviewer(*role) && outcome.starts_with("question: ") =>
                {
                    asked_at = Some(r.ts.clone());
                    run_id = Some(rid.clone());
                    break;
                }
                // ADR-0021 D2: ディスパッチャが出した質問（委譲した子が失敗し、やり直せなかった）。
                Event::QuestionRaised { run_id: rid, .. } => {
                    asked_at = Some(r.ts.clone());
                    run_id = Some(rid.clone());
                    break;
                }
                _ => {}
            }
        }

        let previous = derive::answers_from_events(&events);
        items.push(QuestionItem {
            task: view::task_ref(t),
            question,
            asked_at,
            run_id,
            previous,
            approval_id: pending_approval_by_task.get(&t.id).copied(),
        });
    }
    items.sort_by(|a, b| a.asked_at.cmp(&b.asked_at));
    Ok(items)
}

/// `status == Draft` のタスクを `parent_id` でまとめる。根（`parent_id == None`）は最後。
fn build_drafts(
    store: &dyn TaskStore,
    all_tasks: &[Task],
    by_id: &HashMap<TaskId, Task>,
    ctx: &ViewContext,
    now: OffsetDateTime,
) -> Result<Vec<DraftGroup>, OpsError> {
    let mut groups: HashMap<Option<TaskId>, Vec<Task>> = HashMap::new();
    for t in all_tasks.iter().filter(|t| t.status == Status::Draft) {
        groups.entry(t.parent_id).or_default().push(t.clone());
    }

    let mut keys: Vec<Option<TaskId>> = groups.keys().copied().collect();
    keys.sort_by_key(|k| sort_key_for_draft_group(k, by_id));
    // 子の件数は全件から 1 回だけ集計する（draft ごとに全件を読むと件数の二乗になる。Phase 9 監査）。
    let counts = view::child_counts(all_tasks);

    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let mut members = groups.remove(&key).unwrap_or_default();
        members.sort_by_key(|t| t.id);

        let parent_task = key.and_then(|pid| by_id.get(&pid));
        let parent = parent_task.map(view::task_ref);
        let plan_summary = match parent_task {
            Some(p) => {
                let rows = store.event_rows_for(p.id, None, view::ALL_EVENTS)?;
                rows.iter().rev().find_map(|r| match &r.event {
                    Event::WorkerFinished { outcome, role, .. } if !derive::is_reviewer(*role) => {
                        outcome.strip_prefix("done: ").map(str::to_string)
                    }
                    _ => None,
                })
            }
            None => None,
        };

        let drafts: Vec<TaskSummary> = members
            .iter()
            .map(|t| {
                let (children, pending) = counts.get(&t.id).copied().unwrap_or((0, 0));
                view::build_task_summary(t, children, pending, ctx, now)
            })
            .collect();

        out.push(DraftGroup {
            parent,
            plan_summary,
            drafts,
        });
    }
    Ok(out)
}

/// 親の `created_at` 昇順、根（`None`）は最後になるようなソートキー。
fn sort_key_for_draft_group(key: &Option<TaskId>, by_id: &HashMap<TaskId, Task>) -> (u8, String, String) {
    match key {
        Some(pid) => (
            0,
            by_id.get(pid).map(|p| view::to_rfc3339(p.created_at)).unwrap_or_default(),
            pid.to_string(),
        ),
        None => (1, String::new(), String::new()),
    }
}

/// (a) 24h 以内に `failed`、(b) `ready` で連続 requeue が上限近く、(c) スナップショットの `unroutable`。
fn build_attention(
    store: &dyn TaskStore,
    all_tasks: &[Task],
    by_id: &HashMap<TaskId, Task>,
    snapshot: Option<&DaemonSnapshot>,
    ctx: &ViewContext,
    now: OffsetDateTime,
) -> Result<Vec<AttentionItem>, OpsError> {
    let mut items = Vec::new();
    let cutoff = now - time::Duration::hours(24);

    for t in all_tasks.iter().filter(|t| t.status == Status::Failed) {
        if t.updated_at < cutoff {
            continue;
        }
        let rows = store.event_rows_for(t.id, None, view::ALL_EVENTS)?;
        let events = view::seq_pairs(&rows);

        let mut reasons: Vec<String> = Vec::new();
        if let Some(outcome) = rows.iter().rev().find_map(|r| match &r.event {
            Event::WorkerFinished { outcome, role, .. } if !derive::is_reviewer(*role) => Some(outcome.clone()),
            _ => None,
        }) {
            reasons.push(outcome);
        }
        if let Some(run_id) = derive::last_run_id(&events) {
            for r in &rows {
                if let Event::ReviewVerdict { run_id: rid, pass, reason, .. } = &r.event
                    && rid == &run_id
                    && !*pass
                {
                    reasons.push(reason.clone());
                }
            }
        }

        items.push(AttentionItem::Failed {
            task: view::task_ref(t),
            reason: reasons.join("; "),
            at: view::to_rfc3339(t.updated_at),
        });
    }

    if ctx.max_requeues > 0 {
        for t in all_tasks.iter().filter(|t| t.status == Status::Ready) {
            let rows = store.event_rows_for(t.id, None, view::ALL_EVENTS)?;
            let events = view::seq_pairs(&rows);
            let count = derive::consecutive_requeues(&events);
            // 一度も requeue していないタスクは対象外（`max_requeues = 1` で全 ready が並ぶのを防ぐ。Phase 9 監査）。
            if count > 0 && count >= ctx.max_requeues - 1 {
                items.push(AttentionItem::RequeueLimitNear {
                    task: view::task_ref(t),
                    count,
                    max: ctx.max_requeues,
                    at: view::to_rfc3339(t.updated_at),
                });
            }
        }
    }

    if let Some(snap) = snapshot {
        for tid in &snap.unroutable {
            if let Some(t) = by_id.get(tid) {
                items.push(AttentionItem::Unroutable {
                    task: view::task_ref(t),
                    hint: t.worker_hint.clone(),
                    at: snap.last_tick_at.clone(),
                });
            }
        }
    }

    // (d) 直近 24 時間に `ClusterUnavailable` があったクラスタを 1 件ずつ出す（ADR-0018 D2）。
    // ワークスペースが `Remote` で**終端でない**タスクだけを対象にする（`Local` はクラスタと無関係。done / failed / cancelled の
    // タスクはもうクラスタを待っていないので、イベント列を読まない。監査 4-2: 走査を待っているタスクの数に抑える）。
    let mut cluster_agg: HashMap<String, (OffsetDateTime, String, String, HashSet<TaskId>)> = HashMap::new();
    for t in all_tasks.iter() {
        if t.status.is_terminal() || !matches!(t.workspace, WorkspaceSpec::Remote { .. }) {
            continue;
        }
        let rows = store.event_rows_for(t.id, None, view::ALL_EVENTS)?;
        for r in &rows {
            let Event::ClusterUnavailable { cluster: ev_cluster, host, .. } = &r.event else {
                continue;
            };
            let Ok(ts) = OffsetDateTime::parse(&r.ts, &time::format_description::well_known::Rfc3339) else {
                continue;
            };
            if ts < cutoff {
                continue;
            }
            let entry = cluster_agg
                .entry(ev_cluster.clone())
                .or_insert_with(|| (ts, r.ts.clone(), host.clone(), HashSet::new()));
            entry.3.insert(t.id);
            if ts > entry.0 {
                entry.0 = ts;
                entry.1 = r.ts.clone();
                entry.2 = host.clone();
            }
        }
    }

    let mut cluster_ids: Vec<String> = cluster_agg.keys().cloned().collect();
    cluster_ids.sort();
    for cluster_id in cluster_ids {
        // 接続が戻っている（人が再度ログインした）なら、この呼びかけはもう不要。
        if let Some(snap) = snapshot
            && snap.clusters.iter().any(|c| c.id == cluster_id && c.connected)
        {
            continue;
        }
        let (_, latest_ts, latest_host, task_ids) = &cluster_agg[&cluster_id];
        let host = if !latest_host.is_empty() {
            latest_host.clone()
        } else {
            snapshot
                .and_then(|snap| snap.clusters.iter().find(|c| c.id == cluster_id))
                .map(|c| c.host.clone())
                .unwrap_or_default()
        };
        items.push(AttentionItem::ClusterUnavailable {
            cluster: cluster_id,
            host,
            at: latest_ts.clone(),
            tasks: task_ids.len() as u32,
        });
    }

    items.sort_by(|a, b| attention_at(b).cmp(attention_at(a)));
    Ok(items)
}

/// `docs/gui/api.md` §5.1。`evidence` は `(親タスク, run_id)` から `runs/<run_id>/result.json` の `evidence[]` を読む
/// 呼び出し側の関数（ファイル I/O は task-api が行う）。
pub fn inbox(
    store: &dyn TaskStore,
    snapshot: Option<&DaemonSnapshot>,
    ctx: &ViewContext,
    now: OffsetDateTime,
    evidence: &dyn Fn(&Task, &str) -> Vec<EvidenceView>,
) -> Result<Inbox, OpsError> {
    let all_tasks = store.list(None)?;
    let by_id: HashMap<TaskId, Task> = all_tasks.iter().map(|t| (t.id, t.clone())).collect();

    let approvals = build_approvals(store, &all_tasks, &by_id, evidence)?;
    let questions = build_questions(store, &all_tasks)?;
    let drafts = build_drafts(store, &all_tasks, &by_id, ctx, now)?;
    let attention = build_attention(store, &all_tasks, &by_id, snapshot, ctx, now)?;

    let by_status = store
        .count_by_status()?
        .into_iter()
        .map(|(s, n)| (view::status_key(s).to_string(), n))
        .collect();

    let counts = InboxCounts {
        approvals: approvals.len() as u32,
        questions: questions.len() as u32,
        // グループ数ではなく draft タスクの件数（バッジ表示用。Phase 9 監査）。
        drafts: drafts.iter().map(|g| g.drafts.len() as u32).sum(),
        attention: attention.len() as u32,
        by_status,
    };

    Ok(Inbox {
        approvals,
        questions,
        drafts,
        attention,
        counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration as StdDuration;
    use task_core::{Budget, Check, Criterion, SqliteStore, Task, TaskId, Tier, WorkspaceSpec};

    fn view_ctx() -> ViewContext {
        ViewContext {
            workspace_root: std::path::PathBuf::from("/tmp/workspaces"),
            retry_backoff_base: StdDuration::from_secs(10),
            retry_backoff_max: StdDuration::from_secs(300),
            max_requeues: 5,
            clusters: Default::default(),
        }
    }

    fn sample_task(kind: TaskKind, status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind,
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            acceptance: vec![Criterion {
                text: "tests pass".to_string(),
                check: Check::Command {
                    cmd: "true".to_string(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: task_core::WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local { path: "workspace".into(), mode: None },
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

    fn no_evidence(_task: &Task, _run_id: &str) -> Vec<EvidenceView> {
        Vec::new()
    }

    #[test]
    fn inbox_approvals_section_links_parent_run_and_calls_evidence_for_done_run() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut parent = sample_task(TaskKind::Execute, Status::Reviewing);
        parent.acceptance = vec![Criterion {
            text: "looks good".into(),
            check: Check::Human,
        }];
        store.insert(&parent).expect("insert parent");
        store
            .append_event(
                parent.id,
                &Event::WorkerStarted {
                    run_id: "run-1".into(),
                    adapter: "claude-code".into(),
                    model: "m".into(),
                    provider: Some("claude-a".into()),
                    account: None,
                    role: None,
                    task_role: None,
                },
            )
            .expect("started");
        store
            .append_event(
                parent.id,
                &Event::WorkerFinished {
                    run_id: "run-1".into(),
                    outcome: "done: implemented".into(),
                    usage: None,
                    role: None,
                },
            )
            .expect("finished");
        store
            .append_event(
                parent.id,
                &Event::ReviewVerdict {
                    run_id: "run-1".into(),
                    criterion_idx: 0,
                    pass: false,
                    reason: "needs human sign-off".into(),
                },
            )
            .expect("verdict");

        let mut approval = sample_task(TaskKind::Approval, Status::Ready);
        approval.parent_id = Some(parent.id);
        approval.title = derive::human_approval_title(&parent, 0);
        store.insert(&approval).expect("insert approval");
        store.append_event(approval.id, &Event::ApprovalRequested).expect("requested");

        let calls: Cell<u32> = Cell::new(0);
        let evidence_fn = |task: &Task, run_id: &str| {
            calls.set(calls.get() + 1);
            assert_eq!(task.id, parent.id);
            assert_eq!(run_id, "run-1");
            vec![EvidenceView {
                criterion: 0,
                command: Some("cargo test".into()),
                exit: Some(0),
                stdout_tail: Some("ok".into()),
            }]
        };

        let ctx = view_ctx();
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &evidence_fn).expect("inbox");

        assert_eq!(result.approvals.len(), 1);
        let item = &result.approvals[0];
        assert_eq!(item.approval.id, approval.id);
        assert_eq!(item.parent.as_ref().map(|p| p.id), Some(parent.id));
        assert_eq!(item.criterion_idx, Some(0));
        assert_eq!(item.attempt, Some(1));
        assert_eq!(item.criterion_text, "looks good");
        assert_eq!(item.last_run.as_ref().map(|r| r.run_id.clone()), Some("run-1".to_string()));
        assert_eq!(item.other_verdicts.len(), 1);
        assert_eq!(item.artifacts.len(), 0);
        assert_eq!(item.evidence.len(), 1);
        assert_eq!(calls.get(), 1);
        assert_eq!(result.counts.approvals, 1);
    }

    #[test]
    fn inbox_approvals_ordered_by_requested_at_and_includes_previous_decisions() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut parent = sample_task(TaskKind::Execute, Status::Reviewing);
        parent.acceptance = vec![Criterion {
            text: "looks good".into(),
            check: Check::Human,
        }];
        store.insert(&parent).expect("insert parent");

        // attempt 1: already decided (rejected).
        let mut attempt1 = sample_task(TaskKind::Approval, Status::Failed);
        attempt1.parent_id = Some(parent.id);
        attempt1.title = derive::human_approval_title(&parent, 0);
        store.insert(&attempt1).expect("insert attempt1");
        store
            .append_event(
                attempt1.id,
                &Event::ApprovalDecided {
                    by: "human".into(),
                    approved: false,
                    note: Some("not yet".into()),
                },
            )
            .expect("decide attempt1");

        // attempt 2: pending, requested after attempt 1's decision.
        let mut attempt2_parent_snapshot = parent.clone();
        attempt2_parent_snapshot.attempts = 1;
        let mut attempt2 = sample_task(TaskKind::Approval, Status::Ready);
        attempt2.parent_id = Some(parent.id);
        attempt2.title = derive::human_approval_title(&attempt2_parent_snapshot, 0);
        store.insert(&attempt2).expect("insert attempt2");
        store.append_event(attempt2.id, &Event::ApprovalRequested).expect("requested attempt2");

        let ctx = view_ctx();
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");

        assert_eq!(result.approvals.len(), 1, "only the ready approval appears");
        let pending = &result.approvals[0];
        assert_eq!(pending.approval.id, attempt2.id);
        assert_eq!(pending.previous_decisions.len(), 1);
        assert!(!pending.previous_decisions[0].approved);
        assert_eq!(pending.previous_decisions[0].note.as_deref(), Some("not yet"));
    }

    #[test]
    fn inbox_questions_section_reports_question_asked_at_and_run_id() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert task");
        store
            .append_event(
                task.id,
                &Event::WorkerFinished {
                    run_id: "run-7".into(),
                    outcome: "question: which version?".into(),
                    usage: None,
                    role: None,
                },
            )
            .expect("finished");

        let ctx = view_ctx();
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        assert_eq!(result.questions.len(), 1);
        let q = &result.questions[0];
        assert_eq!(q.task.id, task.id);
        assert_eq!(q.question, "which version?");
        assert_eq!(q.run_id.as_deref(), Some("run-7"));
        assert!(q.asked_at.is_some());
        assert_eq!(result.counts.questions, 1);
        assert_eq!(q.approval_id, None, "approvals の行がまだ無ければ null");
    }

    /// GUI 監査対応 Phase 29: 質問に対応する未決の `approvals` の id が付き、GUI が認可画面へ
    /// 直接リンクできる。決定済みの approval は付かない（`pending = true` でしか引かないため）。
    #[test]
    fn inbox_questions_carry_the_id_of_their_pending_approval() {
        use task_core::approval::{Approval, ApprovalId, ApprovalStore};

        let store = SqliteStore::open_in_memory().expect("open store");
        let with_pending = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&with_pending).expect("insert");
        let pending = Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "secretary".into(),
            task_id: Some(with_pending.id),
            question: "どのクラスタを使いますか".into(),
            decision: None,
            answer: None,
            created_at: OffsetDateTime::now_utc(),
            decided_at: None,
        };
        store.approval_append(&pending).expect("append");

        // すでに決定済みの approval を持つ別のタスク（新しい質問はまだ来ていない想定）には付かない。
        let with_decided_only = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&with_decided_only).expect("insert");
        let decided = Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "secretary".into(),
            task_id: Some(with_decided_only.id),
            question: "別の質問".into(),
            decision: Some(task_core::approval::Decision::Once),
            answer: Some("x".into()),
            created_at: OffsetDateTime::now_utc(),
            decided_at: Some(OffsetDateTime::now_utc()),
        };
        store.approval_append(&decided).expect("append");

        let ctx = view_ctx();
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        let find = |id: TaskId| result.questions.iter().find(|q| q.task.id == id).expect("question");
        assert_eq!(find(with_pending.id).approval_id, Some(pending.id));
        assert_eq!(find(with_decided_only.id).approval_id, None);
    }

    #[test]
    fn inbox_drafts_grouped_by_parent_with_root_group_last() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let plan = sample_task(TaskKind::Plan, Status::Done);
        store.insert(&plan).expect("insert plan");
        store
            .append_event(
                plan.id,
                &Event::WorkerFinished {
                    run_id: "run-1".into(),
                    outcome: "done: built the plan".into(),
                    usage: None,
                    role: None,
                },
            )
            .expect("finished");

        let mut child = sample_task(TaskKind::Execute, Status::Draft);
        child.parent_id = Some(plan.id);
        store.insert(&child).expect("insert child");

        let root_draft = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&root_draft).expect("insert root draft");

        let ctx = view_ctx();
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");

        assert_eq!(result.drafts.len(), 2);
        assert_eq!(result.drafts[0].parent.as_ref().map(|p| p.id), Some(plan.id));
        assert_eq!(result.drafts[0].plan_summary.as_deref(), Some("built the plan"));
        assert_eq!(result.drafts[0].drafts.len(), 1);
        assert_eq!(result.drafts[0].drafts[0].id, child.id);

        assert!(result.drafts[1].parent.is_none(), "root group must be last");
        assert_eq!(result.drafts[1].drafts.len(), 1);
        assert_eq!(result.drafts[1].drafts[0].id, root_draft.id);
        assert_eq!(result.counts.drafts, 2);
    }

    #[test]
    fn inbox_attention_includes_recent_failure_and_requeue_near_limit() {
        let store = SqliteStore::open_in_memory().expect("open store");

        let failed = sample_task(TaskKind::Execute, Status::Failed);
        store.insert(&failed).expect("insert failed");
        store
            .append_event(
                failed.id,
                &Event::WorkerFinished {
                    run_id: "run-1".into(),
                    outcome: "error(retryable=false): boom".into(),
                    usage: None,
                    role: None,
                },
            )
            .expect("finished");

        let mut near_limit = sample_task(TaskKind::Execute, Status::Ready);
        near_limit.attempts = 1;
        store.insert(&near_limit).expect("insert near_limit");
        for _ in 0..4 {
            store
                .append_event(
                    near_limit.id,
                    &Event::Transitioned {
                        from: Status::Running,
                        to: Status::Ready,
                        reason: "requeue".into(),
                    },
                )
                .expect("requeue event");
        }

        let ctx = view_ctx(); // max_requeues = 5, so >= 4 triggers RequeueLimitNear.
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");

        assert!(result.attention.iter().any(|a| matches!(a, AttentionItem::Failed { task, .. } if task.id == failed.id)));
        assert!(result.attention.iter().any(
            |a| matches!(a, AttentionItem::RequeueLimitNear { task, count, max, .. } if task.id == near_limit.id && *count == 4 && *max == 5)
        ));
        assert!(!result.attention.iter().any(|a| matches!(a, AttentionItem::Unroutable { .. })));
    }

    #[test]
    fn inbox_attention_unroutable_only_populated_with_snapshot() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let stuck = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&stuck).expect("insert stuck");

        let ctx = view_ctx();
        let without_snapshot = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        assert!(!without_snapshot.attention.iter().any(|a| matches!(a, AttentionItem::Unroutable { .. })));

        let snapshot = DaemonSnapshot {
            instance_id: "01J000000000000000000000AA".into(),
            pid: 1,
            hostname: "host".into(),
            started_at: view::to_rfc3339(OffsetDateTime::now_utc()),
            last_tick_at: view::to_rfc3339(OffsetDateTime::now_utc()),
            ticks: 1,
            tick_ms: 2000,
            in_flight: vec![],
            cooldowns: vec![],
            awaiting_human: vec![],
            awaiting_children: vec![],
            unroutable: vec![stuck.id],
            reports: None,
            approvals_pending: 0,
            clusters: vec![],
            providers: vec![],
            accounts_root: None,
            accounts_roots: std::collections::HashMap::new(),
            max_runs_per_account: None,
            accounts: vec![],
        };
        let with_snapshot = inbox(&store, Some(&snapshot), &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        assert!(
            with_snapshot
                .attention
                .iter()
                .any(|a| matches!(a, AttentionItem::Unroutable { task, .. } if task.id == stuck.id))
        );
    }

    #[test]
    fn inbox_counts_match_section_lengths_and_status_totals() {
        let store = SqliteStore::open_in_memory().expect("open store");
        store.insert(&sample_task(TaskKind::Execute, Status::Draft)).expect("insert");
        store.insert(&sample_task(TaskKind::Execute, Status::Ready)).expect("insert");

        let ctx = view_ctx();
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        assert_eq!(result.counts.approvals, result.approvals.len() as u32);
        assert_eq!(result.counts.questions, result.questions.len() as u32);
        assert_eq!(result.counts.drafts, result.drafts.iter().map(|g| g.drafts.len() as u32).sum::<u32>());
        assert_eq!(result.counts.attention, result.attention.len() as u32);
        assert_eq!(result.counts.by_status.get("draft").copied(), Some(1));
        assert_eq!(result.counts.by_status.get("ready").copied(), Some(1));
    }

    /// Phase 9 監査: `counts.drafts` は draft タスクの件数（グループ数ではない）。draft の子の件数は一覧と同じ規則で 1 回だけ数える。
    #[test]
    fn inbox_draft_count_is_tasks_not_groups_and_child_counts_are_filled() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let parent = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&parent).expect("insert parent");
        for status in [Status::Draft, Status::Done] {
            let mut child = sample_task(TaskKind::Execute, status);
            child.parent_id = Some(parent.id);
            store.insert(&child).expect("insert child");
        }
        store.insert(&sample_task(TaskKind::Execute, Status::Draft)).expect("insert other root");

        let result = inbox(&store, None, &view_ctx(), OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        assert_eq!(result.drafts.len(), 2, "root group + the parent's group");
        assert_eq!(result.counts.drafts, 3);
        let root_group = result.drafts.iter().find(|g| g.parent.is_none()).expect("root group");
        let summary = root_group.drafts.iter().find(|s| s.id == parent.id).expect("parent summary");
        assert_eq!((summary.children, summary.pending_children), (2, 1));
    }

    /// Phase 9 監査: 一度も requeue していない ready タスクは、`max_requeues = 1` でも `requeue_limit_near` にならない。
    #[test]
    fn inbox_requeue_limit_near_ignores_tasks_that_never_requeued() {
        let store = SqliteStore::open_in_memory().expect("open store");
        store.insert(&sample_task(TaskKind::Execute, Status::Ready)).expect("insert fresh");
        let requeued = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&requeued).expect("insert requeued");
        store
            .append_event(
                requeued.id,
                &Event::Transitioned { from: Status::Running, to: Status::Ready, reason: "requeue".into() },
            )
            .expect("requeue event");

        let ctx = ViewContext { max_requeues: 1, ..view_ctx() };
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        let near: Vec<(TaskId, u32)> = result
            .attention
            .iter()
            .filter_map(|a| match a {
                AttentionItem::RequeueLimitNear { task, count, .. } => Some((task.id, *count)),
                _ => None,
            })
            .collect();
        assert_eq!(near, vec![(requeued.id, 1)]);
    }

    fn remote_task(status: Status, cluster: &str) -> Task {
        let mut t = sample_task(TaskKind::Execute, status);
        t.workspace = WorkspaceSpec::Remote { cluster: cluster.to_string(), path: "workspace".into() };
        t
    }

    fn cluster_unavailable_find<'a>(items: &'a [AttentionItem], cluster: &str) -> Option<(&'a str, &'a str, u32)> {
        items.iter().find_map(|a| match a {
            AttentionItem::ClusterUnavailable { cluster: c, host, at, tasks } if c == cluster => {
                Some((host.as_str(), at.as_str(), *tasks))
            }
            _ => None,
        })
    }

    /// ADR-0018 D2 / 受け入れ条件 9: `Remote` タスク 2 件で `ClusterUnavailable` が起きたら、クラスタ 1 件にまとまる。
    /// `Local` タスクの同イベントは対象外。
    #[test]
    fn inbox_attention_cluster_unavailable_groups_remote_tasks_by_cluster() {
        let store = SqliteStore::open_in_memory().expect("open store");

        let remote1 = remote_task(Status::Ready, "pegasus");
        store.insert(&remote1).expect("insert remote1");
        store
            .append_event(
                remote1.id,
                &Event::ClusterUnavailable {
                    cluster: "pegasus".into(),
                    host: "pegasus".into(),
                    reason: "no multiplexed connection".into(),
                },
            )
            .expect("cluster unavailable 1");

        let remote2 = remote_task(Status::Ready, "pegasus");
        store.insert(&remote2).expect("insert remote2");
        store
            .append_event(
                remote2.id,
                &Event::ClusterUnavailable {
                    cluster: "pegasus".into(),
                    host: "pegasus".into(),
                    reason: "no multiplexed connection".into(),
                },
            )
            .expect("cluster unavailable 2");

        let local = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&local).expect("insert local");
        store
            .append_event(
                local.id,
                &Event::ClusterUnavailable {
                    cluster: "pegasus".into(),
                    host: "pegasus".into(),
                    reason: "no multiplexed connection".into(),
                },
            )
            .expect("cluster unavailable local");

        let ctx = view_ctx();
        let result = inbox(&store, None, &ctx, OffsetDateTime::now_utc(), &no_evidence).expect("inbox");
        let (host, at, tasks) = cluster_unavailable_find(&result.attention, "pegasus").expect("cluster item present");
        assert_eq!(host, "pegasus");
        assert_eq!(tasks, 2, "only the remote tasks count, the local one does not");
        assert!(!at.is_empty());
        assert_eq!(result.counts.attention, result.attention.len() as u32);
    }

    /// 24h の窓の外（`now` を +25h にする）になったら消える。
    #[test]
    fn inbox_attention_cluster_unavailable_drops_outside_24h_window() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let remote = remote_task(Status::Ready, "sirius");
        store.insert(&remote).expect("insert remote");
        store
            .append_event(
                remote.id,
                &Event::ClusterUnavailable {
                    cluster: "sirius".into(),
                    host: "sirius".into(),
                    reason: "no multiplexed connection".into(),
                },
            )
            .expect("cluster unavailable");

        let ctx = view_ctx();
        let now = OffsetDateTime::now_utc();
        let fresh = inbox(&store, None, &ctx, now, &no_evidence).expect("inbox");
        assert!(cluster_unavailable_find(&fresh.attention, "sirius").is_some());

        let later = now + time::Duration::hours(25);
        let expired = inbox(&store, None, &ctx, later, &no_evidence).expect("inbox");
        assert!(cluster_unavailable_find(&expired.attention, "sirius").is_none());
    }

    /// スナップショットの `clusters[].connected == true` なら「ログインし直してください」の呼びかけは用済みなので消える。
    /// `connected == false` なら残る。第 1 段階の行（`host: ""`）はスナップショットの `host` で補われる。
    #[test]
    fn inbox_attention_cluster_unavailable_hidden_once_reconnected_and_host_filled_from_snapshot() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let remote = remote_task(Status::Ready, "pegasus");
        store.insert(&remote).expect("insert remote");
        store
            .append_event(
                remote.id,
                &Event::ClusterUnavailable {
                    cluster: "pegasus".into(),
                    host: String::new(),
                    reason: "no multiplexed connection".into(),
                },
            )
            .expect("cluster unavailable");

        let ctx = view_ctx();
        let now = OffsetDateTime::now_utc();

        let disconnected_snapshot = DaemonSnapshot {
            instance_id: "01J000000000000000000000AA".into(),
            pid: 1,
            hostname: "host".into(),
            started_at: view::to_rfc3339(now),
            last_tick_at: view::to_rfc3339(now),
            ticks: 1,
            tick_ms: 2000,
            in_flight: vec![],
            cooldowns: vec![],
            awaiting_human: vec![],
            awaiting_children: vec![],
            unroutable: vec![],
            reports: None,
            approvals_pending: 0,
            clusters: vec![crate::daemon::ClusterLive {
                id: "pegasus".into(),
                host: "pegasus".into(),
                concurrency: 1,
                in_use: 0,
                connected: false,
                cooldown_until: None,
                auth: "manual".into(),
                connect_pending: false,
            }],
            providers: vec![],
            accounts_root: None,
            accounts_roots: std::collections::HashMap::new(),
            max_runs_per_account: None,
            accounts: vec![],
        };
        let still_present = inbox(&store, Some(&disconnected_snapshot), &ctx, now, &no_evidence).expect("inbox");
        let (host, _, _) =
            cluster_unavailable_find(&still_present.attention, "pegasus").expect("item present while disconnected");
        assert_eq!(host, "pegasus", "host filled from the snapshot's cluster entry");

        let connected_snapshot = DaemonSnapshot {
            clusters: vec![crate::daemon::ClusterLive {
                id: "pegasus".into(),
                host: "pegasus".into(),
                concurrency: 1,
                in_use: 0,
                connected: true,
                cooldown_until: None,
                auth: "manual".into(),
                connect_pending: false,
            }],
            ..disconnected_snapshot
        };
        let hidden = inbox(&store, Some(&connected_snapshot), &ctx, now, &no_evidence).expect("inbox");
        assert!(cluster_unavailable_find(&hidden.attention, "pegasus").is_none());
    }
}
