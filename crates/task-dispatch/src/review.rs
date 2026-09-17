//! Reviewer（DESIGN §5.7, ADR-0005 D5, ADR-0007 D4/D5）。
//!
//! - `Command` はワーカーの自己申告を信じず、ワークスペースで実際に再実行する。
//! - `ArtifactExists` はファイル存在と sha256。
//! - `Plan` kind は追加で `artifacts/plan.json` を `PlanOutput` として決定的に検証する（暗黙の条件、
//!   `criterion_idx = acceptance.len()`）。
//! - `Reviewer` は、決定的条件が全て pass のときだけ、ワーカーアダプタ経由の **別 run**（合成した `Review` kind
//!   タスク + `context.review`）を起動し、`artifacts/review.json` の判定を採る。**このモジュールは LLM を呼ばない**
//!   （アダプタに run を依頼するだけ。どのアダプタ／プロバイダを使うかはディスパッチャが tick で決める）。
//! - `Human` は、ディスパッチャが `Approval` 子タスクの結果を `ReviewExtras::human` として渡す
//!   (ADR-0008 D2)。このモジュール自身はストアに触れない。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use task_core::plan::{PlanLimits, PlanOutput, parse_and_validate};
use task_core::{ArtifactRef, Check, GenreSpec, Status, Task, TaskId, TaskKind, Tier, Usage, WorkerHint};
use task_worker::artifact::sha256_file;

use crate::policy::ProviderOutcome;
use task_worker::{
    EventSink, Evidence, PROTOCOL_VERSION, ReviewOutput, ReviewRequest, RunContext, RunLimits, RunRequest, Terminal,
    WorkerAdapter, Workspace,
};

/// 条件 1 件の判定結果。`Event::ReviewVerdict` にそのまま写す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub criterion_idx: usize,
    pub pass: bool,
    pub reason: String,
}

/// 対象 run の `done` の内容（`Reviewer` run に `context.review` として渡す。ADR-0007 D5）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewSubject {
    pub summary: String,
    pub evidence: Vec<Evidence>,
}

/// `Reviewer` 条件のためにディスパッチャが選んだ run（ADR-0007 D5 1./3.）。
pub struct ReviewerRun {
    pub adapter: Arc<dyn WorkerAdapter>,
    pub run_id: String,
    pub limits: RunLimits,
    pub sink: Box<dyn EventSink>,
    /// 合成 `Review` タスクの `worker_hint`（設定 `[reviewer]`。ADR-0010 D9）。
    pub hint: WorkerHint,
}

/// `Plan` kind の検証パラメータ（ADR-0007 D2/D4, ADR-0028 D3）。
#[derive(Debug, Clone)]
pub struct PlanCheck {
    /// その Plan 自身を含む祖先 Plan の数。
    pub depth: u32,
    pub limits: PlanLimits,
    /// ADR-0028 D3: `PlanOutput.tasks[].genre` / `role` の整合検証に使う（`[[genres]]`）。
    pub genres: Vec<GenreSpec>,
}

/// `Reviewer` run が供給側の失敗で判定できなかったこと（ADR-0010 D5, P-29）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerProviderFailure {
    pub outcome: ProviderOutcome,
    pub message: String,
}

/// `review_task` の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOutcome {
    pub verdicts: Vec<Verdict>,
    /// `Plan` kind で検証に通った場合の出力（子タスクの生成に使う）。
    pub plan: Option<PlanOutput>,
    /// `Some` のとき判定は無効。ディスパッチャは遷移を適用せず `reviewing` のまま延期する（attempts を消費しない）。
    pub provider_failure: Option<ReviewerProviderFailure>,
    /// Reviewer run を起動した場合の、その run 自身の結果（ADR-0014 D1。ディスパッチャが `WorkerFinished{role: reviewer}` にする）。
    pub reviewer_run: Option<ReviewerRunRecord>,
}

/// Reviewer run 自身の終わり方（ADR-0014 D1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerRunRecord {
    pub run_id: String,
    /// `WorkerFinished.outcome` と同じ接頭辞の規則（`done: ` / `question: ` / `error(retryable=…): ` / `requeue: `）。
    pub outcome: String,
    /// `done` の `usage`（それ以外は `None`）。
    pub usage: Option<Usage>,
}

impl ReviewOutcome {
    pub fn all_pass(&self) -> bool {
        self.verdicts.iter().all(|v| v.pass)
    }
}

/// `Check::Human` の criterion idx ごとに解決した `(pass, reason)`（ADR-0008 D2）。
pub type HumanVerdicts = HashMap<usize, (bool, String)>;

pub const PLAN_FILE: &str = "artifacts/plan.json";
pub const REVIEW_FILE: &str = "artifacts/review.json";

const REASON_TAIL: usize = 1024;

fn tail(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut start = s.len() - n;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// `task` に `Check::Reviewer` の条件があるか（ディスパッチャがレビュー run の要否を決めるのに使う）。
pub fn needs_reviewer_run(task: &Task) -> bool {
    task.acceptance.iter().any(|c| matches!(c.check, Check::Reviewer))
}

/// `Reviewer` run に使う `WorkerHint`（DESIGN §5.7: `Standard` tier）。
pub fn reviewer_hint() -> WorkerHint {
    WorkerHint {
        tier: Tier::Standard,
        adapter: None,
    }
}

/// `review_task` の Phase 5 追加入力（ADR-0007）。
#[derive(Default)]
pub struct ReviewExtras {
    /// 対象 run の `done` の内容（`Reviewer` run に渡す）。
    pub subject: ReviewSubject,
    /// `task.kind == Plan` のときだけ `Some`。
    pub plan: Option<PlanCheck>,
    /// `Reviewer` 条件があり、ディスパッチャが run を用意できたときだけ `Some`。
    pub reviewer: Option<ReviewerRun>,
    /// `Check::Human` の各 criterion idx について、ディスパッチャが `Approval` 子タスクの終端状態から
    /// 解決した `(pass, reason)`（ADR-0008 D2）。呼び出し元は `task.acceptance` の全 `Human` criterion が
    /// 解決済みのときだけ `review_task` を呼ぶ想定（未解決分を待つ間はレビュー全体を延期する）。
    pub human: HumanVerdicts,
    /// ADR-0016 D3 / M4: 集約 run のレビューなら true。暗黙の条件「`artifacts/summary.md` が存在する」を
    /// `criterion_idx = acceptance.len()`（Plan の暗黙条件があればその次）に加える。
    pub aggregate: bool,
}

pub const SUMMARY_FILE: &str = "artifacts/summary.md";

/// `task.acceptance` を順に判定する。`produced` はその run の `ArtifactProduced`（名前の照合に使う）。
pub async fn review_task(
    task: &Task,
    workspace: &dyn Workspace,
    workspace_dir: &Path,
    produced: &[ArtifactRef],
    command_timeout: Duration,
    extras: ReviewExtras,
) -> ReviewOutcome {
    let ReviewExtras {
        subject,
        plan,
        reviewer,
        human,
        aggregate,
    } = extras;
    let subject = &subject;
    let mut verdicts = Vec::with_capacity(task.acceptance.len() + 1);
    let mut reviewer_criteria: Vec<usize> = Vec::new();
    for (idx, criterion) in task.acceptance.iter().enumerate() {
        let (pass, reason) = match &criterion.check {
            Check::Command { cmd, expect_exit } => {
                match workspace.exec(cmd, command_timeout).await {
                    Err(e) => (false, format!("exec failed: {e}")),
                    Ok(r) if r.timed_out => (
                        false,
                        format!("command timed out after {}s: {cmd}", command_timeout.as_secs()),
                    ),
                    Ok(r) => {
                        let pass = r.exit == Some(*expect_exit);
                        (
                            pass,
                            format!(
                                "cmd={cmd:?} exit={:?} expected={expect_exit} stdout_tail={:?} stderr_tail={:?}",
                                r.exit,
                                tail(&r.stdout_tail, REASON_TAIL),
                                tail(&r.stderr_tail, REASON_TAIL)
                            ),
                        )
                    }
                }
            }
            Check::ArtifactExists { name } => {
                let rel = produced
                    .iter()
                    .rev()
                    .find(|a| &a.name == name)
                    .map(|a| a.path.clone())
                    .unwrap_or_else(|| format!("artifacts/{name}"));
                let full = workspace_dir.join(&rel);
                if full.is_file() {
                    match sha256_file(&full) {
                        Ok(sha) => (true, format!("path={rel} sha256={sha}")),
                        Err(e) => (false, format!("path={rel} unreadable: {e}")),
                    }
                } else {
                    (false, format!("artifact {name:?} not found at {rel}"))
                }
            }
            Check::Reviewer => {
                // 決定的条件の結果を見てから判定する（後段）。
                reviewer_criteria.push(idx);
                continue;
            }
            Check::Human => match human.get(&idx) {
                Some((pass, reason)) => (*pass, reason.clone()),
                // 呼び出し元（dispatcher::spawn_review）は Human criterion が全て解決してから呼ぶので
                // 通常到達しない防御的フォールバック（ADR-0008 D2）。
                None => (false, "human approval state missing".to_string()),
            },
        };
        verdicts.push(Verdict {
            criterion_idx: idx,
            pass,
            reason,
        });
    }

    // ADR-0007 D4: Plan kind は暗黙の条件「artifacts/plan.json が PlanOutput として妥当」を追加する。
    let mut plan_output = None;
    if let Some(check) = &plan {
        let (pass, reason, parsed) = check_plan_file(workspace_dir, check);
        plan_output = parsed;
        verdicts.push(Verdict {
            criterion_idx: task.acceptance.len(),
            pass,
            reason,
        });
    }

    // ADR-0016 D3 / M4: 集約 run は artifacts/summary.md を作っていなければならない（暗黙の条件）。
    if aggregate {
        let idx = task.acceptance.len() + usize::from(plan.is_some());
        let full = workspace_dir.join(SUMMARY_FILE);
        let (pass, reason) = if full.is_file() {
            match sha256_file(&full) {
                Ok(sha) => (true, format!("path={SUMMARY_FILE} sha256={sha}")),
                Err(e) => (false, format!("path={SUMMARY_FILE} unreadable: {e}")),
            }
        } else {
            (false, format!("aggregate run did not produce {SUMMARY_FILE}"))
        };
        verdicts.push(Verdict {
            criterion_idx: idx,
            pass,
            reason,
        });
    }

    // ADR-0007 D5 2./3.: 決定的条件が全 pass のときだけ LLM レビュー run を起動する。
    let mut provider_failure = None;
    let mut reviewer_run = None;
    if !reviewer_criteria.is_empty() {
        let deterministic_ok = verdicts.iter().all(|v| v.pass);
        let reviewer_verdicts = if !deterministic_ok {
            reviewer_criteria
                .iter()
                .map(|&idx| Verdict {
                    criterion_idx: idx,
                    pass: false,
                    reason: "not evaluated: a deterministic check failed".to_string(),
                })
                .collect()
        } else {
            match reviewer {
                None => reviewer_criteria
                    .iter()
                    .map(|&idx| Verdict {
                        criterion_idx: idx,
                        pass: false,
                        reason: "not evaluated: no reviewer run was available".to_string(),
                    })
                    .collect(),
                Some(run) => {
                    let (result, record) =
                        run_reviewer(task, workspace_dir, produced, subject, &reviewer_criteria, run).await;
                    reviewer_run = Some(record);
                    match result {
                        Ok(v) => v,
                        Err(pf) => {
                            // ADR-0010 D5（P-29）: 判定そのものが無効。遷移はディスパッチャが適用しない。
                            provider_failure = Some(pf);
                            Vec::new()
                        }
                    }
                }
            }
        };
        verdicts.extend(reviewer_verdicts);
    }

    verdicts.sort_by_key(|v| v.criterion_idx);
    ReviewOutcome {
        verdicts,
        plan: plan_output,
        provider_failure,
        reviewer_run,
    }
}

fn check_plan_file(workspace_dir: &Path, check: &PlanCheck) -> (bool, String, Option<PlanOutput>) {
    let path = workspace_dir.join(PLAN_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return (false, format!("{PLAN_FILE} not found or unreadable: {e}"), None),
    };
    match parse_and_validate(&text, check.depth, &check.limits, &check.genres) {
        Ok(plan) => {
            let n = plan.tasks.len();
            (true, format!("{PLAN_FILE} is a valid PlanOutput with {n} tasks"), Some(plan))
        }
        Err(e) => (false, format!("{PLAN_FILE}: {e}"), None),
    }
}

/// 合成した `Review` kind のタスク（永続化しない。ADR-0007 D5 3.）。
pub fn synthetic_review_task(subject_task: &Task, run_id: &str, hint: &WorkerHint) -> Task {
    let now = time::OffsetDateTime::now_utc();
    Task {
        id: TaskId::new(),
        parent_id: Some(subject_task.id),
        kind: TaskKind::Review,
        title: format!("Review: {}", subject_task.title),
        objective: subject_task.objective.clone(),
        acceptance: subject_task.acceptance.clone(),
        inputs: vec![],
        depends_on: vec![],
        status: Status::Running,
        priority: subject_task.priority,
        worker_hint: hint.clone(),
        workspace: subject_task.workspace.clone(),
        budget: subject_task.budget,
        attempts: 0,
        lease: Some(task_core::Lease {
            worker_run_id: run_id.to_string(),
            expires_at: now + time::Duration::seconds(subject_task.budget.max_wall_secs as i64),
        }),
        created_at: now,
        updated_at: now,
        role: None,
        genre: None,
        aggregate: false,
        project_id: None,
        milestone_id: None,
        assignee: None,
    }
}

/// Reviewer run を実行し、判定と、その run 自身の結果（`WorkerFinished` 用。ADR-0014 D1）を返す。
async fn run_reviewer(
    task: &Task,
    workspace_dir: &Path,
    produced: &[ArtifactRef],
    subject: &ReviewSubject,
    criteria: &[usize],
    run: ReviewerRun,
) -> (Result<Vec<Verdict>, ReviewerProviderFailure>, ReviewerRunRecord) {
    let mut record = ReviewerRunRecord { run_id: run.run_id.clone(), outcome: String::new(), usage: None };
    let result = run_reviewer_inner(task, workspace_dir, produced, subject, criteria, run, &mut record).await;
    (result, record)
}

async fn run_reviewer_inner(
    task: &Task,
    workspace_dir: &Path,
    produced: &[ArtifactRef],
    subject: &ReviewSubject,
    criteria: &[usize],
    run: ReviewerRun,
    record: &mut ReviewerRunRecord,
) -> Result<Vec<Verdict>, ReviewerProviderFailure> {
    let fail_all = |reason: String| -> Result<Vec<Verdict>, ReviewerProviderFailure> {
        Ok(criteria
            .iter()
            .map(|&idx| Verdict {
                criterion_idx: idx,
                pass: false,
                reason: reason.clone(),
            })
            .collect())
    };
    let review_path = workspace_dir.join(REVIEW_FILE);
    // 前回のレビュー run の出力を今回の結果と誤読しない（ADR-0007 D1）。
    let _ = std::fs::remove_file(&review_path);

    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: synthetic_review_task(task, &run.run_id, &run.hint),
        workspace: workspace_dir.to_path_buf(),
        context: RunContext {
            prior_review: vec![],
            inputs: produced.to_vec(),
            answers: vec![],
            review: Some(ReviewRequest {
                summary: subject.summary.clone(),
                evidence: subject.evidence.clone(),
                criteria: criteria.to_vec(),
            }),
            role: None,
            children: Vec::new(),
            // ADR-0027 D1: Reviewer run は委譲しない（M8 相当）。
            available_genres: Vec::new(),
        },
    };
    let tag = format!("reviewer({})", run.run_id);
    run.sink.progress(&format!("started (adapter={})", run.adapter.id()));
    let outcome = match run.adapter.run(req, &run.run_id, run.limits, run.sink.as_ref()).await {
        Ok(o) => o,
        Err(e) => {
            run.sink.progress(&format!("adapter error: {e}"));
            if let Some(outcome) = crate::dispatcher::provider_failure_outcome(&e) {
                record.outcome = format!("requeue: adapter: {e}");
                return Err(ReviewerProviderFailure {
                    outcome,
                    message: format!("{tag}: {e}"),
                });
            }
            record.outcome = format!("error(retryable=false): adapter error: {e}");
            return fail_all(format!("{tag}: adapter error: {e}"));
        }
    };
    match outcome.terminal {
        Terminal::Done { summary, usage, .. } => {
            run.sink.progress(&format!("done: {summary}"));
            record.outcome = format!("done: {summary}");
            record.usage = usage;
        }
        Terminal::Question { text } => {
            run.sink.progress(&format!("question: {text}"));
            record.outcome = format!("question: {text}");
            return fail_all(format!("{tag}: reviewer asked a question instead of judging: {text}"));
        }
        Terminal::Error { message, retryable } => {
            run.sink.progress(&format!("error(retryable={retryable}): {message}"));
            record.outcome = format!("error(retryable={retryable}): {message}");
            return fail_all(format!("{tag}: reviewer run failed: {message}"));
        }
    }
    let text = match std::fs::read_to_string(&review_path) {
        Ok(t) => t,
        Err(e) => return fail_all(format!("{tag}: {REVIEW_FILE} not found or unreadable: {e}")),
    };
    let output: ReviewOutput = match serde_json::from_str(&text) {
        Ok(o) => o,
        Err(e) => return fail_all(format!("{tag}: {REVIEW_FILE} is not a valid ReviewOutput: {e}")),
    };
    Ok(criteria
        .iter()
        .map(|&idx| match output.verdicts.iter().rev().find(|v| v.criterion == idx) {
            Some(v) => Verdict {
                criterion_idx: idx,
                pass: v.pass,
                reason: format!("{tag}: {}", v.reason),
            },
            None => Verdict {
                criterion_idx: idx,
                pass: false,
                reason: format!("{tag}: no verdict for criterion {idx} in {REVIEW_FILE}"),
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use task_core::*;
    use task_worker::{AdapterError, LocalWorkspace, RunOutcome};

    fn task_with(checks: Vec<Check>, dir: &Path) -> Task {
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: checks
                .into_iter()
                .map(|check| Criterion {
                    text: "c".into(),
                    check,
                })
                .collect(),
            inputs: vec![],
            depends_on: vec![],
            status: Status::Reviewing,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from(dir),
            },
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 10,
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
        }
    }

    async fn plain_review(task: &Task, ws: &LocalWorkspace, dir: &Path, produced: &[ArtifactRef], t: Duration) -> Vec<Verdict> {
        review_task(task, ws, dir, produced, t, ReviewExtras::default())
            .await
            .verdicts
    }

    #[tokio::test]
    async fn command_checks_are_re_executed_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("present.txt"), "x").unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let task = task_with(
            vec![
                Check::Command {
                    cmd: "test -f present.txt".into(),
                    expect_exit: 0,
                },
                Check::Command {
                    cmd: "test -f absent.txt".into(),
                    expect_exit: 0,
                },
                Check::Command {
                    cmd: "exit 7".into(),
                    expect_exit: 7,
                },
                Check::Command {
                    cmd: "sleep 30".into(),
                    expect_exit: 0,
                },
            ],
            dir.path(),
        );
        let v = plain_review(&task, &ws, dir.path(), &[], Duration::from_millis(300)).await;
        assert_eq!(v.iter().map(|x| x.pass).collect::<Vec<_>>(), vec![true, false, true, false]);
        assert!(v[3].reason.contains("timed out"));
        assert_eq!(v[1].criterion_idx, 1);
    }

    #[tokio::test]
    async fn artifact_exists_uses_produced_path_then_fallback_and_unsupported_kinds_fail() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts/sub")).unwrap();
        std::fs::write(dir.path().join("artifacts/sub/bench.json"), "{}").unwrap();
        std::fs::write(dir.path().join("artifacts/report.md"), "# r").unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let produced = vec![ArtifactRef {
            name: "bench".into(),
            path: "artifacts/sub/bench.json".into(),
            sha256: String::new(),
            kind: "json".into(),
        }];
        let task = task_with(
            vec![
                Check::ArtifactExists { name: "bench".into() },
                Check::ArtifactExists { name: "report.md".into() },
                Check::ArtifactExists { name: "missing".into() },
                Check::Reviewer,
                Check::Human,
            ],
            dir.path(),
        );
        let v = plain_review(&task, &ws, dir.path(), &produced, Duration::from_secs(5)).await;
        assert_eq!(v.iter().map(|x| x.criterion_idx).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
        assert_eq!(v.iter().map(|x| x.pass).collect::<Vec<_>>(), vec![true, true, false, false, false]);
        assert!(v[0].reason.contains("sha256=44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"));
        assert!(v[1].reason.contains("artifacts/report.md"));
        // 決定的条件に fail があるので Reviewer 条件は評価されない。
        assert!(v[3].reason.contains("not evaluated"), "{}", v[3].reason);
        // extras.human が空なので防御的フォールバックになる（ADR-0008 D2: 通常呼び出し元が先に解決する）。
        assert!(v[4].reason.contains("human approval state missing"), "{}", v[4].reason);
    }

    /// ADR-0008 D2: `extras.human` に解決済みの `(pass, reason)` があれば、その内容がそのまま検証結果になる。
    #[tokio::test]
    async fn human_check_uses_resolved_verdict_from_extras() {
        let dir = tempfile::tempdir().unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let task = task_with(vec![Check::Human, Check::Human], dir.path());
        let mut human = HashMap::new();
        human.insert(0, (true, "approved by human".to_string()));
        human.insert(1, (false, "rejected by human: needs more work".to_string()));
        let out = review_task(
            &task,
            &ws,
            dir.path(),
            &[],
            Duration::from_secs(5),
            ReviewExtras { human, ..Default::default() },
        )
        .await;
        assert_eq!(out.verdicts.iter().map(|v| v.pass).collect::<Vec<_>>(), vec![true, false]);
        assert!(out.verdicts[0].reason.contains("approved by human"));
        assert!(out.verdicts[1].reason.contains("rejected by human"));
    }

    /// 同プロセスで `artifacts/review.json` を書く（または書かない）テスト用アダプタ。
    struct StubReviewer {
        review_json: Option<String>,
        terminal: Terminal,
        seen: Mutex<Vec<RunRequest>>,
    }

    #[async_trait]
    impl WorkerAdapter for StubReviewer {
        fn id(&self) -> &str {
            "stub-reviewer"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            sink.progress("judging");
            if let Some(json) = &self.review_json {
                std::fs::create_dir_all(req.workspace.join("artifacts")).unwrap();
                std::fs::write(req.workspace.join(REVIEW_FILE), json).unwrap();
            }
            self.seen.lock().unwrap().push(req);
            Ok(RunOutcome {
                terminal: self.terminal.clone(),
                exit_code: Some(0),
            })
        }
    }

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<String>>);
    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.0.lock().unwrap().push(msg.to_string());
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
    }

    fn reviewer_run(adapter: Arc<StubReviewer>) -> ReviewerRun {
        ReviewerRun {
            adapter,
            run_id: "rev-1".into(),
            limits: RunLimits {
                wall_clock: Duration::from_secs(5),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
            },
            sink: Box::new(RecordingSink::default()),
            hint: reviewer_hint(),
        }
    }

    /// 常に供給側失敗を返すレビュー用アダプタ。
    struct ThrottledReviewer;

    #[async_trait]
    impl WorkerAdapter for ThrottledReviewer {
        fn id(&self) -> &str {
            "throttled"
        }
        async fn run(
            &self,
            _req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            Err(AdapterError::Throttled { retry_after: Duration::from_secs(3) })
        }
    }

    /// ADR-0010 D5（P-29）: Reviewer run の供給側失敗は fail の判定にせず `provider_failure` として返す。
    #[tokio::test]
    async fn reviewer_provider_failure_is_reported_instead_of_failing_criteria() {
        let dir = tempfile::tempdir().unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let task = task_with(vec![Check::Command { cmd: "true".into(), expect_exit: 0 }, Check::Reviewer], dir.path());
        let run = ReviewerRun {
            adapter: Arc::new(ThrottledReviewer),
            run_id: "rev-x".into(),
            limits: RunLimits {
                wall_clock: Duration::from_secs(5),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
            },
            sink: Box::new(RecordingSink::default()),
            hint: reviewer_hint(),
        };
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { reviewer: Some(run), ..Default::default() }).await;
        let pf = out.provider_failure.expect("provider failure");
        assert_eq!(pf.outcome, ProviderOutcome::Throttled { retry_after: Duration::from_secs(3) });
        assert!(pf.message.contains("reviewer(rev-x)"), "{}", pf.message);
        // 決定的条件の判定だけが残り、Reviewer 条件の verdict は作らない。
        assert_eq!(out.verdicts.iter().map(|v| v.criterion_idx).collect::<Vec<_>>(), vec![0]);
    }

    #[tokio::test]
    async fn reviewer_check_runs_review_task_through_adapter_and_reads_review_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), "x").unwrap();
        // 前回のレビュー結果が残っていても消される。
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(dir.path().join(REVIEW_FILE), r#"{"verdicts":[{"criterion":1,"pass":true,"reason":"stale"}]}"#).unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let task = task_with(
            vec![
                Check::Command { cmd: "test -f ok.txt".into(), expect_exit: 0 },
                Check::Reviewer,
                Check::Reviewer,
            ],
            dir.path(),
        );
        let adapter = Arc::new(StubReviewer {
            review_json: Some(
                r#"{"verdicts":[{"criterion":1,"pass":true,"reason":"looks right"},{"criterion":2,"pass":false,"reason":"missing docs"}]}"#
                    .into(),
            ),
            terminal: Terminal::Done { summary: "reviewed".into(), evidence: vec![], usage: None },
            seen: Mutex::new(vec![]),
        });
        let produced = vec![ArtifactRef { name: "a".into(), path: "artifacts/a".into(), sha256: "0".into(), kind: "file".into() }];
        let subject = ReviewSubject {
            summary: "did the thing".into(),
            evidence: vec![Evidence { criterion: 0, command: Some("test -f ok.txt".into()), exit: Some(0), stdout_tail: None }],
        };
        let out = review_task(&task, &ws, dir.path(), &produced, Duration::from_secs(5), ReviewExtras { subject: subject.clone(), plan: None, reviewer: Some(reviewer_run(adapter.clone())), ..Default::default() }).await;
        assert_eq!(out.verdicts.iter().map(|v| (v.criterion_idx, v.pass)).collect::<Vec<_>>(), vec![(0, true), (1, true), (2, false)]);
        assert!(out.verdicts[1].reason.contains("reviewer(rev-1): looks right"), "{}", out.verdicts[1].reason);
        assert!(out.verdicts[2].reason.contains("missing docs"));
        assert!(out.plan.is_none());
        // アダプタには合成 Review タスクと context.review が渡る。
        let seen = adapter.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let req = &seen[0];
        assert_eq!(req.task.kind, TaskKind::Review);
        assert_eq!(req.task.parent_id, Some(task.id));
        assert_eq!(req.task.acceptance.len(), 3);
        assert_eq!(req.task.worker_hint.tier, Tier::Standard);
        let review = req.context.review.as_ref().unwrap();
        assert_eq!(review.criteria, vec![1, 2]);
        assert_eq!(review.summary, "did the thing");
        assert_eq!(review.evidence.len(), 1);
        assert_eq!(req.context.inputs, produced);
    }

    #[tokio::test]
    async fn reviewer_run_failure_or_missing_verdict_fails_reviewer_criteria() {
        let dir = tempfile::tempdir().unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let task = task_with(vec![Check::Reviewer, Check::Reviewer], dir.path());
        let subject = ReviewSubject::default();

        // done だが review.json が無い。
        let adapter = Arc::new(StubReviewer {
            review_json: None,
            terminal: Terminal::Done { summary: "s".into(), evidence: vec![], usage: None },
            seen: Mutex::new(vec![]),
        });
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { subject: subject.clone(), plan: None, reviewer: Some(reviewer_run(adapter)), ..Default::default() }).await;
        assert!(out.verdicts.iter().all(|v| !v.pass));
        assert!(out.verdicts[0].reason.contains("review.json not found"), "{}", out.verdicts[0].reason);

        // error 終端。
        let adapter = Arc::new(StubReviewer {
            review_json: Some(r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"x"},{"criterion":1,"pass":true,"reason":"y"}]}"#.into()),
            terminal: Terminal::Error { message: "boom".into(), retryable: true },
            seen: Mutex::new(vec![]),
        });
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { subject: subject.clone(), plan: None, reviewer: Some(reviewer_run(adapter)), ..Default::default() }).await;
        assert!(out.verdicts.iter().all(|v| !v.pass && v.reason.contains("boom")));

        // 判定の欠落（criterion 1 が無い）。
        let adapter = Arc::new(StubReviewer {
            review_json: Some(r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"x"}]}"#.into()),
            terminal: Terminal::Done { summary: "s".into(), evidence: vec![], usage: None },
            seen: Mutex::new(vec![]),
        });
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { subject: subject.clone(), plan: None, reviewer: Some(reviewer_run(adapter)), ..Default::default() }).await;
        assert_eq!(out.verdicts.iter().map(|v| v.pass).collect::<Vec<_>>(), vec![true, false]);
        assert!(out.verdicts[1].reason.contains("no verdict for criterion 1"));

        // reviewer run が無い（ディスパッチャが供給できなかった）。
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { subject: subject.clone(), plan: None, reviewer: None, ..Default::default() }).await;
        assert!(out.verdicts.iter().all(|v| !v.pass && v.reason.contains("no reviewer run")));
    }

    #[tokio::test]
    async fn plan_kind_adds_implicit_plan_file_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let ws = LocalWorkspace::new(dir.path());
        let mut task = task_with(vec![], dir.path());
        task.kind = TaskKind::Plan;
        let check = PlanCheck { depth: 1, limits: PlanLimits::default(), genres: vec![] };

        // ファイル無し。
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { plan: Some(check.clone()), ..Default::default() }).await;
        assert_eq!(out.verdicts.len(), 1);
        assert_eq!(out.verdicts[0].criterion_idx, 0);
        assert!(!out.verdicts[0].pass);
        assert!(out.verdicts[0].reason.contains("plan.json not found"));
        assert!(out.plan.is_none());

        // 不正（依存が範囲外）。
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join(PLAN_FILE),
            r#"{"tasks":[{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"command","cmd":"true","expect_exit":0}}],"depends_on":[5]}]}"#,
        )
        .unwrap();
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { plan: Some(check.clone()), ..Default::default() }).await;
        assert!(!out.verdicts[0].pass);
        assert!(out.verdicts[0].reason.contains("out of range"), "{}", out.verdicts[0].reason);

        // 妥当。acceptance に Command 条件があれば idx 0、plan は idx 1。
        std::fs::write(
            dir.path().join(PLAN_FILE),
            r#"{"tasks":[{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"reviewer"}}]},{"title":"b","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],"depends_on":[0]}]}"#,
        )
        .unwrap();
        task.acceptance.push(Criterion { text: "c".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } });
        let out = review_task(&task, &ws, dir.path(), &[], Duration::from_secs(5), ReviewExtras { plan: Some(check.clone()), ..Default::default() }).await;
        assert_eq!(out.verdicts.iter().map(|v| (v.criterion_idx, v.pass)).collect::<Vec<_>>(), vec![(0, true), (1, true)]);
        assert!(out.verdicts[1].reason.contains("2 tasks"));
        assert_eq!(out.plan.unwrap().tasks.len(), 2);
    }
}
