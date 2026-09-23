//! ADR-0044 D7（Phase 57）: 文書（`/projects/{id}/docs`、`/projects/{id}/docs/page`、
//! `/projects/{id}/docs/init`、`/tasks/{id}/artifacts/promote`）。
//!
//! 見るもの: 文書の根の決まり方（primary の `[outputs].docs` / 既定 `docs` / リポジトリが無い案件は作る）、
//! ツリーと `q=`（`git grep`）、ページの描画（front matter・リンク・生 HTML を捨てる・履歴）、
//! `PUT` / `DELETE`（etag の 409、`main` 編集中の 409、`..` の 403、`.md` 以外の 422）、
//! 昇格（404 / 409 / `overwrite` / front matter の混ぜ方 / 逆リンク）、管理系の 401。
//!
//! **外部ネットワークには出ない**（CLAUDE.md）。リポジトリは全部 tempdir の中の git リポジトリ。

mod common;

use common::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use task_core::{ArtifactRef, Event, Status, Task, TaskKind, TaskStore};

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

fn pu(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    put_json_with(
        path,
        body,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn d(path: &str) -> axum::http::Request<axum::body::Body> {
    delete_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn env() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..EnvOptions::default()
    })
}

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

async fn make_project(app: &axum::Router, title: &str) -> String {
    let resp = send(
        app,
        p(
            "/api/v1/projects",
            &json!({"title": title, "request": "書く"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    resp.json()["id"].as_str().expect("id").to_string()
}

/// `main` に `docs/README.md` を 1 枚持つ git リポジトリ（人のチェックアウトのつもり）。
fn seed_repo(dir: &Path, docs: &str, workspace_toml: Option<&str>) {
    std::fs::create_dir_all(dir).expect("mkdir");
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "t"]);
    let root = dir.join(docs);
    std::fs::create_dir_all(&root).expect("mkdir docs");
    std::fs::write(root.join("README.md"), b"# \xe6\xa1\x88\xe4\xbb\xb6\n\n\xe6\x9c\x80\xe5\x88\x9d\xe3\x81\xae\xe3\x83\x9a\xe3\x83\xbc\xe3\x82\xb8\n")
        .expect("write");
    if let Some(toml) = workspace_toml {
        let path = dir.join(".config/celeris/workspace.toml");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir config");
        std::fs::write(&path, toml).expect("write toml");
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "first"]);
}

/// 案件 + primary の git リポジトリ。
async fn project_with_repo(
    app: &axum::Router,
    dir: &Path,
    docs: &str,
    toml: Option<&str>,
) -> (String, PathBuf) {
    let project = make_project(app, "Pluvio PoC").await;
    let repo = dir.join("primary");
    seed_repo(&repo, docs, toml);
    let created = send(
        app,
        p(
            &format!("/api/v1/projects/{project}/repos"),
            &json!({"location": {"kind": "local", "path": repo.to_string_lossy()}}),
        ),
    )
    .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text());
    assert_eq!(created.json()["is_primary"], true);
    (project, repo)
}

// ---------------------------------------------------------------------------
// 文書の根の決まり方（ADR-0044 D7）
// ---------------------------------------------------------------------------

/// primary の `[outputs].docs` を使う。既定は `docs`。
#[tokio::test]
async fn the_docs_root_comes_from_the_primary_repository() {
    let env = env();
    let app = env.router();
    let dir = tempfile::tempdir().expect("tempdir");

    // 既定（`workspace.toml` が無い）。
    let (project, _repo) = project_with_repo(&app, dir.path(), "docs", None).await;
    let tree = send(&app, g(&format!("/api/v1/projects/{project}/docs"))).await;
    assert_eq!(tree.status.as_u16(), 200, "{}", tree.text());
    let body = tree.json();
    assert_eq!(body["root"], "docs");
    assert_eq!(body["repo"], "primary");
    assert_eq!(body["default_branch"], "main");
    assert_eq!(body["items"][0]["path"], "docs/README.md");
    assert_eq!(body["items"][0]["title"], "案件");
    assert_eq!(body["items"][0]["last_commit"]["subject"], "first");
    assert!(
        body["items"][0]["updated_at"]
            .as_str()
            .is_some_and(|t| t.contains('T')),
        "{body}"
    );

    // `[outputs] docs` を書けばそこが根（ADR-0043 D4）。
    let other = tempfile::tempdir().expect("tempdir");
    let (moved, _) = project_with_repo(
        &app,
        other.path(),
        "handbook",
        Some("[outputs]\ndocs = \"handbook\"\n"),
    )
    .await;
    let tree = send(&app, g(&format!("/api/v1/projects/{moved}/docs")))
        .await
        .json();
    assert_eq!(tree["root"], "handbook");
    assert_eq!(tree["items"][0]["path"], "handbook/README.md", "{tree}");
}

/// リポジトリの無い案件は 409。`docs/init`（管理系）で `~/workspace/<slug>/` に作って primary にする。
#[tokio::test]
async fn a_project_without_a_repository_gets_a_documents_repository_on_init() {
    let env = env();
    let app = env.router();
    let project = make_project(&app, "Pluvio PoC").await;

    // 読み取りは**何も作らない**。
    assert_problem(
        &send(&app, g(&format!("/api/v1/projects/{project}/docs"))).await,
        409,
        "docs_unavailable",
    );
    assert!(
        !env.docs_repo_root.join("pluvio-poc").exists(),
        "読み取りでは作らない"
    );

    // 管理系。トークンが無ければ 401。
    assert_problem(
        &send(
            &app,
            post_json(&format!("/api/v1/projects/{project}/docs/init"), &json!({})),
        )
        .await,
        401,
        "unauthorized",
    );

    let init = send(
        &app,
        p(&format!("/api/v1/projects/{project}/docs/init"), &json!({})),
    )
    .await;
    assert_eq!(init.status.as_u16(), 200, "{}", init.text());
    let body = init.json();
    assert_eq!(body["created"], true);
    assert_eq!(body["root"], "docs");
    assert_eq!(body["default_branch"], "main");
    assert_eq!(body["repo"], "pluvio-poc");
    let path = PathBuf::from(body["path"].as_str().expect("path"));
    assert_eq!(path, env.docs_repo_root.join("pluvio-poc"));
    assert!(path.join(".git").exists(), "git init されている");
    assert!(path.join("docs/README.md").is_file(), "最初のページがある");

    // primary として登録されている（`Project.workspace` の写しも動く）。
    let repos = send(&app, g(&format!("/api/v1/projects/{project}/repos")))
        .await
        .json();
    assert_eq!(repos["items"].as_array().expect("items").len(), 1);
    assert_eq!(repos["items"][0]["is_primary"], true);
    assert_eq!(repos["items"][0]["kind"], "git");
    assert_eq!(repos["items"][0]["default_branch"], "main");

    // ツリーが読める。2 回目の init は作り直さない。
    let tree = send(&app, g(&format!("/api/v1/projects/{project}/docs")))
        .await
        .json();
    assert_eq!(tree["items"][0]["path"], "docs/README.md", "{tree}");
    let again = send(
        &app,
        p(&format!("/api/v1/projects/{project}/docs/init"), &json!({})),
    )
    .await
    .json();
    assert_eq!(again["created"], false);

    // 題名が ASCII にならない案件は案件 id を使う。
    let jp = make_project(&app, "調査").await;
    let init = send(
        &app,
        p(&format!("/api/v1/projects/{jp}/docs/init"), &json!({})),
    )
    .await
    .json();
    assert_eq!(init["repo"], jp.to_lowercase(), "{init}");
}

/// primary が `dir`（git ではない）の案件も、文書リポジトリを作って primary を移す（ADR-0044 D7）。
#[tokio::test]
async fn a_dir_primary_is_replaced_by_a_documents_repository() {
    let env = env();
    let app = env.router();
    let dir = tempfile::tempdir().expect("tempdir");
    let project = make_project(&app, "Data Project").await;
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("mkdir");
    let created = send(
        &app,
        p(
            &format!("/api/v1/projects/{project}/repos"),
            &json!({"location": {"kind": "local", "path": data.to_string_lossy()}}),
        ),
    )
    .await;
    assert_eq!(created.json()["kind"], "dir");

    let init = send(
        &app,
        p(&format!("/api/v1/projects/{project}/docs/init"), &json!({})),
    )
    .await
    .json();
    assert_eq!(init["created"], true);
    assert_eq!(init["repo"], "data-project");
    let repos = send(&app, g(&format!("/api/v1/projects/{project}/repos")))
        .await
        .json();
    let items = repos["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    let primary: Vec<&str> = items
        .iter()
        .filter(|r| r["is_primary"] == true)
        .map(|r| r["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        primary,
        vec!["data-project"],
        "primary は文書リポジトリに移る"
    );
}

// ---------------------------------------------------------------------------
// ツリーと検索・ページの描画
// ---------------------------------------------------------------------------

/// `?q=` は `git grep -il`（大文字小文字を区別しない）。ページは front matter と履歴とリンクを持つ。
#[tokio::test]
async fn pages_are_searched_rendered_and_linked() {
    let env = env();
    let app = env.router();
    let dir = tempfile::tempdir().expect("tempdir");
    let (project, _repo) = project_with_repo(&app, dir.path(), "docs", None).await;
    let mut task = new_task(TaskKind::Execute, Status::Done);
    task.project_id = Some(project.parse().expect("project id"));
    env.seed(&task);

    let body = format!(
        "---\ntitle: 調べたこと\ntags: [research]\ntasks: [{}]\n---\n\n# 別の見出し\n\n\
         <script>alert(1)</script>\n\n[担当](celeris:task/{}) と [[../README.md]]\n\n\
         | a | b |\n| --- | --- |\n| 1 | 2 |\n",
        task.id, task.id
    );
    let put = send(
        &app,
        pu(
            &format!("/api/v1/projects/{project}/docs/page"),
            &json!({"path": "docs/research/fs.md", "body": body}),
        ),
    )
    .await;
    assert_eq!(put.status.as_u16(), 200, "{}", put.text());

    // ツリー（2 枚）と `q=`（1 枚）。
    let tree = send(&app, g(&format!("/api/v1/projects/{project}/docs")))
        .await
        .json();
    let paths: Vec<&str> = tree["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["path"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(paths, vec!["docs/README.md", "docs/research/fs.md"]);
    assert_eq!(
        tree["items"][1]["title"], "調べたこと",
        "front matter の title が勝つ"
    );
    let found = send(
        &app,
        g(&format!(
            "/api/v1/projects/{project}/docs?q=%E8%AA%BF%E3%81%B9"
        )),
    )
    .await
    .json();
    assert_eq!(found["q"], "調べ");
    assert_eq!(
        found["items"].as_array().expect("items").len(),
        1,
        "{found}"
    );
    assert_eq!(found["items"][0]["path"], "docs/research/fs.md");
    let none = send(&app, g(&format!("/api/v1/projects/{project}/docs?q=zzz")))
        .await
        .json();
    assert!(none["items"].as_array().expect("items").is_empty());

    // ページ。
    let page = send(
        &app,
        g(&format!(
            "/api/v1/projects/{project}/docs/page?path=docs/research/fs.md"
        )),
    )
    .await;
    assert_eq!(page.status.as_u16(), 200, "{}", page.text());
    let page = page.json();
    assert_eq!(page["title"], "調べたこと");
    assert_eq!(page["tags"][0], "research");
    assert_eq!(page["tasks"][0], task.id.to_string());
    assert_eq!(page["root"], "docs");
    assert!(
        page["etag"].as_str().is_some_and(|e| e.len() >= 40),
        "{page}"
    );
    let html = page["html"].as_str().expect("html");
    assert!(!html.contains("<script"), "生 HTML は捨てる: {html}");
    assert!(
        html.contains(&format!("href=\"/tasks/{}\"", task.id)),
        "{html}"
    );
    assert!(
        html.contains(&format!("/projects/{project}/docs?path=docs/README.md")),
        "[[…]] がリンクになる: {html}"
    );
    assert!(html.contains("<table>"), "表が描ける: {html}");
    assert_eq!(page["history"].as_array().expect("history").len(), 1);
    assert_eq!(page["history"][0]["subject"], "docs: docs/research/fs.md");
    assert_eq!(page["history"][0]["author"], "Celeris (human)");
    assert_eq!(page["too_large"], false);

    // 逆リンク: タイムラインにこのページが出る（front matter の `tasks:` に載っているから）。
    let timeline = send(&app, g(&format!("/api/v1/tasks/{}/timeline", task.id)))
        .await
        .json();
    let docs: Vec<&Value> = timeline["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|i| i["kind"] == "doc")
        .collect();
    assert_eq!(docs.len(), 1, "{timeline}");
    assert_eq!(docs[0]["path"], "docs/research/fs.md");
    assert_eq!(docs[0]["title"], "調べたこと");
    assert_eq!(docs[0]["project_id"], project.as_str());

    // 無いページは 404。
    assert_problem(
        &send(
            &app,
            g(&format!(
                "/api/v1/projects/{project}/docs/page?path=docs/none.md"
            )),
        )
        .await,
        404,
        "page_not_found",
    );
}

// ---------------------------------------------------------------------------
// 編集（PUT / DELETE。管理系）
// ---------------------------------------------------------------------------

/// 作る → 直す（etag）→ 409（古い etag・etag 無し）→ 消す。`main` は fast-forward される。
#[tokio::test]
async fn editing_a_page_commits_on_the_default_branch_and_checks_the_etag() {
    let env = env();
    let app = env.router();
    let dir = tempfile::tempdir().expect("tempdir");
    let (project, repo) = project_with_repo(&app, dir.path(), "docs", None).await;
    let page = format!("/api/v1/projects/{project}/docs/page");

    // 読み取りはトークン無しでよいが、書き込みは管理系。
    assert_problem(
        &send(
            &app,
            put_json_with(&page, &json!({"path": "docs/a.md", "body": "# A\n"}), &[]),
        )
        .await,
        401,
        "unauthorized",
    );

    let created = send(
        &app,
        pu(&page, &json!({"path": "docs/a.md", "body": "# A\n"})),
    )
    .await;
    assert_eq!(created.status.as_u16(), 200, "{}", created.text());
    let created = created.json();
    assert_eq!(created["path"], "docs/a.md");
    assert_eq!(created["deleted"], false);
    assert_eq!(created["unchanged"], false);
    let etag = created["etag"].as_str().expect("etag").to_string();
    // 人のチェックアウト（`main` を出している）もその場で進む。
    assert!(repo.join("docs/a.md").is_file(), "作業ツリーが早送りされる");

    // 既にあるのに etag を付けなければ 409。
    let missing = send(
        &app,
        pu(&page, &json!({"path": "docs/a.md", "body": "# B\n"})),
    )
    .await;
    let problem = assert_problem(&missing, 409, "etag_mismatch");
    assert_eq!(problem["etag"], etag, "いまの etag を返す");
    // 違う etag も 409。
    assert_problem(
        &send(
            &app,
            pu(
                &page,
                &json!({"path": "docs/a.md", "body": "# B\n", "etag": "0".repeat(40)}),
            ),
        )
        .await,
        409,
        "etag_mismatch",
    );

    // 正しい etag なら通る。message も効く。
    let updated = send(
        &app,
        pu(
            &page,
            &json!({"path": "docs/a.md", "body": "# B\n", "etag": etag, "message": "docs: 直した"}),
        ),
    )
    .await
    .json();
    let next = updated["etag"].as_str().expect("etag").to_string();
    assert_ne!(next, etag);
    let read = send(&app, g(&format!("{page}?path=docs/a.md")))
        .await
        .json();
    assert_eq!(read["raw"], "# B\n");
    assert_eq!(read["history"][0]["subject"], "docs: 直した");
    assert_eq!(read["history"].as_array().expect("history").len(), 2);

    // 根を書かないパスも同じページを指す（`docs/` を前に付ける）。
    let same = send(&app, g(&format!("{page}?path=a.md"))).await.json();
    assert_eq!(same["path"], "docs/a.md");

    // 境界: `..` は 403、`.md` 以外は 422、空は 400。
    assert_problem(
        &send(
            &app,
            pu(&page, &json!({"path": "../escape.md", "body": "x"})),
        )
        .await,
        403,
        "path_forbidden",
    );
    assert_problem(
        &send(&app, pu(&page, &json!({"path": "docs/a.txt", "body": "x"}))).await,
        422,
        "validation",
    );
    assert_problem(
        &send(&app, pu(&page, &json!({"path": "", "body": "x"}))).await,
        400,
        "bad_request",
    );

    // 消す（etag 必須）。
    assert_problem(
        &send(&app, d(&format!("{page}?path=docs/a.md"))).await,
        409,
        "etag_mismatch",
    );
    let deleted = send(&app, d(&format!("{page}?path=docs/a.md&etag={next}"))).await;
    assert_eq!(deleted.status.as_u16(), 200, "{}", deleted.text());
    assert_eq!(deleted.json()["deleted"], true);
    assert!(deleted.json()["etag"].is_null());
    assert!(!repo.join("docs/a.md").exists(), "作業ツリーからも消える");
    assert_problem(
        &send(&app, d(&format!("{page}?path=docs/a.md&etag={next}"))).await,
        404,
        "page_not_found",
    );
}

/// ADR-0043 D5 と同じ規則: 人が `main` を編集中なら 409（何も触らない）。
#[tokio::test]
async fn editing_while_the_default_branch_is_dirty_is_409() {
    let env = env();
    let app = env.router();
    let dir = tempfile::tempdir().expect("tempdir");
    let (project, repo) = project_with_repo(&app, dir.path(), "docs", None).await;
    std::fs::write(repo.join("docs/README.md"), b"human is editing\n").expect("write");

    let resp = send(
        &app,
        pu(
            &format!("/api/v1/projects/{project}/docs/page"),
            &json!({"path": "docs/a.md", "body": "# A\n"}),
        ),
    )
    .await;
    let problem = assert_problem(&resp, 409, "default_branch_busy");
    assert!(
        problem["detail"]
            .as_str()
            .is_some_and(|d| d.contains("main")),
        "{problem}"
    );
    assert!(!repo.join("docs/a.md").exists(), "何も触らない");
}

// ---------------------------------------------------------------------------
// 昇格（`POST /tasks/{id}/artifacts/promote`）
// ---------------------------------------------------------------------------

fn produced(env: &TestEnv, task: &Task, name: &str, body: &[u8]) {
    let dir = env.workspace(task).join("artifacts");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), body).expect("write");
    env.store
        .append_event(
            task.id,
            &Event::ArtifactProduced {
                run_id: ulid::Ulid::new().to_string(),
                artifact: ArtifactRef {
                    name: name.to_string(),
                    path: format!("artifacts/{name}"),
                    sha256: "0".repeat(64),
                    kind: "file".into(),
            declared: true,
                },
            },
        )
        .expect("append artifact");
}

/// 成果物をページにする。front matter に `tasks: [<id>]` が入り、タイムラインに逆リンクが出る。
#[tokio::test]
async fn an_artifact_can_be_promoted_to_a_page() {
    let env = env();
    let app = env.router();
    let dir = tempfile::tempdir().expect("tempdir");
    let (project, repo) = project_with_repo(&app, dir.path(), "docs", None).await;
    let project_id: task_core::ProjectId = project.parse().expect("project id");
    let mut task = new_task(TaskKind::Execute, Status::Done);
    task.project_id = Some(project_id);
    env.seed(&task);
    produced(
        &env,
        &task,
        "answer.md",
        "# 調査の答え\n\n本文\n".as_bytes(),
    );
    let promote = format!("/api/v1/tasks/{}/artifacts/promote", task.id);

    // 管理系。
    assert_problem(
        &send(
            &app,
            post_json(
                &promote,
                &json!({"name": "answer.md", "path": "docs/research/x.md"}),
            ),
        )
        .await,
        401,
        "unauthorized",
    );
    // 知らない成果物は 404。
    assert_problem(
        &send(
            &app,
            p(
                &promote,
                &json!({"name": "nope.md", "path": "docs/research/x.md"}),
            ),
        )
        .await,
        404,
        "artifact_not_found",
    );

    let resp = send(
        &app,
        p(
            &promote,
            &json!({"name": "answer.md", "path": "docs/research/x.md"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["path"], "docs/research/x.md");

    let page = send(
        &app,
        g(&format!(
            "/api/v1/projects/{project}/docs/page?path=docs/research/x.md"
        )),
    )
    .await
    .json();
    assert_eq!(
        page["title"], "調査の答え",
        "front matter の title は中身から"
    );
    assert_eq!(page["tasks"][0], task.id.to_string());
    let raw = page["raw"].as_str().expect("raw");
    assert!(raw.starts_with("---\ntitle: 調査の答え\n"), "{raw}");
    assert!(raw.contains(&format!("tasks: [{}]", task.id)), "{raw}");
    assert!(raw.ends_with("# 調査の答え\n\n本文\n"), "{raw}");
    assert!(
        repo.join("docs/research/x.md").is_file(),
        "人の作業ツリーにも出る"
    );

    // 同じ宛先は 409。`overwrite: true` なら上書きできる。
    assert_problem(
        &send(
            &app,
            p(
                &promote,
                &json!({"name": "answer.md", "path": "docs/research/x.md"}),
            ),
        )
        .await,
        409,
        "page_exists",
    );
    produced(&env, &task, "answer.md", "# 新しい答え\n".as_bytes());
    let again = send(
        &app,
        p(
            &promote,
            &json!({"name": "answer.md", "path": "docs/research/x.md", "title": "決定版", "overwrite": true}),
        ),
    )
    .await;
    assert_eq!(again.status.as_u16(), 200, "{}", again.text());
    let page = send(
        &app,
        g(&format!(
            "/api/v1/projects/{project}/docs/page?path=docs/research/x.md"
        )),
    )
    .await
    .json();
    assert_eq!(page["title"], "決定版");
    assert_eq!(
        page["tasks"].as_array().expect("tasks").len(),
        1,
        "同じタスクは 1 回だけ"
    );
    assert_eq!(page["history"].as_array().expect("history").len(), 2);

    // 逆リンク（front matter の `tasks:`）。
    let timeline = send(&app, g(&format!("/api/v1/tasks/{}/timeline", task.id)))
        .await
        .json();
    let docs: Vec<&Value> = timeline["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|i| i["kind"] == "doc")
        .collect();
    assert_eq!(docs.len(), 1, "{timeline}");
    assert_eq!(docs[0]["path"], "docs/research/x.md");

    // 宛先の境界は `PUT` と同じ。
    assert_problem(
        &send(
            &app,
            p(&promote, &json!({"name": "answer.md", "path": "../x.md"})),
        )
        .await,
        403,
        "path_forbidden",
    );
    assert_problem(
        &send(
            &app,
            p(
                &promote,
                &json!({"name": "answer.md", "path": "docs/x.txt"}),
            ),
        )
        .await,
        422,
        "validation",
    );
}

/// 案件に属さないタスクの昇格は 409（文書の置き場が無い）。知らないタスクは 404。
#[tokio::test]
async fn promoting_needs_a_project() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Done);
    env.seed(&task);
    produced(&env, &task, "answer.md", b"# x\n");
    assert_problem(
        &send(
            &app,
            p(
                &format!("/api/v1/tasks/{}/artifacts/promote", task.id),
                &json!({"name": "answer.md", "path": "docs/x.md"}),
            ),
        )
        .await,
        409,
        "docs_unavailable",
    );
    assert_problem(
        &send(
            &app,
            p(
                &format!(
                    "/api/v1/tasks/{}/artifacts/promote",
                    task_core::TaskId::new()
                ),
                &json!({"name": "answer.md", "path": "docs/x.md"}),
            ),
        )
        .await,
        404,
        "task_not_found",
    );
}

/// 無い案件は 404（文書の API も他と同じ）。
#[tokio::test]
async fn an_unknown_project_is_404() {
    let env = env();
    let app = env.router();
    let id = task_core::ProjectId::new();
    assert_problem(
        &send(&app, g(&format!("/api/v1/projects/{id}/docs"))).await,
        404,
        "project_not_found",
    );
    assert_problem(
        &send(
            &app,
            p(&format!("/api/v1/projects/{id}/docs/init"), &json!({})),
        )
        .await,
        404,
        "project_not_found",
    );
}
