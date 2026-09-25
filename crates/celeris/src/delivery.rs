//! ADR-0051: 部署レビュアーのマージ可否判定 → merge → release/verify。
//! 判断は既存の独立したReviewer run。ここは永続状態を決定的に進めるだけで promote は呼ばない。
use crate::config::Config;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use task_core::{
    Delivery, DeliveryState as State, Event, Message, MessageId, MessageRole, RepairClass, Status,
    StoreError, TaskStore, WorkUnitKind, WorkUnitStatus,
};
use task_ops::changes::{git, git_with_env};
use time::OffsetDateTime;

/// ADR-0051 Phase 106追記: push の待ち時間。ローカルの merge/rev-parse より長め
/// （ssh 越しの origin を想定。`BatchMode=yes` で対話的な認証には絶対に落ちない）。
const PUSH_TIMEOUT: Duration = Duration::from_secs(120);
/// 1度だけ再試行したがなお失敗した `push_error` の目印。通知文では外して見せる
/// （[`push_error_display`]）。この目印が付いていたら以後は触らない。
const PUSH_RETRIED_PREFIX: &str = "[retried] ";
const MERGE_BASE_FAILURE: &str = "[merge-base] ";

/// 配送で局所修復できる失敗だけを分類する。gate の他の step は従来の Reopen に残す。
fn classify_delivery_failure(
    state: State,
    detail: &str,
    failed_step: Option<&str>,
) -> Option<RepairClass> {
    if state != State::Blocked {
        return None;
    }
    if detail.starts_with(MERGE_BASE_FAILURE) {
        return Some(RepairClass::MergeBase);
    }
    if detail.starts_with("リリース準備に失敗") && failed_step == Some("cargo-fmt-check") {
        return Some(RepairClass::Format);
    }
    None
}

fn git_text(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo, args, Duration::from_secs(20)).ok_or("git を起動できません")?;
    if out.ok {
        Ok(out.stdout.trim().into())
    } else {
        Err(out.stderr.chars().take(1000).collect())
    }
}
fn sha(repo: &Path, reference: &str) -> Result<String, String> {
    git_text(repo, &["rev-parse", "--verify", reference])
}

fn push_error_display(err: &str) -> &str {
    err.strip_prefix(PUSH_RETRIED_PREFIX).unwrap_or(err)
}

/// stderr の末尾 `max` バイト（文字境界を守る）。
fn tail_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// origin が既にこのブランチと同じかそれより先か（省略できるか）。判定できなければ
/// （リモート追跡ブランチが無い等）省略しない。
fn push_already_up_to_date(repo: &Path, remote: &str, branch: &str) -> bool {
    git_text(
        repo,
        &[
            "rev-list",
            "--count",
            &format!("{remote}/{branch}..{branch}"),
        ],
    )
    .map(|s| s.trim() == "0")
    .unwrap_or(false)
}

/// ADR-0051 Phase 106: `merge_reviewed` が成功した直後、release.sh を起こす前に呼ぶ純粋な git 呼び出し。
/// 非対話（`git()` が既定で `GIT_TERMINAL_PROMPT=0` を設定。ここでは `GIT_SSH_COMMAND` に
/// `-o BatchMode=yes` を足す）。force push はしない。
fn push_merged(repo: &Path, remote: &str, branch: &str, timeout: Duration) -> Result<(), String> {
    if push_already_up_to_date(repo, remote, branch) {
        return Ok(());
    }
    let ssh_command = match std::env::var("GIT_SSH_COMMAND") {
        Ok(existing) if !existing.trim().is_empty() => format!("{existing} -o BatchMode=yes"),
        _ => "ssh -o BatchMode=yes".to_string(),
    };
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    match git_with_env(
        repo,
        &["push", remote, &refspec],
        &[("GIT_SSH_COMMAND", ssh_command.as_str())],
        timeout,
    ) {
        Some(o) if o.ok => Ok(()),
        Some(o) => Err(tail_bytes(o.stderr.trim(), 500)),
        None => Err("git を起動できません".into()),
    }
}

/// push が失敗して（まだ再試行の目印が付いていなければ）1度だけ再試行する。release の準備を
/// 止めないので、これは `advance` の状態遷移そのものではなく独立した CAS 保存。
fn maybe_retry_push(
    store: &dyn TaskStore,
    config: &Config,
    old: &Delivery,
    now: OffsetDateTime,
) -> Result<Delivery, StoreError> {
    if !config.selfdeploy.push || old.release.is_none() || old.pushed_at.is_some() {
        return Ok(old.clone());
    }
    let Some(err) = &old.push_error else {
        return Ok(old.clone());
    };
    if err.starts_with(PUSH_RETRIED_PREFIX) {
        return Ok(old.clone());
    }
    let mut next = old.clone();
    match push_merged(
        &config.selfdeploy.repo,
        &config.selfdeploy.push_remote,
        &old.default_branch,
        PUSH_TIMEOUT,
    ) {
        Ok(()) => {
            next.pushed_at = Some(now);
            next.push_error = None;
        }
        Err(e) => next.push_error = Some(format!("{PUSH_RETRIED_PREFIX}{e}")),
    }
    Ok(if store.delivery_save(Some(old), &next)? {
        next
    } else {
        old.clone()
    })
}
pub fn tick(store: &dyn TaskStore, config: &Config, now: OffsetDateTime) -> Result<(), StoreError> {
    for delivery in store.delivery_list()? {
        if config
            .selfdeploy
            .delivery_projects
            .contains(&delivery.project_id.to_string())
        {
            advance(store, config, &delivery, now)?;
        }
    }
    Ok(())
}

fn validate_candidate(repo: &Path, d: &Delivery) -> Result<(), String> {
    if sha(repo, &format!("refs/heads/{}", d.branch))? != d.head
        || sha(repo, &format!("refs/heads/{}", d.default_branch))? != d.base
    {
        return Err(
            "対象コミットまたは既定ブランチが変わりました。更新して再レビューが必要です".into(),
        );
    }
    if let Err(stderr) = git_text(repo, &["merge-base", "--is-ancestor", &d.base, &d.head]) {
        return Err(format!(
            "git merge-base --is-ancestor {} {} failed; stderr: {stderr}",
            d.base, d.head
        ));
    }
    Ok(())
}

/// 承認した SHA だけを早送り。git 自身が無関係な未コミット変更を保持する。
fn merge_reviewed(repo: &Path, d: &Delivery) -> Result<(), String> {
    validate_candidate(repo, d)?;
    if task_ops::changes::current_branch(repo).as_deref() != Some(d.default_branch.as_str()) {
        return Err("自己リポジトリが既定ブランチをチェックアウトしていません".into());
    }
    git_text(
        repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "merge",
            "--ff-only",
            "--no-autostash",
            &d.head,
        ],
    )?;
    if sha(repo, "HEAD")? != d.head {
        return Err("取り込み後のSHAが一致しません".into());
    }
    Ok(())
}

fn make_repair(
    store: &dyn TaskStore,
    config: &Config,
    d: &Delivery,
    class: RepairClass,
    now: OffsetDateTime,
) -> Result<Option<String>, StoreError> {
    let units = store.work_units_for(d.task_id)?;
    let repairs: Vec<_> = units
        .iter()
        .filter(|u| u.kind == WorkUnitKind::Repair)
        .collect();
    let same_class = repairs
        .iter()
        .filter(|u| {
            u.spec
                .title
                .starts_with(&format!("repair ({}):", class.bucket()))
        })
        .count();
    if repairs.len() as u32 >= config.execution.max_repairs
        || same_class as u32 >= config.execution.max_repairs_per_class
    {
        return Ok(None);
    }
    let branch_head = sha(&config.selfdeploy.repo, &format!("refs/heads/{}", d.branch))
        .unwrap_or_else(|_| d.head.clone());
    let main_head = sha(
        &config.selfdeploy.repo,
        &format!("refs/heads/{}", d.default_branch),
    )
    .unwrap_or_else(|_| d.base.clone());
    let output = if class == RepairClass::Format {
        let log = config
            .selfdeploy
            .releases_dir
            .join(d.release.as_deref().unwrap_or(""))
            .join(".gate-cargo-fmt-check.log");
        std::fs::read_to_string(log)
            .map(|s| tail_bytes(&s, 4000))
            .unwrap_or_default()
    } else {
        tail_bytes(&d.detail, 4000)
    };
    let details = vec![
        format!("対象ブランチ {}: {}", d.branch, branch_head),
        format!("既定ブランチ {}: {}", d.default_branch, main_head),
        format!("失敗した検査の出力:\n{output}"),
    ];
    let (max_turns, max_wall_secs) = class.budget();
    let n = repairs.len() + 1;
    let key = format!("repair-{n}");
    let spec = task_core::WorkUnitSpec {
        key: key.clone(),
        kind: WorkUnitKind::Repair,
        title: format!("repair ({}): 配送の局所修復", class.bucket()),
        objective: task_core::build_repair_objective(class, &details, "配送", "", None),
        depends_on: vec![],
        done_when: vec![],
        checks: vec![],
        context: Default::default(),
        harness: None,
        features: None,
        budget: Some(task_core::WorkUnitBudget {
            max_turns: Some(max_turns),
            max_wall_secs: Some(max_wall_secs),
        }),
        outputs: vec![],
    };
    let stamp = now
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
    let active = store.execution_plan_active(d.task_id)?;
    let (new_plan, rows, event) = if let Some(plan) = active {
        let row = task_core::WorkUnitRow::new(
            task_core::new_id(),
            d.task_id.to_string(),
            plan.id,
            units.iter().map(|u| u.seq).max().unwrap_or(0) + 1,
            spec,
            WorkUnitStatus::Ready,
            stamp,
        );
        let ev = Event::WorkUnitTransitioned {
            work_unit_id: row.id.clone(),
            key: key.clone(),
            from: WorkUnitStatus::Pending,
            to: WorkUnitStatus::Ready,
            reason: "delivery_repair".into(),
            run_id: None,
        };
        (None, vec![row], ev)
    } else {
        let task = store
            .get(d.task_id)?
            .ok_or_else(|| StoreError::Invalid("task missing".into()))?;
        let plan_id = task_core::new_id();
        let main = task_core::WorkUnitSpec {
            key: "main".into(),
            kind: WorkUnitKind::Implement,
            title: task.title,
            objective: task.objective,
            depends_on: vec![],
            done_when: vec![],
            checks: vec![],
            context: Default::default(),
            harness: None,
            features: None,
            budget: None,
            outputs: vec![],
        };
        let main_row = task_core::WorkUnitRow::new(
            task_core::new_id(),
            d.task_id.to_string(),
            plan_id.clone(),
            0,
            main.clone(),
            WorkUnitStatus::Done,
            stamp.clone(),
        );
        let repair_row = task_core::WorkUnitRow::new(
            task_core::new_id(),
            d.task_id.to_string(),
            plan_id.clone(),
            1,
            spec.clone(),
            WorkUnitStatus::Ready,
            stamp.clone(),
        );
        let plan_spec = task_core::ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.into(),
            rationale: "delivery repair: 暗黙の WorkUnit を実体化".into(),
            work_units: vec![main, spec],
        };
        let plan = task_core::ExecutionPlanRow {
            id: plan_id.clone(),
            task_id: d.task_id.to_string(),
            version: 1,
            origin: task_core::PlanOrigin::Repair,
            planner_run_id: None,
            status: task_core::PlanStatus::Active,
            spec: plan_spec.clone(),
            created_at: stamp,
            superseded_at: None,
        };
        let ev = Event::ExecutionPlanned {
            plan_id,
            version: 1,
            origin: task_core::PlanOrigin::Repair,
            supersedes: None,
            reason: Some("delivery_repair".into()),
            plan: Box::new(plan_spec),
        };
        (Some(plan), vec![main_row, repair_row], ev)
    };
    store.delivery_repair_apply(d.task_id, vec![event], new_plan, rows)?;
    Ok(Some(key))
}

fn advance(
    store: &dyn TaskStore,
    config: &Config,
    old: &Delivery,
    now: OffsetDateTime,
) -> Result<(), StoreError> {
    // ADR-0051 Phase 106追記: mergeが既に済んでいてpushが未成功・未再試行なら、状態遷移とは独立に
    // 1度だけ再試行する（`old` を最新の永続状態に差し替えてから通常の遷移へ進む）。
    let retried = maybe_retry_push(store, config, old, now)?;
    let old = &retried;
    let mut d = old.clone();
    let repo = &config.selfdeploy.repo;
    if old.state == State::MergeQueued {
        if !store
            .get(old.task_id)?
            .is_some_and(|t| t.status == Status::Done)
        {
            // 委譲した子や集約待ちを含め、元の仕事のdoneを待つ。
            return Ok(());
        }
        if let Some(marker) =
            task_ops::workspace::read_marker(&config.workspace_root.join(old.task_id.to_string()))
            && marker
                .repos
                .iter()
                .any(|r| task_ops::changes::is_dirty(Path::new(&r.dir)) != Some(false))
        {
            d.state = State::Blocked;
            d.detail = format!(
                "{MERGE_BASE_FAILURE}実装の作業ツリーに未コミットの変更があります。コミット後に再レビューしてください"
            );
            store.delivery_save(Some(old), &d)?;
            return Ok(());
        }
    }
    match old.state {
        State::Reviewing => {
            if store
                .get(old.task_id)?
                .is_some_and(|t| t.status.is_terminal())
            {
                d.state = State::Blocked;
                d.detail = "レビュアーのマージ判定が記録されず終了しました。部署内で実行結果を確認してください".into();
                store.delivery_save(Some(old), &d)?;
            }
        }
        State::MergeQueued => {
            d.state = State::Merging;
            if !store.delivery_save(Some(old), &d)? {
                return Ok(());
            }
            let claimed = d.clone();
            match merge_reviewed(repo, &d) {
                Ok(()) => {
                    d.state = State::Preparing;
                    d.release = Some(d.head[..12].into());
                    d.detail =
                        "部署内レビュー合格・マージ済み。ビルドとリリース検証を実行中です".into();
                    // ADR-0051 Phase 106: release.sh（start_prepare）を起こす前にoriginへpush。
                    // 失敗してもrelease準備は止めない（下のstart_prepareはこの結果を待たない）。
                    if config.selfdeploy.push {
                        match push_merged(
                            repo,
                            &config.selfdeploy.push_remote,
                            &d.default_branch,
                            PUSH_TIMEOUT,
                        ) {
                            Ok(()) => d.pushed_at = Some(now),
                            Err(e) => d.push_error = Some(e),
                        }
                    }
                }
                Err(e) => {
                    d.state = State::Blocked;
                    d.detail = format!("{MERGE_BASE_FAILURE}{e}");
                }
            }
            if !store.delivery_save(Some(&claimed), &d)? {
                return Ok(());
            }
            if d.state == State::Preparing {
                let mut integration = task_core::TaskIntegration::new(
                    d.task_id,
                    Some(d.repo_id),
                    &d.repo,
                    task_core::IntegrationMethod::Merge,
                    task_core::IntegrationState::Done,
                    now,
                )
                .with_detail(format!(
                    "部署のレビュアーの承認により {} を取り込み。worktreeとブランチは証跡として保持",
                    d.head
                ));
                integration.merged_at = Some(now);
                store.integration_put(&integration)?;
                start_prepare(store, config, &d)?;
            }
        }
        State::Merging => {
            // 再起動を跨いだ外部操作は自動で再実行しない。
            d.state = State::Blocked;
            d.detail = "取り込み処理が中断しました。gitと承認SHAの照合が必要です".into();
            store.delivery_save(Some(old), &d)?;
        }
        State::Preparing => {
            let dir = preparation_dir(config, old);
            if let Some(result) = read_json(&dir.join("result.json")) {
                let rel = config
                    .selfdeploy
                    .releases_dir
                    .join(old.release.as_deref().unwrap_or(""));
                let verified = read_json(&rel.join("verify.json")).is_some_and(|v| v["ok"] == true);
                let built = read_json(&rel.join("gate.json")).is_some_and(|v| v["ok"] == true);
                let same = read_json(&rel.join("manifest.json"))
                    .is_some_and(|v| v["sha"].as_str() == Some(&old.head));
                if result["ok"] == true && built && verified && same {
                    d.state = State::Ready;
                    d.detail = "部署内レビュー、マージ、ビルド、検証が完了しました。リリース画面のデプロイ操作を待っています（本番は未変更）".into();
                } else {
                    d.state = State::Blocked;
                    d.detail = format!(
                        "リリース準備に失敗しました。ログ: {}",
                        dir.join("prepare.log").display()
                    );
                }
                store.delivery_save(Some(old), &d)?;
            } else if !old.prepare_pid.is_some_and(crate::instance::pid_alive) {
                d.state = State::Blocked;
                d.detail = format!(
                    "リリース準備が中断しました。ログ: {}",
                    dir.join("prepare.log").display()
                );
                store.delivery_save(Some(old), &d)?;
            }
        }
        State::Ready | State::Blocked => {
            if old.notification.is_some() {
                return Ok(());
            }
            // repair WU とそのレビューが終わったら、新しい ref で配送を再検証する。
            let latest_repair = store
                .work_units_for(old.task_id)?
                .into_iter()
                .filter(|u| u.kind == WorkUnitKind::Repair)
                .max_by_key(|u| u.seq);
            let repaired_and_not_requeued = latest_repair
                .as_ref()
                .is_some_and(|u| u.status == WorkUnitStatus::Done)
                && !store.comments_for(old.task_id)?.iter().any(|c| {
                    latest_repair
                        .as_ref()
                        .is_some_and(|u| c.body == format!("[delivery-repair-requeued] {}", u.key))
                });
            if old.state == State::Blocked
                && old.decision == Some(true)
                && store
                    .get(old.task_id)?
                    .is_some_and(|t| t.status == Status::Done)
                && repaired_and_not_requeued
            {
                d.base = sha(repo, &format!("refs/heads/{}", d.default_branch))
                    .unwrap_or_else(|_| old.base.clone());
                d.head = sha(repo, &format!("refs/heads/{}", d.branch))
                    .unwrap_or_else(|_| old.head.clone());
                d.state = State::MergeQueued;
                d.release = None;
                d.prepare_pid = None;
                d.detail = "局所修復後の配送を再検証します".into();
                if store.delivery_save(Some(old), &d)?
                    && let Some(u) = latest_repair
                {
                    store.comment_add(
                        &task_core::TaskComment {
                            id: task_core::CommentId::new(),
                            task_id: old.task_id,
                            author_kind: task_core::CommentAuthorKind::System,
                            author: None,
                            run_id: None,
                            created_at: now,
                            body: format!("[delivery-repair-requeued] {}", u.key),
                        },
                        None,
                    )?;
                }
                return Ok(());
            }
            let needs_user =
                old.state == State::Ready || old.detail.trim_start().starts_with("[needs-human]");
            let node = if needs_user {
                store
                    .org_list()?
                    .into_iter()
                    .find(|n| n.kind == task_core::OrgKind::Secretary)
            } else {
                store.org_get(&old.department)?
            };
            let Some(node) = node else { return Ok(()) };
            // 技術的な差し戻しは通常のworker再試行が先。最終失敗/準備失敗だけを部署に戻す。
            if !needs_user
                && store
                    .get(old.task_id)?
                    .is_some_and(|t| !t.status.is_terminal())
            {
                return Ok(());
            }
            // マージ/ビルドの技術的な不備は実装担当へ一度だけ戻す。コメントと再開は同一transaction。
            const REPAIR: &str = "[delivery-repair]";
            if !needs_user
                && old.decision == Some(true)
                && store
                    .get(old.task_id)?
                    .is_some_and(|t| t.status == Status::Done)
            {
                let rel = config
                    .selfdeploy
                    .releases_dir
                    .join(old.release.as_deref().unwrap_or(""));
                let gate = read_json(&rel.join("gate.json"));
                let failed_step = gate
                    .as_ref()
                    .filter(|v| v["ok"] == false)
                    .and_then(|v| v["failed_step"].as_str());
                if let Some(class) = classify_delivery_failure(old.state, &old.detail, failed_step)
                {
                    if let Some(key) = make_repair(store, config, old, class, now)? {
                        store.comment_add(&task_core::TaskComment {
                            id: task_core::CommentId::new(), task_id: old.task_id,
                            author_kind: task_core::CommentAuthorKind::System, author: None,
                            run_id: None, created_at: now,
                            body: format!("{REPAIR} {key} class={}。対象ブランチと既定ブランチ、失敗した検査だけを渡して局所修復します。", class.bucket()),
                        }, None)?;
                    } else {
                        d.detail = format!(
                            "[needs-human] 配送の局所修復が上限に達しました: {}",
                            class.bucket()
                        );
                        store.delivery_save(Some(old), &d)?;
                    }
                    return Ok(());
                }
            }
            if !needs_user
                && old.decision == Some(true)
                && store
                    .get(old.task_id)?
                    .is_some_and(|t| t.status == Status::Done)
                && !store.comments_for(old.task_id)?.iter().any(|c| {
                    c.author_kind == task_core::CommentAuthorKind::System
                        && c.body.starts_with(REPAIR)
                })
            {
                store.comment_add(&task_core::TaskComment {
                    id:task_core::CommentId::new(),task_id:old.task_id,author_kind:task_core::CommentAuthorKind::System,author:None,run_id:None,created_at:now,
                    body:format!("{REPAIR} 部署内の取り込み・リリース検証で差し戻し。元の実装依頼を保持し、次の問題を修正してコミットと検証まで行ってください。必要なら自分のworktreeに現在の既定ブランチを取り込んでください。元のチェックアウトや本番サービスは変更せず、デプロイも実行しないでください。\n{}",old.detail)
                },Some((task_core::Trigger::Reopen,Vec::new())))?;
                return Ok(());
            }
            // review run由来の固定MessageIdで、追記後に落ちても重複しない。
            let id = MessageId(ulid::Ulid::from(
                u128::from(d.review_run.parse::<ulid::Ulid>().unwrap_or(d.task_id.0))
                    ^ 0x64656c6976657279u128,
            ));
            if !store
                .message_list(&node.id, Some(d.project_id), 1000)?
                .iter()
                .any(|m| m.id == id)
            {
                let title = store.get(d.task_id)?.map(|t| t.title).unwrap_or_default();
                let label = if d.state == State::Ready {
                    "デプロイ準備完了"
                } else {
                    "取り込み・リリース準備の確認が必要"
                };
                let release = d
                    .release
                    .as_ref()
                    .map(|s| format!("\n[リリース {s} を確認してデプロイ](/releases#release-{s})"))
                    .unwrap_or_default();
                // ADR-0051 Phase 106追記: origin へのpush状況を1行足す。push=falseで一度も
                // 試みていなければ何も足さない。
                let push_line = if let Some(head) = d.pushed_at.is_some().then(|| d.head.clone()) {
                    format!("\norigin へ push 済み: {head}")
                } else if let Some(err) = d.push_error.as_deref().map(push_error_display) {
                    format!("\n**push に失敗**: {err}。人が `git push` してください")
                } else {
                    String::new()
                };
                store.message_append(&Message { id, node_id: node.id, project_id: Some(d.project_id), role: MessageRole::Node, text: format!("【{label}・自動引き渡し結果】{title}\n{}\n[タスクと部署レビュー](/tasks/{}?tab=changes){}{}", d.detail, d.task_id, release, push_line), run_id: None, task_id: Some(d.task_id), metadata: None, created_at: now })?;
            }
            d.notification = Some(id);
            store.delivery_save(Some(old), &d)?;
        }
    }
    Ok(())
}
fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
fn preparation_dir(config: &Config, d: &Delivery) -> std::path::PathBuf {
    config
        .selfdeploy
        .releases_dir
        .join(".deliveries")
        .join(d.task_id.to_string())
        .join(&d.head)
}
fn start_prepare(store: &dyn TaskStore, config: &Config, old: &Delivery) -> Result<(), StoreError> {
    let dir = preparation_dir(config, old);
    let spawn = || -> std::io::Result<std::process::Child> {
        std::fs::create_dir_all(&dir)?;
        let log = std::fs::File::create(dir.join("prepare.log"))?;
        let mut cmd = Command::new("bash");
        cmd.arg(
            config
                .selfdeploy
                .releases_dir
                .parent()
                .unwrap_or(Path::new("."))
                .join("current/scripts/prepare.sh"),
        )
        .arg(&old.head)
        .arg(&dir)
        .env("SD_REPO", &config.selfdeploy.repo)
        .env(
            "CELERIS_STATE_DIR",
            config
                .selfdeploy
                .releases_dir
                .parent()
                .unwrap_or(Path::new(".")),
        )
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
        if let Some(path) = &config.source_path {
            cmd.env("CELERIS_CONFIG", path);
        }
        cmd.spawn()
    };
    let mut next = old.clone();
    match spawn() {
        Ok(mut child) => {
            next.prepare_pid = Some(child.id());
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => {
            next.state = State::Blocked;
            next.detail = format!("リリース準備を起動できません: {e}");
        }
    }
    store.delivery_save(Some(old), &next)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use task_core::{ProjectId, RepoId, TaskId};

    fn record() -> Delivery {
        Delivery {
            task_id: TaskId::new(),
            project_id: ProjectId::new(),
            repo_id: RepoId::new(),
            repo: "test".into(),
            branch: "feature".into(),
            base: String::new(),
            head: String::new(),
            default_branch: "main".into(),
            department: "engineering".into(),
            review_run: "review-run".into(),
            worker_run: "worker-run".into(),
            criterion_idx: 0,
            decision: Some(true),
            state: State::Reviewing,
            detail: String::new(),
            release: None,
            prepare_pid: None,
            notification: None,
            pushed_at: None,
            push_error: None,
        }
    }
    fn repository() -> (tempfile::TempDir, Delivery) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git_text(p, &["init", "-b", "main"]).unwrap();
        git_text(p, &["config", "user.email", "test@example.invalid"]).unwrap();
        git_text(p, &["config", "user.name", "test"]).unwrap();
        fs::write(p.join("base"), "base").unwrap();
        fs::write(p.join("user-file"), "keep").unwrap();
        git_text(p, &["add", "."]).unwrap();
        git_text(p, &["commit", "-m", "base"]).unwrap();
        let mut d = record();
        d.base = sha(p, "HEAD").unwrap();
        git_text(p, &["checkout", "-b", "feature"]).unwrap();
        fs::write(p.join("feature"), "implemented").unwrap();
        git_text(p, &["add", "."]).unwrap();
        git_text(p, &["commit", "-m", "feature"]).unwrap();
        d.head = sha(p, "HEAD").unwrap();
        git_text(p, &["checkout", "main"]).unwrap();
        (dir, d)
    }
    #[test]
    fn reviewed_merge_preserves_unrelated_user_deletion_and_branch() {
        let (dir, d) = repository();
        fs::remove_file(dir.path().join("user-file")).unwrap();
        merge_reviewed(dir.path(), &d).unwrap();
        assert_eq!(sha(dir.path(), "HEAD").unwrap(), d.head);
        assert!(!dir.path().join("user-file").exists());
        assert_eq!(sha(dir.path(), "feature").unwrap(), d.head);
    }
    #[test]
    fn new_commits_or_moved_base_cannot_reuse_approval() {
        let (dir, d) = repository();
        let p = dir.path();
        git_text(p, &["checkout", "feature"]).unwrap();
        git_text(p, &["commit", "--allow-empty", "-m", "unreviewed"]).unwrap();
        git_text(p, &["checkout", "main"]).unwrap();
        assert!(merge_reviewed(p, &d).is_err());
        assert_eq!(sha(p, "HEAD").unwrap(), d.base);
        let (dir, d) = repository();
        let p = dir.path();
        git_text(p, &["commit", "--allow-empty", "-m", "main moved"]).unwrap();
        let moved = sha(p, "HEAD").unwrap();
        assert!(merge_reviewed(p, &d).is_err());
        assert_eq!(sha(p, "HEAD").unwrap(), moved);
    }
    #[test]
    fn overlapping_untracked_files_block_merge_without_loss() {
        let (dir, d) = repository();
        fs::write(dir.path().join("feature"), "human draft").unwrap();
        assert!(merge_reviewed(dir.path(), &d).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("feature")).unwrap(),
            "human draft"
        );
        assert_eq!(sha(dir.path(), "HEAD").unwrap(), d.base);
    }
    fn stored_delivery() -> (task_core::SqliteStore, Delivery) {
        use task_core::*;
        let store = SqliteStore::open_in_memory().unwrap();
        let now = OffsetDateTime::now_utc();
        for (id, parent, kind) in [
            ("cos", None, OrgKind::Secretary),
            ("engineering", Some("cos"), OrgKind::Department),
        ] {
            store
                .org_upsert(&OrgNode {
                    id: id.into(),
                    parent_id: parent.map(str::to_string),
                    name: id.into(),
                    kind,
                    genre: None,
                    brief: String::new(),
                    profile: Default::default(),
                    position: 0,
                    created_at: now,
                    updated_at: now,
                })
                .unwrap();
        }
        let p = Project {
            id: ProjectId::new(),
            title: "test".into(),
            request: "fix".into(),
            status: ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            archived_at: None,
            paused_from: None,
            created_at: now,
            updated_at: now,
        };
        store.project_create(&p).unwrap();
        let spec:task_ops::add::NewTaskSpec=serde_json::from_value(serde_json::json!({"title":"fix","objective":"implement","acceptance":[],"project_id":p.id,"assignee":"engineering"})).unwrap();
        let task = task_ops::add::create_support_task(&store, spec, &[], &[], now).unwrap();
        for trigger in [Trigger::Dispatch, Trigger::WorkerDone, Trigger::ReviewPass] {
            store.apply_transition(task.id, trigger, None).unwrap();
        }
        let mut d = record();
        d.task_id = task.id;
        d.project_id = p.id;
        (store, d)
    }
    #[test]
    fn delivery_failure_classification_is_limited_to_merge_base_and_format() {
        assert_eq!(
            classify_delivery_failure(State::Blocked, "[merge-base] git failed", None),
            Some(RepairClass::MergeBase)
        );
        assert_eq!(
            classify_delivery_failure(
                State::Blocked,
                "リリース準備に失敗しました",
                Some("cargo-fmt-check")
            ),
            Some(RepairClass::Format)
        );
        assert_eq!(
            classify_delivery_failure(
                State::Blocked,
                "リリース準備に失敗しました",
                Some("cargo-test")
            ),
            None
        );
        assert_eq!(
            classify_delivery_failure(
                State::Blocked,
                "リリース準備に失敗しました",
                Some("pnpm-e2e-mock")
            ),
            None
        );
        assert_eq!(
            classify_delivery_failure(State::Ready, "[merge-base] git failed", None),
            None
        );
    }
    #[test]
    fn preparation_requires_all_gates_and_notifies_cos_only_when_ready() {
        use task_core::DeliveryStore;
        let (store, mut d) = stored_delivery();
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = toml::from_str("").unwrap();
        cfg.selfdeploy.releases_dir = tmp.path().join("releases");
        d.head = "b".repeat(40);
        d.release = Some("b".repeat(12));
        d.state = State::Preparing;
        let dir = preparation_dir(&cfg, &d);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("result.json"), r#"{"ok":true}"#).unwrap();
        let rel = cfg
            .selfdeploy
            .releases_dir
            .join(d.release.as_ref().unwrap());
        fs::create_dir_all(&rel).unwrap();
        fs::write(rel.join("gate.json"), r#"{"ok":true}"#).unwrap();
        fs::write(rel.join("verify.json"), r#"{"ok":true}"#).unwrap();
        fs::write(rel.join("manifest.json"), r#"{"sha":"wrong"}"#).unwrap();
        store.delivery_save(None, &d).unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let blocked = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(blocked.state, State::Blocked);
        store.delivery_save(Some(&blocked), &d).unwrap();
        fs::write(
            rel.join("manifest.json"),
            serde_json::json!({"sha":d.head}).to_string(),
        )
        .unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let ready = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(ready.state, State::Ready);
        advance(&store, &cfg, &ready, OffsetDateTime::now_utc()).unwrap();
        let notified = store.delivery_get(d.task_id).unwrap().unwrap();
        advance(&store, &cfg, &notified, OffsetDateTime::now_utc()).unwrap();
        assert_eq!(
            store
                .message_list("cos", Some(d.project_id), 20)
                .unwrap()
                .len(),
            1
        );
        assert!(!tmp.path().join("current").exists());
    }
    #[test]
    fn other_gate_failure_keeps_legacy_reopen_once() {
        use task_core::{DeliveryStore, Trigger};
        let (store, mut d) = stored_delivery();
        let cfg: Config = toml::from_str("").unwrap();
        d.state = State::Blocked;
        d.detail = "ビルドの検査失敗".into();
        store.delivery_save(None, &d).unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        assert_eq!(store.get(d.task_id).unwrap().unwrap().status, Status::Ready);
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        assert_eq!(store.comments_for(d.task_id).unwrap().len(), 1);
        for trigger in [Trigger::Dispatch, Trigger::WorkerDone, Trigger::ReviewPass] {
            store.apply_transition(d.task_id, trigger, None).unwrap();
        }
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        assert_eq!(store.get(d.task_id).unwrap().unwrap().status, Status::Done);
        assert_eq!(
            store
                .message_list("engineering", Some(d.project_id), 20)
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .message_list("cos", Some(d.project_id), 20)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn technical_failure_is_repaired_once_with_no_cos_review_or_message() {
        use task_core::DeliveryStore;
        let (store, mut d) = stored_delivery();
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = cfg_for(tmp.path(), &tmp.path().join("releases"));
        cfg.execution.max_repairs_per_class = 1;
        d.state = State::Blocked;
        d.detail = "リリース準備に失敗しました。ログ: prepare.log".into();
        d.head = "a".repeat(40);
        d.release = Some("a".repeat(12));
        let rel = cfg
            .selfdeploy
            .releases_dir
            .join(d.release.as_deref().unwrap());
        fs::create_dir_all(&rel).unwrap();
        fs::write(
            rel.join("gate.json"),
            r#"{"ok":false,"failed_step":"cargo-fmt-check"}"#,
        )
        .unwrap();
        fs::write(
            rel.join(".gate-cargo-fmt-check.log"),
            "Diff in src/main.rs:1",
        )
        .unwrap();
        store.delivery_save(None, &d).unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        assert_eq!(store.get(d.task_id).unwrap().unwrap().status, Status::Ready);
        let units = store.work_units_for(d.task_id).unwrap();
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].status, WorkUnitStatus::Done);
        assert_eq!(units[1].status, WorkUnitStatus::Ready);
        assert!(units[1].spec.title.starts_with("repair (format):"));
        assert!(units[1].spec.objective.contains("Diff in src/main.rs:1"));
        assert!(!units[1].spec.objective.contains("implement"));
        assert_eq!(
            task_core::next_work_unit(&units),
            task_core::NextStep::RunWorkUnit(units[1].id.clone())
        );
        assert!(
            store
                .comments_for(d.task_id)
                .unwrap()
                .iter()
                .any(|c| c.body.contains("repair-1 class=format"))
        );

        // 同じ class の上限に達した再失敗は人へ戻す。
        for trigger in [
            task_core::Trigger::Dispatch,
            task_core::Trigger::WorkerDone,
            task_core::Trigger::ReviewPass,
        ] {
            store.apply_transition(d.task_id, trigger, None).unwrap();
        }
        let mut completed = units[1].clone();
        completed.status = WorkUnitStatus::Done;
        store
            .work_unit_transition(
                d.task_id,
                completed,
                Event::WorkUnitTransitioned {
                    work_unit_id: units[1].id.clone(),
                    key: units[1].key.clone(),
                    from: WorkUnitStatus::Ready,
                    to: WorkUnitStatus::Done,
                    reason: "test".into(),
                    run_id: None,
                },
            )
            .unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let requeued = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(requeued.state, State::MergeQueued);
        let mut failed_again = requeued.clone();
        failed_again.state = State::Blocked;
        failed_again.detail = d.detail.clone();
        failed_again.release = d.release.clone();
        store.delivery_save(Some(&requeued), &failed_again).unwrap();
        advance(&store, &cfg, &failed_again, OffsetDateTime::now_utc()).unwrap();
        assert!(
            store
                .delivery_get(d.task_id)
                .unwrap()
                .unwrap()
                .detail
                .starts_with("[needs-human]")
        );
    }

    #[test]
    fn merge_base_repair_revalidates_and_delivers_only_the_repaired_branch() {
        use task_core::{DeliveryStore, Trigger};
        let (store, dir, d) = merge_queued_delivery();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(dir.path(), &tmp.path().join("releases"));
        // The previous run may have a large transcript; it must not enter the repair objective.
        let transcript_marker = "ORIGINAL_RUN_TRANSCRIPT_MARKER";
        store
            .append_event(
                d.task_id,
                &Event::worker_progress("original-run", transcript_marker),
            )
            .unwrap();
        git_text(dir.path(), &["commit", "--allow-empty", "-m", "main moved"]).unwrap();
        let moved_main = sha(dir.path(), "main").unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let blocked = store.delivery_get(d.task_id).unwrap().unwrap();
        assert!(blocked.detail.starts_with(MERGE_BASE_FAILURE));
        advance(&store, &cfg, &blocked, OffsetDateTime::now_utc()).unwrap();
        assert_eq!(store.get(d.task_id).unwrap().unwrap().status, Status::Ready);
        let units = store.work_units_for(d.task_id).unwrap();
        assert_eq!(units.len(), 2);
        let repair = &units[1];
        assert_eq!(repair.kind, WorkUnitKind::Repair);
        assert_eq!(repair.status, WorkUnitStatus::Ready);
        assert!(repair.spec.title.starts_with("repair (merge_base):"));
        assert_eq!(
            store
                .execution_plan_active(d.task_id)
                .unwrap()
                .unwrap()
                .status,
            task_core::PlanStatus::Active
        );
        assert!(repair.spec.objective.contains(&d.branch));
        assert!(repair.spec.objective.contains(&moved_main));
        assert!(repair.spec.objective.contains(&blocked.detail));
        assert!(!repair.spec.objective.contains(transcript_marker));
        assert!(!repair.spec.objective.contains("implement"));
        assert!(!repair.spec.objective.contains("work_units"));

        // Simulate the repair worker rebasing feature onto the new main and passing review.
        git_text(dir.path(), &["checkout", "feature"]).unwrap();
        git_text(dir.path(), &["rebase", "main"]).unwrap();
        let repaired_head = sha(dir.path(), "feature").unwrap();
        git_text(dir.path(), &["checkout", "main"]).unwrap();
        let mut done = repair.clone();
        done.status = WorkUnitStatus::Done;
        store
            .work_unit_transition(
                d.task_id,
                done,
                Event::WorkUnitTransitioned {
                    work_unit_id: repair.id.clone(),
                    key: repair.key.clone(),
                    from: WorkUnitStatus::Ready,
                    to: WorkUnitStatus::Done,
                    reason: "test_repair_completed".into(),
                    run_id: None,
                },
            )
            .unwrap();
        for trigger in [Trigger::Dispatch, Trigger::WorkerDone, Trigger::ReviewPass] {
            store.apply_transition(d.task_id, trigger, None).unwrap();
        }
        let now = OffsetDateTime::now_utc();
        advance(&store, &cfg, &blocked, now).unwrap();
        let requeued = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(requeued.state, State::MergeQueued);
        assert_eq!(requeued.base, moved_main);
        assert_eq!(requeued.head, repaired_head);
        // A local fake prepare script lets the merge proceed without touching a real release.
        let script = cfg
            .selfdeploy
            .releases_dir
            .parent()
            .unwrap()
            .join("current/scripts/prepare.sh");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        advance(&store, &cfg, &requeued, now).unwrap();
        let delivered = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(delivered.state, State::Preparing);
        assert_eq!(sha(dir.path(), "main").unwrap(), repaired_head);
        assert_eq!(delivered.release.as_deref(), Some(&repaired_head[..12]));
    }

    // ---- ADR-0051 Phase 106追記: merge直後・release前のpush。ここから ----

    fn bare_remote() -> tempfile::TempDir {
        let bare = tempfile::tempdir().unwrap();
        git_text(bare.path(), &["init", "--bare", "-b", "main"]).unwrap();
        bare
    }

    #[test]
    fn push_merged_pushes_new_commits_to_a_bare_remote() {
        let (dir, d) = repository();
        let p = dir.path();
        merge_reviewed(p, &d).unwrap();
        let bare = bare_remote();
        git_text(
            p,
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        )
        .unwrap();
        push_merged(p, "origin", "main", Duration::from_secs(20)).unwrap();
        assert_eq!(sha(bare.path(), "refs/heads/main").unwrap(), d.head);
    }

    #[test]
    fn push_merged_reports_failure_reason_when_remote_is_unreachable() {
        let (dir, d) = repository();
        let p = dir.path();
        merge_reviewed(p, &d).unwrap();
        git_text(p, &["remote", "add", "origin", "/no/such/path-phase106"]).unwrap();
        let err = push_merged(p, "origin", "main", Duration::from_secs(20)).unwrap_err();
        assert!(!err.trim().is_empty());
    }

    #[test]
    fn push_merged_skips_when_remote_is_already_up_to_date() {
        let (dir, d) = repository();
        let p = dir.path();
        merge_reviewed(p, &d).unwrap();
        // 実際には壊れたリモートだが、追跡refだけを直接作って「既に同じ」を再現する
        // （ネットワークにもリモートにも触れずに済む）。
        git_text(p, &["remote", "add", "origin", "/no/such/path-phase106"]).unwrap();
        git_text(p, &["update-ref", "refs/remotes/origin/main", &d.head]).unwrap();
        push_merged(p, "origin", "main", Duration::from_secs(5)).unwrap();
    }

    /// `stored_delivery()`（task がDone・cos/engineeringの組織）と `repository()`（git リポジトリ）を
    /// 組み合わせ、`State::MergeQueued` から `advance` を通す。
    fn merge_queued_delivery() -> (task_core::SqliteStore, tempfile::TempDir, Delivery) {
        use task_core::DeliveryStore;
        let (store, mut d) = stored_delivery();
        let (dir, repo_d) = repository();
        d.base = repo_d.base;
        d.head = repo_d.head;
        d.branch = repo_d.branch;
        d.default_branch = repo_d.default_branch;
        d.state = State::MergeQueued;
        store.delivery_save(None, &d).unwrap();
        (store, dir, d)
    }

    fn cfg_for(repo: &Path, releases_dir: &Path) -> Config {
        let mut cfg: Config = toml::from_str("").unwrap();
        cfg.selfdeploy.repo = repo.to_path_buf();
        cfg.selfdeploy.releases_dir = releases_dir.to_path_buf();
        cfg
    }

    #[test]
    fn advance_pushes_to_origin_right_after_merge_before_release_prep() {
        use task_core::DeliveryStore;
        let (store, dir, d) = merge_queued_delivery();
        let bare = bare_remote();
        git_text(
            dir.path(),
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(dir.path(), &tmp.path().join("releases"));
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let after = store.delivery_get(d.task_id).unwrap().unwrap();
        assert!(after.pushed_at.is_some());
        assert!(after.push_error.is_none());
        assert_eq!(after.state, State::Preparing);
        assert!(after.prepare_pid.is_some());
        assert_eq!(sha(bare.path(), "refs/heads/main").unwrap(), after.head);
    }

    #[test]
    fn push_failure_after_merge_does_not_block_release_prep_and_retries_once() {
        use task_core::DeliveryStore;
        let (store, dir, d) = merge_queued_delivery();
        git_text(
            dir.path(),
            &["remote", "add", "origin", "/no/such/path-phase106-retry"],
        )
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_for(dir.path(), &tmp.path().join("releases"));

        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let after_merge = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(after_merge.state, State::Preparing, "release準備は進む");
        assert!(after_merge.pushed_at.is_none());
        let first_err = after_merge
            .push_error
            .clone()
            .expect("push は失敗しているはず");
        assert!(!first_err.starts_with(PUSH_RETRIED_PREFIX));

        // 次のtickで1度だけ再試行。
        advance(&store, &cfg, &after_merge, OffsetDateTime::now_utc()).unwrap();
        let after_retry = store.delivery_get(d.task_id).unwrap().unwrap();
        assert!(after_retry.pushed_at.is_none());
        let retried_err = after_retry
            .push_error
            .clone()
            .expect("再試行後も失敗しているはず");
        assert!(retried_err.starts_with(PUSH_RETRIED_PREFIX));

        // それでも失敗したら以後は触らない: もう一度 advance しても push_error は変わらない。
        advance(&store, &cfg, &after_retry, OffsetDateTime::now_utc()).unwrap();
        let after_third = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(after_third.push_error, after_retry.push_error);
        assert!(after_third.pushed_at.is_none());
    }

    #[test]
    fn maybe_retry_push_noop_when_push_disabled() {
        let store = task_core::SqliteStore::open_in_memory().unwrap();
        let mut cfg: Config = toml::from_str("").unwrap();
        cfg.selfdeploy.push = false;
        let mut d = record();
        d.release = Some("a".repeat(12));
        d.push_error = Some("boom".into());
        let out = maybe_retry_push(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        assert_eq!(out, d, "push=falseなら何もしない");
    }

    /// Preparing → Ready の通知文に、pushの結果を1行足す（成功）。
    #[test]
    fn ready_notification_reports_successful_push() {
        use task_core::DeliveryStore;
        let (store, mut d) = stored_delivery();
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = toml::from_str("").unwrap();
        cfg.selfdeploy.releases_dir = tmp.path().join("releases");
        d.head = "c".repeat(40);
        d.release = Some("c".repeat(12));
        d.state = State::Preparing;
        d.pushed_at = Some(OffsetDateTime::now_utc());
        let dir = preparation_dir(&cfg, &d);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("result.json"), r#"{"ok":true}"#).unwrap();
        let rel = cfg
            .selfdeploy
            .releases_dir
            .join(d.release.as_ref().unwrap());
        fs::create_dir_all(&rel).unwrap();
        fs::write(rel.join("gate.json"), r#"{"ok":true}"#).unwrap();
        fs::write(rel.join("verify.json"), r#"{"ok":true}"#).unwrap();
        fs::write(
            rel.join("manifest.json"),
            serde_json::json!({"sha": d.head}).to_string(),
        )
        .unwrap();
        store.delivery_save(None, &d).unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let ready = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(ready.state, State::Ready);
        advance(&store, &cfg, &ready, OffsetDateTime::now_utc()).unwrap();
        let messages = store.message_list("cos", Some(d.project_id), 20).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].text.contains("origin へ push 済み"));
        assert!(messages[0].text.contains(&d.head));
    }

    /// Preparing → Ready の通知文に、pushの結果を1行足す（失敗。`[retried]` の目印は見せない）。
    #[test]
    fn ready_notification_reports_push_failure_without_the_retry_marker() {
        use task_core::DeliveryStore;
        let (store, mut d) = stored_delivery();
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = toml::from_str("").unwrap();
        cfg.selfdeploy.releases_dir = tmp.path().join("releases");
        d.head = "d".repeat(40);
        d.release = Some("d".repeat(12));
        d.state = State::Preparing;
        d.push_error = Some(format!("{PUSH_RETRIED_PREFIX}fatal: repository not found"));
        let dir = preparation_dir(&cfg, &d);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("result.json"), r#"{"ok":true}"#).unwrap();
        let rel = cfg
            .selfdeploy
            .releases_dir
            .join(d.release.as_ref().unwrap());
        fs::create_dir_all(&rel).unwrap();
        fs::write(rel.join("gate.json"), r#"{"ok":true}"#).unwrap();
        fs::write(rel.join("verify.json"), r#"{"ok":true}"#).unwrap();
        fs::write(
            rel.join("manifest.json"),
            serde_json::json!({"sha": d.head}).to_string(),
        )
        .unwrap();
        store.delivery_save(None, &d).unwrap();
        advance(&store, &cfg, &d, OffsetDateTime::now_utc()).unwrap();
        let ready = store.delivery_get(d.task_id).unwrap().unwrap();
        assert_eq!(ready.state, State::Ready);
        advance(&store, &cfg, &ready, OffsetDateTime::now_utc()).unwrap();
        let messages = store.message_list("cos", Some(d.project_id), 20).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].text.contains("push に失敗"));
        assert!(messages[0].text.contains("fatal: repository not found"));
        assert!(!messages[0].text.contains(PUSH_RETRIED_PREFIX));
    }
    // ---- ADR-0051 Phase 106追記: ここまで ----
}
