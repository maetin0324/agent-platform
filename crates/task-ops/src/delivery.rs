//! ADR-0051: 既存レビュアーの対象SHAを固定し、同じrunに取り込み条件を追加する。
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use task_core::{Check, Criterion, Delivery, DeliveryState, StoreError, Task, TaskStore};

#[derive(Debug, Clone, Default)]
pub struct DeliveryPolicy {
    pub projects: Vec<String>,
    pub repo: PathBuf,
}

pub fn begin(
    store: &dyn TaskStore,
    task: &mut Task,
    workspace_root: &Path,
    policy: &DeliveryPolicy,
    worker_run: &str,
    review_run: &str,
) -> Result<Option<Delivery>, StoreError> {
    let Some(project_id) = task.project_id else {
        return Ok(None);
    };
    if !policy.projects.contains(&project_id.to_string())
        || task_core::support_kind(task).is_some()
        || task.repos.len() != 1
    {
        return Ok(None);
    }
    let org = store.org_list()?;
    let Some(department) = task
        .assignee
        .as_deref()
        .and_then(|id| task_core::department_of(&org, id))
    else {
        return Ok(None);
    };
    let Some(marker) = crate::workspace::read_marker(&workspace_root.join(task.id.to_string()))
    else {
        return Ok(None);
    };
    if marker.repos.len() != 1 {
        return Ok(None);
    }
    let mr = &marker.repos[0];
    let Some(row) = store
        .repo_list(project_id)?
        .into_iter()
        .find(|r| r.id == task.repos[0].repo_id)
    else {
        return Ok(None);
    };
    let task_core::WorkspaceSpec::Local { path, .. } = &row.location else {
        return Ok(None);
    };
    if !path.is_dir()
        || path.canonicalize().ok() != policy.repo.canonicalize().ok()
        || Path::new(&mr.source).canonicalize().ok() != path.canonicalize().ok()
        || mr.kind != "git"
    {
        return Ok(None);
    }
    let Some(branch) = &mr.branch else {
        return Ok(None);
    };
    if !branch.ends_with(&task.id.to_string()) {
        return Ok(None);
    }
    let default_branch = crate::changes::default_branch(path, row.default_branch.as_deref());
    let resolve = |r: &str| {
        crate::changes::git(path, &["rev-parse", "--verify", r], Duration::from_secs(10))
            .filter(|o| o.ok)
            .map(|o| o.stdout.trim().to_string())
    };
    let (Some(base), Some(head)) = (
        resolve(&format!("refs/heads/{default_branch}")),
        resolve(&format!("refs/heads/{branch}")),
    ) else {
        return Ok(None);
    };
    let old = store.delivery_get(task.id)?;
    if old.as_ref().is_some_and(|d| {
        matches!(d.state, DeliveryState::Merging | DeliveryState::Preparing)
            || (d.state == DeliveryState::Ready && d.head == head)
    }) {
        return Ok(None);
    }
    let criterion_idx = task.acceptance.len();
    task.acceptance.push(Criterion {check:Check::Reviewer,text:format!("【部署内のマージ可否判定】リポジトリ {} の {} ({}) に、コミット {} (ブランチ {}) を取り込めること。指定SHAの実際の差分、依頼範囲、テスト、移行・設定の互換性、秘密の混入を確認し、対象の実装worktreeに未コミット変更や未解決の欠陥があれば不合格にする。登録元リポジトリにはユーザーの未コミット変更があり得る。対象差分と重ならずfast-forwardで保持できる既存変更は拒否理由にせず、復元・コミット・stashしない。対象差分と衝突する変更は停止理由にする。レビューは部署をまとめる担当 {} がこの独立runで行い、上司やCoSで重複レビューしない。合格すると制御プレーンがマージ・release・verifyまで実行し、最終デプロイだけ人が押す。要件や権限などユーザーに確認しないと判断できない場合は、理由の先頭に [needs-human] と具体的な質問を含める。技術的な不備だけなら部署内で差し戻す。自分でmergeやdeploy、実装変更はしない。",path.display(),default_branch,base,head,branch,department) });
    let d = Delivery {
        task_id: task.id,
        project_id,
        repo_id: row.id,
        repo: row.name,
        branch: branch.clone(),
        base,
        head,
        default_branch,
        department,
        review_run: review_run.into(),
        worker_run: worker_run.into(),
        criterion_idx,
        decision: None,
        state: DeliveryState::Reviewing,
        detail: "部署のレビュアーが実装とマージ可否を確認中です".into(),
        release: None,
        prepare_pid: None,
        notification: None,
    };
    if !store.delivery_save(old.as_ref(), &d)? {
        return Err(StoreError::Invalid(
            "delivery review changed concurrently".into(),
        ));
    }
    Ok(Some(d))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use task_core::*;
    #[test]
    fn existing_reviewer_gets_a_pinned_merge_criterion_without_extra_tasks() {
        let store = SqliteStore::open_in_memory().unwrap();
        let now = time::OffsetDateTime::now_utc();
        for (id, parent, kind) in [
            ("cos", None, OrgKind::Secretary),
            ("engineering", Some("cos"), OrgKind::Department),
            ("worker", Some("engineering"), OrgKind::Section),
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
        let project = Project {
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
        store.project_create(&project).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("repo");
        fs::create_dir(&p).unwrap();
        let git = |args: &[&str]| {
            let out = crate::changes::git(&p, args, Duration::from_secs(10)).unwrap();
            assert!(out.ok, "{}", out.stderr);
            out.stdout.trim().to_string()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "test"]);
        git(&["commit", "--allow-empty", "-m", "base"]);
        let row = ProjectRepo {
            id: RepoId::new(),
            project_id: project.id,
            name: "repo".into(),
            kind: RepoKind::Git,
            location: WorkspaceSpec::Local {
                path: p.clone(),
                mode: None,
            },
            default_branch: Some("main".into()),
            sync: None,
            run: Default::default(),
            is_primary: true,
            created_at: now,
        };
        store.repo_create(&row).unwrap();
        let spec:crate::add::NewTaskSpec=serde_json::from_value(serde_json::json!({"title":"fix","objective":"implement","acceptance":[{"type":"reviewer","text":"works"}],"project_id":project.id,"assignee":"worker","repos":["repo"]})).unwrap();
        let mut task = crate::add::create_task_with_roles(&store, spec, &[], &[], now).unwrap();
        let branch = format!("celeris/{}", task.id);
        git(&["checkout", "-b", &branch]);
        git(&["commit", "--allow-empty", "-m", "implemented"]);
        let head = git(&["rev-parse", "HEAD"]);
        let root = dir.path().join("tasks");
        let taskdir = root.join(task.id.to_string());
        fs::create_dir_all(&taskdir).unwrap();
        fs::write(taskdir.join("worktree.json"),serde_json::json!({"repo":p,"dir":p,"branch":branch,"base":"","base_kind":"main","repos":[{"name":"repo","kind":"git","source":p,"dir":p,"branch":branch}]}).to_string()).unwrap();
        let policy = DeliveryPolicy {
            projects: vec![project.id.to_string()],
            repo: p,
        };
        let d = begin(
            &store,
            &mut task,
            &root,
            &policy,
            "worker-run",
            "review-run",
        )
        .unwrap()
        .unwrap();
        assert_eq!(d.head, head);
        assert_eq!(d.department, "engineering");
        assert_eq!(d.review_run, "review-run");
        assert_eq!(d.criterion_idx, 1);
        assert_eq!(task.acceptance.len(), 2);
        assert!(task.acceptance[1].text.contains(&head));
        assert_eq!(
            store
                .list_page(&ListFilter::default(), ListOrder::CreatedDesc, None, 20)
                .unwrap()
                .items
                .len(),
            1,
            "no extra supervisor or CoS tasks"
        );
        assert!(
            store
                .message_list("cos", Some(project.id), 20)
                .unwrap()
                .is_empty()
        );
        let mut next = d.clone();
        next.state = DeliveryState::MergeQueued;
        assert!(store.delivery_save(Some(&d), &next).unwrap());
        assert!(
            !store.delivery_save(Some(&d), &d).unwrap(),
            "stale scheduler cannot erase review"
        );
        let mut unrelated = task.clone();
        unrelated.project_id = None;
        assert!(
            begin(&store, &mut unrelated, &root, &policy, "w", "r")
                .unwrap()
                .is_none()
        );
    }
}
