//! ADR-0051: 部署レビュアーのマージ可否判定 → merge → release/verify。
//! 判断は既存の独立したReviewer run。ここは永続状態を決定的に進めるだけで promote は呼ばない。
use crate::config::Config;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use task_core::{
    Delivery, DeliveryState as State, Message, MessageId, MessageRole, Status, StoreError,
    TaskStore,
};
use task_ops::changes::git;
use time::OffsetDateTime;

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
    if git_text(repo, &["merge-base", "--is-ancestor", &d.base, &d.head]).is_err() {
        return Err(
            "既定ブランチの更新を取り込んでから再レビューしてください（自動rebaseは行いません）"
                .into(),
        );
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

fn advance(
    store: &dyn TaskStore,
    config: &Config,
    old: &Delivery,
    now: OffsetDateTime,
) -> Result<(), StoreError> {
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
            d.detail =
                "実装の作業ツリーに未コミットの変更があります。コミット後に再レビューしてください"
                    .into();
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
                }
                Err(e) => {
                    d.state = State::Blocked;
                    d.detail = e;
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
                store.message_append(&Message { id, node_id: node.id, project_id: Some(d.project_id), role: MessageRole::Node, text: format!("【{label}・自動引き渡し結果】{title}\n{}\n[タスクと部署レビュー](/tasks/{}?tab=changes){}", d.detail, d.task_id, release), run_id: None, task_id: Some(d.task_id), metadata: None, created_at: now })?;
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
    fn technical_failure_is_repaired_once_with_no_cos_review_or_message() {
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
}
