//! ADR-0043 D5（Phase 54）: 変更の取り込み（`/tasks/{id}/changes`、`/changes/{repo}/diff`、
//! `/changes/{repo}/integrate`、`/changes/{repo}/pr/merge`、`/projects/{id}/integrations`）。
//!
//! 見るもの: 差分の一覧と 1 ファイルの diff、`merge`（fast-forward / 409 / 衝突 → 子タスク）、
//! `discard`（確認必須）、`pr`（**偽の `gh`** とローカルの bare `origin`。外に出ない）、
//! 401 / 404 / 409 / 422、案件の一覧。
//!
//! **外部ネットワークには出ない**（CLAUDE.md）。`origin` は同じ tempdir の bare リポジトリで、
//! `gh` は tempdir に書いた shell スクリプト（PATH ではなく `[github] gh` で指す）。

mod common;

use common::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use task_core::{Status, TaskId, TaskKind};

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(
        path,
        body,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

/// `git -C <dir> <args...>`（テストの下ごしらえ専用）。
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// **偽の `gh`**（外に出ない）。`auth status` は常に成功、`pr create` は固定の URL を返し、
/// `pr view` は `<dir>/state` の中身を状態として返し、`pr merge` は `<dir>/merged.log` に書いて
/// 状態を `MERGED` にする。呼ばれた引数は `<dir>/calls.log` に残る。
fn fake_gh(dir: &Path) -> String {
    std::fs::create_dir_all(dir).expect("mkdir");
    let path = dir.join("gh");
    let script = r#"#!/bin/sh
here="$(cd "$(dirname "$0")" && pwd)"
echo "$@" >> "$here/calls.log"
case "$1 $2" in
  "auth status") exit 0 ;;
  "pr create")
    echo "https://github.com/o/r/pull/42"
    exit 0 ;;
  "pr view")
    state="$(cat "$here/state" 2>/dev/null || echo OPEN)"
    if [ "$state" = "MERGED" ]; then
      echo '{"state":"MERGED","mergedAt":"2026-09-19T10:00:00Z","mergeable":"MERGEABLE","reviewDecision":null,"url":"https://github.com/o/r/pull/42"}'
    else
      echo '{"state":"OPEN","mergedAt":null,"mergeable":"MERGEABLE","reviewDecision":null,"url":"https://github.com/o/r/pull/42"}'
    fi
    exit 0 ;;
  "pr merge")
    echo "$@" >> "$here/merged.log"
    echo MERGED > "$here/state"
    exit 0 ;;
esac
echo "fake gh: unsupported $*" 1>&2
exit 1
"#;
    std::fs::write(&path, script).expect("write gh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path.to_string_lossy().into_owned()
}

/// 偽の `gh` を使う環境（`gh auth status` はいつも成功するので、`gh` の有無の記憶が
/// テストの順番に左右されない）。
fn env_with_gh() -> (TestEnv, PathBuf) {
    let bin = tempfile::tempdir().expect("tempdir");
    let gh = fake_gh(bin.path());
    let dir = bin.keep();
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        github: task_api::GithubSettings {
            gh,
            merge_method: "merge".into(),
        },
        ..EnvOptions::default()
    });
    task_ops::changes::forget_gh_auth();
    (env, dir)
}

/// git のリポジトリ（`main` に 1 コミット）と、そのタスクの worktree（ブランチに 1 コミット）と
/// 目印（`worktree.json`）を用意する。
fn seed_git_task(
    env: &TestEnv,
    project_id: Option<task_core::ProjectId>,
) -> (TaskId, PathBuf, PathBuf) {
    let mut task = new_task(TaskKind::Execute, Status::Done);
    task.project_id = project_id;
    env.seed(&task);
    let repo = env.dir.path().join(format!("src-{}", task.id));
    std::fs::create_dir_all(&repo).expect("mkdir");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("README.md"), b"hello\n").expect("write");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);

    let task_dir = env.workspace(&task);
    let tree = task_dir.join("repos").join("code");
    let branch = format!("celeris/{}", task.id);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &tree.to_string_lossy(),
            "main",
        ],
    );
    git(&tree, &["config", "user.email", "t@example.com"]);
    git(&tree, &["config", "user.name", "t"]);
    std::fs::write(tree.join("src.txt"), b"a\nb\n").expect("write");
    git(&tree, &["add", "-A"]);
    git(&tree, &["commit", "-q", "-m", "work"]);

    let base = git_out(&repo, &["rev-parse", "refs/heads/main"]);
    let marker = task_ops::workspace::WorktreeMarker {
        repo: repo.to_string_lossy().into_owned(),
        dir: tree.to_string_lossy().into_owned(),
        branch: branch.clone(),
        base: base.clone(),
        base_kind: "main".into(),
        repos: vec![
            task_ops::workspace::WorktreeMarkerRepo {
                name: "code".into(),
                kind: "git".into(),
                source: repo.to_string_lossy().into_owned(),
                dir: tree.to_string_lossy().into_owned(),
                branch: Some(branch),
                base: Some(base),
                base_kind: Some("main".into()),
            },
            // `dir` のリポジトリは取り込みの対象外（ADR-0043 D5）。
            task_ops::workspace::WorktreeMarkerRepo {
                name: "data".into(),
                kind: "dir".into(),
                source: env.dir.path().join("data").to_string_lossy().into_owned(),
                dir: task_dir
                    .join("repos")
                    .join("data")
                    .to_string_lossy()
                    .into_owned(),
                branch: None,
                base: None,
                base_kind: None,
            },
        ],
    };
    task_ops::workspace::write_marker(&task_dir, &marker).expect("marker");
    (task.id, repo, tree)
}

/// ADR-0043 D5: リポジトリごとの差分（`dir` は出ない）と、1 ファイルの unified diff。
#[tokio::test]
async fn the_changes_list_every_git_repo_and_serve_one_file_diff() {
    let (env, _bin) = env_with_gh();
    let app = env.router();
    let (task_id, _repo, tree) = seed_git_task(&env, None);

    // 未コミットの変更と追跡外のファイルも出る。
    std::fs::write(tree.join("README.md"), b"hello\nmore\n").expect("write");
    std::fs::write(tree.join("new.txt"), b"x\n").expect("write");

    let resp = send(&app, g(&format!("/api/v1/tasks/{task_id}/changes"))).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    let repos = body["repos"].as_array().expect("repos");
    assert_eq!(repos.len(), 1, "`dir` のリポジトリは対象外: {body}");
    let repo = &repos[0];
    assert_eq!(repo["repo"], "code");
    assert_eq!(repo["default_branch"], "main");
    assert_eq!(repo["ahead"], 1);
    assert_eq!(repo["dirty"], true);
    assert_eq!(repo["missing"], false);
    assert_eq!(repo["origin"], false, "origin を足していない");
    assert!(repo["integration"].is_null(), "まだ取り込んでいない");
    assert_eq!(repo["stat"]["files"], 3);
    let paths: Vec<&str> = repo["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|f| f["path"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(paths, vec!["README.md", "new.txt", "src.txt"], "{repo}");
    assert_eq!(body["merge_method"], "merge");

    // 1 ファイルの diff。
    let diff = send(
        &app,
        g(&format!(
            "/api/v1/tasks/{task_id}/changes/code/diff?path=src.txt"
        )),
    )
    .await;
    assert_eq!(diff.status.as_u16(), 200, "{}", diff.text());
    assert!(
        diff.json()["diff"]
            .as_str()
            .is_some_and(|d| d.contains("+a")),
        "{}",
        diff.text()
    );
    assert_eq!(diff.json()["truncated"], false);

    // 境界と 404。
    assert_eq!(
        send(
            &app,
            g(&format!("/api/v1/tasks/{task_id}/changes/code/diff"))
        )
        .await
        .status
        .as_u16(),
        400
    );
    assert_problem(
        &send(
            &app,
            g(&format!(
                "/api/v1/tasks/{task_id}/changes/code/diff?path=../x"
            )),
        )
        .await,
        403,
        "path_forbidden",
    );
    assert_problem(
        &send(
            &app,
            g(&format!("/api/v1/tasks/{task_id}/changes/nope/diff?path=a")),
        )
        .await,
        404,
        "file_not_found",
    );
    // 作業ツリーを持たないタスクは 404。
    let bare = new_task(TaskKind::Execute, Status::Done);
    env.seed(&bare);
    assert_problem(
        &send(&app, g(&format!("/api/v1/tasks/{}/changes", bare.id))).await,
        404,
        "file_not_found",
    );
    assert_problem(
        &send(&app, g(&format!("/api/v1/tasks/{}/changes", TaskId::new()))).await,
        404,
        "task_not_found",
    );
}

/// ADR-0043 D5 の `merge`: `main` が綺麗なら fast-forward し、worktree とブランチを消して
/// `state = done` を記録する。押せるのは人（管理系のトークン）だけ。
#[tokio::test]
async fn merging_fast_forwards_the_default_branch_and_records_the_integration() {
    let (env, _bin) = env_with_gh();
    let app = env.router();
    let (task_id, repo, tree) = seed_git_task(&env, None);
    let before = git_out(&repo, &["rev-parse", "refs/heads/main"]);

    // 読み取りはトークン無しでよいが、取り込みは管理系。
    let anon = send(
        &app,
        post_json(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "merge"}),
        ),
    )
    .await;
    assert_problem(&anon, 401, "unauthorized");

    let resp = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "merge", "note": "見たよ"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["integration"]["method"], "merge");
    assert_eq!(body["integration"]["state"], "done");
    assert_eq!(body["integration"]["repo"], "code");
    assert!(body["child_task_id"].is_null());
    assert!(
        body["integration"]["detail"]
            .as_str()
            .is_some_and(|d| d.starts_with("見たよ / ")),
        "{body}"
    );

    // `main` が進み、worktree もブランチも消えている。
    assert_ne!(git_out(&repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(
        repo.join("src.txt").is_file(),
        "人の作業ツリーも早送りされる"
    );
    assert!(!tree.exists());
    assert!(!branch_exists(&repo, &format!("celeris/{task_id}")));

    // 取り込んだ後の `changes` は「もう無い」。記録は残る。
    let after = send(&app, g(&format!("/api/v1/tasks/{task_id}/changes")))
        .await
        .json();
    assert_eq!(after["repos"][0]["missing"], true, "{after}");
    assert_eq!(after["repos"][0]["ahead"], 0);
    assert_eq!(after["repos"][0]["integration"]["state"], "done");
}

/// ADR-0043 D5: 人が `main` を編集中なら 409（何も触らない）。
#[tokio::test]
async fn merging_while_the_default_branch_is_being_edited_is_409() {
    let (env, _bin) = env_with_gh();
    let app = env.router();
    let (task_id, repo, tree) = seed_git_task(&env, None);
    std::fs::write(repo.join("README.md"), b"human is editing\n").expect("write");
    let before = git_out(&repo, &["rev-parse", "refs/heads/main"]);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "merge"}),
        ),
    )
    .await;
    let problem = assert_problem(&resp, 409, "default_branch_busy");
    assert_eq!(problem["detail"], "main が編集中");
    assert_eq!(git_out(&repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(tree.join("src.txt").is_file(), "worktree は残る");
    // 何も起きていないので記録も残さない。
    let after = send(&app, g(&format!("/api/v1/tasks/{task_id}/changes")))
        .await
        .json();
    assert!(after["repos"][0]["integration"].is_null(), "{after}");
}

/// ADR-0043 D5: rebase が衝突したら `state = conflict` を記録し、「衝突の解消」タスクを作る。
#[tokio::test]
async fn a_conflicting_merge_records_a_conflict_and_creates_the_child_task() {
    let (env, _bin) = env_with_gh();
    let app = env.router();
    let (task_id, repo, tree) = seed_git_task(&env, None);
    // ブランチと `main` が同じ行を動かす。
    std::fs::write(tree.join("README.md"), b"task side\n").expect("write");
    git(&tree, &["add", "-A"]);
    git(&tree, &["commit", "-q", "-m", "task"]);
    std::fs::write(repo.join("README.md"), b"human side\n").expect("write");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "human"]);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "merge"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["integration"]["state"], "conflict");
    assert!(
        body["integration"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("README.md")),
        "{body}"
    );
    let child_id = body["child_task_id"].as_str().expect("child").to_string();

    // worktree もブランチも残る（衝突の解消はそこで行う）。
    assert!(tree.join("README.md").is_file());
    assert!(branch_exists(&repo, &format!("celeris/{task_id}")));

    // 子タスク: 親の子で、`ready`、親の worktree の上（`mode = shared`）、受け入れ条件は検査コマンド。
    let child = send(&app, g(&format!("/api/v1/tasks/{child_id}"))).await;
    assert_eq!(child.status.as_u16(), 200, "{}", child.text());
    let child = child.json();
    let task = &child["task"];
    assert_eq!(task["parent_id"], task_id.to_string());
    assert_eq!(task["status"], "ready");
    assert!(
        task["title"]
            .as_str()
            .is_some_and(|t| t.starts_with("衝突の解消: ")),
        "{task}"
    );
    assert_eq!(task["workspace"]["kind"], "local");
    assert_eq!(task["workspace"]["path"], tree.to_string_lossy().as_ref());
    assert_eq!(task["workspace"]["mode"], "shared");
    assert!(
        task["repos"].as_array().is_none_or(|r| r.is_empty()),
        "{task}"
    );
    assert!(
        task["objective"]
            .as_str()
            .is_some_and(|o| o.contains("README.md")),
        "{task}"
    );
    let checks: Vec<&str> = task["acceptance"]
        .as_array()
        .expect("acceptance")
        .iter()
        .map(|c| c["check"]["type"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(checks, vec!["command", "command", "command"], "{task}");
}

/// ADR-0043 D5: `discard` は確認が要る（422）。確認すれば worktree とブランチが消える。
#[tokio::test]
async fn discarding_needs_confirmation_and_then_removes_the_worktree_and_branch() {
    let (env, _bin) = env_with_gh();
    let app = env.router();
    let (task_id, repo, tree) = seed_git_task(&env, None);

    let without = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "discard"}),
        ),
    )
    .await;
    let problem = assert_problem(&without, 422, "validation");
    assert_eq!(problem["errors"][0]["field"], "confirm");
    assert!(tree.exists(), "確認していないので何も起きない");

    let resp = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "discard", "confirm": true}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["integration"]["method"], "discard");
    assert_eq!(resp.json()["integration"]["state"], "done");
    assert!(!tree.exists());
    assert!(!branch_exists(&repo, &format!("celeris/{task_id}")));
    assert!(repo.join("README.md").is_file(), "元のリポジトリは無事");
}

/// ADR-0043 D5 の `pr`: `origin` が無ければ 409。あれば push して `gh pr create`、
/// 開いたときに `gh pr view` で同期し、「Celeris で merge」で `gh pr merge` を呼んで片付ける。
/// **偽の `gh`** とローカルの bare リポジトリしか使わない（外に出ない）。
#[tokio::test]
async fn a_pull_request_is_pushed_created_refreshed_and_merged_with_a_fake_gh() {
    let (env, bin) = env_with_gh();
    let app = env.router();
    let (task_id, repo, tree) = seed_git_task(&env, None);
    let branch = format!("celeris/{task_id}");

    // `origin` が無いうちは 409。
    let no_origin = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "pr"}),
        ),
    )
    .await;
    let problem = assert_problem(&no_origin, 409, "pr_unavailable");
    assert!(
        problem["detail"]
            .as_str()
            .is_some_and(|d| d.contains("origin")),
        "{problem}"
    );

    // ローカルの bare リポジトリを `origin` にする。
    let origin = env.dir.path().join("origin.git");
    let out = std::process::Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&origin)
        .output()
        .expect("git init --bare");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );

    let created = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "pr"}),
        ),
    )
    .await;
    assert_eq!(created.status.as_u16(), 200, "{}", created.text());
    let body = created.json();
    assert_eq!(body["integration"]["method"], "pr");
    assert_eq!(body["integration"]["state"], "open");
    assert_eq!(body["integration"]["pr_number"], 42);
    assert_eq!(
        body["integration"]["pr_url"],
        "https://github.com/o/r/pull/42"
    );

    // 本当に push された（bare リポジトリにブランチが立っている）。
    assert!(branch_exists(&origin, &branch), "push されていない");
    // `gh pr create` の引数に題名と base が入っている。
    let calls = std::fs::read_to_string(bin.join("calls.log")).unwrap_or_default();
    assert!(calls.contains("pr create --base main --head"), "{calls}");

    // 画面を開いたら同期する（まだ OPEN なので何も変わらない）。
    let open = send(&app, g(&format!("/api/v1/tasks/{task_id}/changes")))
        .await
        .json();
    assert_eq!(open["repos"][0]["integration"]["state"], "open", "{open}");
    assert_eq!(open["gh"], true);
    assert_eq!(open["repos"][0]["origin"], true);

    // 「Celeris で merge」。
    let merged = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/pr/merge"),
            &json!({}),
        ),
    )
    .await;
    assert_eq!(merged.status.as_u16(), 200, "{}", merged.text());
    assert_eq!(merged.json()["integration"]["state"], "merged");
    assert!(merged.json()["integration"]["merged_at"].is_string());
    let log = std::fs::read_to_string(bin.join("merged.log")).unwrap_or_default();
    assert!(log.contains("pr merge 42 --merge --delete-branch"), "{log}");
    // merge されたら worktree とローカルのブランチを片付ける（ADR-0043 D5）。
    assert!(!tree.exists());
    assert!(!branch_exists(&repo, &branch));

    // 2 回目は 409（もう開いていない）。
    let again = send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/pr/merge"),
            &json!({}),
        ),
    )
    .await;
    assert_problem(&again, 409, "pr_unavailable");
}

/// ADR-0043 D5: 案件画面の「PR と取り込み」（タスク × リポジトリごとに最新の 1 件）。
#[tokio::test]
async fn the_project_page_lists_the_integrations_of_its_tasks() {
    let (env, _bin) = env_with_gh();
    let app = env.router();
    let project = send(
        &app,
        p(
            "/api/v1/projects",
            &json!({"title": "benchfs", "request": "測る"}),
        ),
    )
    .await;
    assert_eq!(project.status.as_u16(), 201, "{}", project.text());
    let project_id = project.json()["id"].as_str().expect("id").to_string();
    let parsed: task_core::ProjectId = project_id.parse().expect("project id");

    // 空でも 200。
    let empty = send(
        &app,
        g(&format!("/api/v1/projects/{project_id}/integrations")),
    )
    .await;
    assert_eq!(empty.status.as_u16(), 200, "{}", empty.text());
    assert_eq!(empty.json()["items"].as_array().expect("items").len(), 0);
    assert_problem(
        &send(
            &app,
            g(&format!(
                "/api/v1/projects/{}/integrations",
                task_core::ProjectId::new()
            )),
        )
        .await,
        404,
        "project_not_found",
    );

    let (task_id, _repo, _tree) = seed_git_task(&env, Some(parsed));
    send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "merge"}),
        ),
    )
    .await;
    // 同じタスク・同じリポジトリをもう一度取り込む（記録は 2 件になるが、一覧には最新の 1 件だけ）。
    send(
        &app,
        p(
            &format!("/api/v1/tasks/{task_id}/changes/code/integrate"),
            &json!({"method": "discard", "confirm": true}),
        ),
    )
    .await;

    let list = send(
        &app,
        g(&format!("/api/v1/projects/{project_id}/integrations")),
    )
    .await;
    assert_eq!(list.status.as_u16(), 200, "{}", list.text());
    let items = list.json()["items"].as_array().expect("items").clone();
    assert_eq!(
        items.len(),
        1,
        "タスク × リポジトリごとに最新の 1 件: {}",
        list.text()
    );
    assert_eq!(items[0]["integration"]["method"], "discard");
    assert_eq!(items[0]["integration"]["repo"], "code");
    assert_eq!(items[0]["task_title"], "Execute task");
    assert_eq!(items[0]["task_status"], "done");
}
