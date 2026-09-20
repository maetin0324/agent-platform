//! ADR-0043 D1 / D6（Phase 52）: 案件のリポジトリ（`/projects/{id}/repos`、`/repos/{id}`）と
//! タスクの作業ツリーの閲覧（`/tasks/{id}/tree`、`/tasks/{id}/tree/file`）。
//!
//! 見るもの: 正常系、変更系が管理系であること（トークン必須）、404 / 409（使用中の削除）/
//! 422（重複した名前・知らないクラスタ・未対応の組み合わせ）、`Project.workspace` の後方互換、
//! ファイル閲覧の境界（`..` / 絶対パス / シンボリックリンクの脱出は 403、バイナリと大きいファイル）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{Status, TaskKind};

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(path, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(path, body, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn pa(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    patch_json_with(path, body, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn d(path: &str) -> axum::http::Request<axum::body::Body> {
    delete_with(path, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn env() -> TestEnv {
    TestEnv::with(EnvOptions { token: Some(TOKEN.into()), ..EnvOptions::default() })
}

async fn make_project(app: &axum::Router) -> String {
    let resp = send(app, p("/api/v1/projects", &json!({"title": "benchfs", "request": "測る"}))).await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    resp.json()["id"].as_str().expect("id").to_string()
}

/// ADR-0043 D1: 作る → 一覧 → primary を移す → 直す → 消す。`Project.workspace` は primary の写し。
#[tokio::test]
async fn repos_can_be_created_listed_repointed_and_deleted() {
    let env = env();
    let app = env.router();
    let project = make_project(&app).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let code = dir.path().join("benchfs");
    let paper = dir.path().join("benchfs-paper");
    std::fs::create_dir_all(code.join(".git")).expect("mkdir");
    std::fs::create_dir_all(&paper).expect("mkdir");

    // 名前も kind も省略できる（パスの末尾から slug、`.git` があれば git）。
    let created = send(
        &app,
        p(
            &format!("/api/v1/projects/{project}/repos"),
            &json!({"location": {"kind": "local", "path": code.to_string_lossy()}}),
        ),
    )
    .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text());
    let first = created.json();
    assert_eq!(first["name"], "benchfs");
    assert_eq!(first["kind"], "git");
    assert_eq!(first["run"], "auto");
    assert_eq!(first["is_primary"], true, "最初の 1 件は primary");
    let first_id = first["id"].as_str().expect("id").to_string();

    // `Project.workspace` は primary の写し（GUI の後方互換）。
    let detail = send(&app, g(&format!("/api/v1/projects/{project}"))).await.json();
    assert_eq!(detail["project"]["workspace"]["kind"], "local");
    assert_eq!(detail["project"]["workspace"]["path"], code.to_string_lossy().as_ref());
    assert_eq!(detail["repos"].as_array().expect("repos").len(), 1);

    // 2 件目（git ではないディレクトリ）。
    let second = send(
        &app,
        p(
            &format!("/api/v1/projects/{project}/repos"),
            &json!({"name": "paper", "location": {"kind": "local", "path": paper.to_string_lossy()}}),
        ),
    )
    .await;
    assert_eq!(second.status.as_u16(), 201, "{}", second.text());
    let second_id = second.json()["id"].as_str().expect("id").to_string();
    assert_eq!(second.json()["kind"], "dir", "`.git` が無ければ dir");
    assert_eq!(second.json()["is_primary"], false);

    let list = send(&app, g(&format!("/api/v1/projects/{project}/repos"))).await.json();
    let items = list["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["name"], "benchfs", "primary が先頭");

    // primary を移す。
    let moved = send(&app, pa(&format!("/api/v1/repos/{second_id}"), &json!({"is_primary": true}))).await;
    assert_eq!(moved.status.as_u16(), 200, "{}", moved.text());
    assert_eq!(moved.json()["is_primary"], true);
    let detail = send(&app, g(&format!("/api/v1/projects/{project}"))).await.json();
    assert_eq!(detail["project"]["workspace"]["path"], paper.to_string_lossy().as_ref());
    assert_eq!(detail["repos"][0]["name"], "paper");

    // 名前と run を直す。
    let patched = send(
        &app,
        pa(&format!("/api/v1/repos/{first_id}"), &json!({"name": "code", "run": "host"})),
    )
    .await;
    assert_eq!(patched.status.as_u16(), 200, "{}", patched.text());
    assert_eq!(patched.json()["name"], "code");
    assert_eq!(patched.json()["run"], "host");

    // 消す。
    let deleted = send(&app, d(&format!("/api/v1/repos/{first_id}"))).await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text());
    let list = send(&app, g(&format!("/api/v1/projects/{project}/repos"))).await.json();
    assert_eq!(list["items"].as_array().expect("items").len(), 1);
    assert_eq!(
        send(&app, d(&format!("/api/v1/repos/{first_id}"))).await.status.as_u16(),
        404
    );
}

/// ADR-0043 D1: 変更系は管理系（トークンが要る）。読み取りは要らない。
#[tokio::test]
async fn changing_repos_needs_the_admin_token_but_reading_does_not() {
    let env = env();
    let app = env.router();
    let project = make_project(&app).await;
    let body = json!({"location": {"kind": "local", "path": "/srv/x"}});

    for request in [
        post_json(&format!("/api/v1/projects/{project}/repos"), &body),
        patch_json_with(&format!("/api/v1/repos/{}", task_core::RepoId::new()), &body, &[]),
        delete_with(&format!("/api/v1/repos/{}", task_core::RepoId::new()), &[]),
    ] {
        let resp = send(&app, request).await;
        assert_problem(&resp, 401, "unauthorized");
    }
    // 読み取りはトークンだけあればよい（このテストの celeris はトークンを設定しているので共通ガードは効く）。
    let resp = send(&app, g(&format!("/api/v1/projects/{project}/repos"))).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
}

/// ADR-0043 D1: 422 になる入力（重複した名前・不正な slug・知らないクラスタ・未対応の `sync = "none"`）と 404。
#[tokio::test]
async fn invalid_repo_input_is_422_and_unknown_ids_are_404() {
    let env = env();
    let app = env.router();
    let project = make_project(&app).await;
    let base = format!("/api/v1/projects/{project}/repos");

    let ok = send(&app, p(&base, &json!({"name": "code", "location": {"kind": "local", "path": "/srv/code"}}))).await;
    assert_eq!(ok.status.as_u16(), 201, "{}", ok.text());

    // 名前の重複。
    let dup = send(&app, p(&base, &json!({"name": "code", "location": {"kind": "local", "path": "/srv/other"}}))).await;
    assert_problem(&dup, 422, "validation");

    // slug でない名前（`repos/<name>/` というディレクトリ名になるので境界）。
    for bad in ["../escape", "Upper", "", ".hidden"] {
        let resp = send(&app, p(&base, &json!({"name": bad, "location": {"kind": "local", "path": "/srv/x"}}))).await;
        assert_problem(&resp, 422, "validation");
    }

    // 知らないクラスタ（ADR-0039 D1 と同じ規律）。
    let unknown_cluster = send(
        &app,
        p(&base, &json!({"location": {"kind": "remote", "cluster": "nope", "path": "/work/x"}})),
    )
    .await;
    let problem = assert_problem(&unknown_cluster, 422, "validation");
    assert_eq!(problem["errors"][0]["field"], "workspace.cluster");

    // ADR-0043 D7: リモート (b)（`sync = "none"`）は未対応。
    let reserved = send(
        &app,
        p(
            &base,
            &json!({"location": {"kind": "remote", "cluster": "pegasus", "path": "/work/x"}, "sync": "none"}),
        ),
    )
    .await;
    assert_problem(&reserved, 422, "validation");

    // 相対パス。
    let relative = send(&app, p(&base, &json!({"location": {"kind": "local", "path": "relative"}}))).await;
    assert_problem(&relative, 422, "validation");

    // 知らない id。
    let missing = task_core::RepoId::new();
    assert_problem(
        &send(&app, pa(&format!("/api/v1/repos/{missing}"), &json!({"run": "host"}))).await,
        404,
        "repo_not_found",
    );
    assert_problem(&send(&app, d(&format!("/api/v1/repos/{missing}"))).await, 404, "repo_not_found");
    assert_problem(
        &send(&app, g(&format!("/api/v1/projects/{}/repos", task_core::ProjectId::new()))).await,
        404,
        "project_not_found",
    );
    // 1 つも書かない PATCH は 422。
    let empty = send(&app, pa(&format!("/api/v1/repos/{missing}"), &json!({}))).await;
    assert_problem(&empty, 422, "validation");
}

/// ADR-0043 D1 / D2: `POST /tasks` の `repos` と、未終端のタスクが使っているリポジトリの 409。
#[tokio::test]
async fn a_task_picks_repos_by_name_and_blocks_their_deletion_until_it_finishes() {
    let env = env();
    let app = env.router();
    let project = make_project(&app).await;
    let base = format!("/api/v1/projects/{project}/repos");
    let code = send(&app, p(&base, &json!({"name": "code", "location": {"kind": "local", "path": "/srv/code"}}))).await;
    let code_id = code.json()["id"].as_str().expect("id").to_string();
    send(&app, p(&base, &json!({"name": "paper", "location": {"kind": "local", "path": "/srv/paper"}}))).await;

    let body = |repos: Value, project: Option<&str>| {
        let mut task = json!({
            "title": "実装",
            "objective": "やる",
            "acceptance": [{"type": "human", "text": "ok"}],
        });
        if let Some(project) = project {
            task["project_id"] = json!(project);
        }
        task["repos"] = repos;
        task
    };

    // 名前で選ぶ（並びはそのまま = `repos[0]` が cwd）。
    let created = send(&app, p("/api/v1/tasks", &body(json!(["paper", "code"]), Some(&project)))).await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text());
    let task = created.json();
    assert_eq!(task["repos"][0]["name"], "paper");
    assert_eq!(task["repos"][1]["name"], "code");
    let task_id = task["id"].as_str().expect("id").to_string();

    // 書かなければ案件の primary を継ぐ。
    let inherited = send(&app, p("/api/v1/tasks", &body(json!([]), Some(&project)))).await;
    assert_eq!(inherited.json()["repos"][0]["name"], "code", "{}", inherited.text());

    // 知らない名前は 422。
    let unknown = send(&app, p("/api/v1/tasks", &body(json!(["nope"]), Some(&project)))).await;
    assert_problem(&unknown, 422, "validation");

    // 案件に属さないタスクに `repos` は書けない。
    let orphan = send(&app, p("/api/v1/tasks", &body(json!(["code"]), None))).await;
    assert_problem(&orphan, 422, "validation");

    // 未終端のタスクが使っているリポジトリは消せない。
    let busy = send(&app, d(&format!("/api/v1/repos/{code_id}"))).await;
    assert_problem(&busy, 409, "repo_in_use");

    // 終わったら消せる。
    for id in [task_id.as_str(), inherited.json()["id"].as_str().expect("id")] {
        let cancelled = send(&app, p(&format!("/api/v1/tasks/{id}/cancel"), &json!({}))).await;
        assert_eq!(cancelled.status.as_u16(), 200, "{}", cancelled.text());
    }
    let freed = send(&app, d(&format!("/api/v1/repos/{code_id}"))).await;
    assert_eq!(freed.status.as_u16(), 204, "{}", freed.text());
}

/// ADR-0043 D2: リモートのリポジトリを他と混ぜたタスクはこの Phase では 422。
#[tokio::test]
async fn mixing_a_remote_repo_with_a_local_one_is_rejected() {
    let env = env();
    let app = env.router();
    let project = make_project(&app).await;
    let base = format!("/api/v1/projects/{project}/repos");
    send(&app, p(&base, &json!({"name": "code", "location": {"kind": "local", "path": "/srv/code"}}))).await;
    let remote = send(
        &app,
        p(
            &base,
            &json!({"name": "cluster", "location": {"kind": "remote", "cluster": "pegasus", "path": "/work/x"}}),
        ),
    )
    .await;
    assert_eq!(remote.status.as_u16(), 201, "{}", remote.text());

    let task = |repos: Value| {
        json!({
            "title": "t", "objective": "o",
            "acceptance": [{"type": "human", "text": "ok"}],
            "project_id": project, "repos": repos,
        })
    };
    // リモート 1 つだけなら通る（ADR-0018 / 0019 の従来の経路）。
    let alone = send(&app, p("/api/v1/tasks", &task(json!(["cluster"])))).await;
    assert_eq!(alone.status.as_u16(), 201, "{}", alone.text());
    // 混ぜたら 422。
    let mixed = send(&app, p("/api/v1/tasks", &task(json!(["cluster", "code"])))).await;
    let problem = assert_problem(&mixed, 422, "validation");
    assert!(
        problem["errors"][0]["message"].as_str().is_some_and(|m| m.contains("remote")),
        "{problem}"
    );
}

// ---- ADR-0043 D6: タスクの作業ツリーの閲覧 ----

/// 目印（`worktree.json`）と作業ツリーを用意したタスクを作る。
fn seed_tree(env: &TestEnv) -> (task_core::TaskId, std::path::PathBuf) {
    let task = new_task(TaskKind::Execute, Status::Done);
    env.seed(&task);
    let task_dir = env.workspace(&task);
    let code = task_dir.join("repos").join("code");
    let data_target = env.dir.path().join("data-store");
    std::fs::create_dir_all(code.join("src")).expect("mkdir");
    std::fs::create_dir_all(&data_target).expect("mkdir");
    std::fs::write(code.join("README.md"), b"# hello\n").expect("write");
    std::fs::write(code.join("src/lib.rs"), b"fn main() {}\n").expect("write");
    std::fs::write(data_target.join("one.csv"), b"1\n").expect("write");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&data_target, task_dir.join("repos").join("data")).expect("symlink");

    let marker = task_ops::workspace::WorktreeMarker {
        repo: "/srv/code".into(),
        dir: code.to_string_lossy().into_owned(),
        branch: "celeris/x".into(),
        base: "abc123".into(),
        base_kind: "main".into(),
        repos: vec![
            task_ops::workspace::WorktreeMarkerRepo {
                name: "code".into(),
                kind: "git".into(),
                source: "/srv/code".into(),
                dir: code.to_string_lossy().into_owned(),
                branch: Some("celeris/x".into()),
                base: Some("abc123".into()),
                base_kind: Some("main".into()),
            },
            task_ops::workspace::WorktreeMarkerRepo {
                name: "data".into(),
                kind: "dir".into(),
                source: data_target.to_string_lossy().into_owned(),
                dir: task_dir.join("repos").join("data").to_string_lossy().into_owned(),
                branch: None,
                base: None,
                base_kind: None,
            },
        ],
    };
    task_ops::workspace::write_marker(&task_dir, &marker).expect("marker");
    (task.id, code)
}

/// ADR-0043 D6: 一覧（既定は先頭のリポジトリ、`repo=` で切り替え、`dir` はリンク越しに見える）。
#[tokio::test]
async fn the_tree_lists_every_repo_of_the_task() {
    let env = env();
    let app = env.router();
    let (task_id, _) = seed_tree(&env);

    let root = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree"))).await;
    assert_eq!(root.status.as_u16(), 200, "{}", root.text());
    let view = root.json();
    assert_eq!(view["repo"], "code", "`repo=` を省略したら先頭");
    assert_eq!(view["path"], "");
    assert_eq!(view["repos"].as_array().expect("repos").len(), 2);
    assert_eq!(view["repos"][0]["branch"], "celeris/x");
    assert_eq!(view["repos"][1]["kind"], "dir");
    // ディレクトリが先、あとは名前順。
    assert_eq!(view["entries"][0]["name"], "src");
    assert_eq!(view["entries"][0]["kind"], "dir");
    assert_eq!(view["entries"][1]["name"], "README.md");
    assert_eq!(view["entries"][1]["kind"], "file");
    assert_eq!(view["entries"][1]["size"], 8);

    let sub = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree?path=src"))).await.json();
    assert_eq!(sub["path"], "src");
    assert_eq!(sub["entries"][0]["path"], "src/lib.rs");

    // `dir` のリポジトリ（シンボリックリンク）もリンク越しに見える。
    let data = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree?repo=data"))).await;
    assert_eq!(data.status.as_u16(), 200, "{}", data.text());
    assert_eq!(data.json()["entries"][0]["name"], "one.csv");
}

/// ADR-0043 D6: 本文（テキスト・バイナリ・大きすぎるもの）。
#[tokio::test]
async fn the_tree_file_returns_text_and_only_the_size_for_binaries() {
    let env = env();
    let app = env.router();
    let (task_id, code) = seed_tree(&env);

    let text = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree/file?path=README.md"))).await;
    assert_eq!(text.status.as_u16(), 200, "{}", text.text());
    let view = text.json();
    assert_eq!(view["repo"], "code");
    assert_eq!(view["text"], "# hello\n");
    assert_eq!(view["binary"], false);
    assert_eq!(view["too_large"], false);

    std::fs::write(code.join("blob.bin"), [0u8, 1, 2, 3]).expect("write");
    let binary = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree/file?path=blob.bin"))).await.json();
    assert_eq!(binary["binary"], true);
    assert_eq!(binary["size"], 4);
    assert!(binary.get("text").is_none(), "{binary}");

    let big = vec![b'a'; (task_api::MAX_TEXT_BYTES + 1) as usize];
    std::fs::write(code.join("big.txt"), &big).expect("write");
    let large = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree/file?path=big.txt"))).await.json();
    assert_eq!(large["too_large"], true);
    assert_eq!(large["size"], big.len());
    assert!(large.get("text").is_none(), "{large}");
}

/// ADR-0043 D6 / ADR-0003 D5: `..`・絶対パス・シンボリックリンクの脱出は 403。知らないものは 404。
#[tokio::test]
async fn the_tree_refuses_to_leave_the_working_tree() {
    let env = env();
    let app = env.router();
    let (task_id, code) = seed_tree(&env);
    let secret = env.dir.path().join("secret.txt");
    std::fs::write(&secret, b"nope\n").expect("write");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret, code.join("escape.txt")).expect("symlink");

    for path in ["..", "../..", "src/../../secret.txt", "/etc/passwd"] {
        let resp = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree?path={path}"))).await;
        assert_problem(&resp, 403, "path_forbidden");
        let resp = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree/file?path={path}"))).await;
        assert_problem(&resp, 403, "path_forbidden");
    }
    // 作業ツリーの外を指すシンボリックリンクも 403。
    let escaped = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree/file?path=escape.txt"))).await;
    assert_problem(&escaped, 403, "path_forbidden");
    // ディレクトリを本文として読もうとしたら 403。
    let dir = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree/file?path=src"))).await;
    assert_problem(&dir, 403, "path_forbidden");
    // `path` 無しの本文は 400。
    let missing_path = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree/file"))).await;
    assert_problem(&missing_path, 400, "bad_request");
    // そのタスクに無いリポジトリは 404。
    let unknown_repo = send(&app, g(&format!("/api/v1/tasks/{task_id}/tree?repo=nope"))).await;
    assert_problem(&unknown_repo, 404, "file_not_found");
    // 作業ツリーを持たないタスクは 404。
    let plain = new_task(TaskKind::Execute, Status::Done);
    env.seed(&plain);
    let no_tree = send(&app, g(&format!("/api/v1/tasks/{}/tree", plain.id))).await;
    assert_problem(&no_tree, 404, "file_not_found");
}
