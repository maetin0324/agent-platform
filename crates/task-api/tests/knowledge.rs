//! ADR-0047（Phase 61）: 知識ベース（`/knowledge/tree`、`/knowledge/page`、`/knowledge/inbox`、
//! `/knowledge/inbox/{id}/{accept,reject}`）。
//!
//! 見るもの: 初期化していない KB（`initialized: false`、変更系は 409）、ツリーと `?scope=` / `?q=` の順位、
//! ページの描画（front matter・生 HTML を捨てる・履歴・etag）、`PUT`（409 etag / 403 `..` / 422 `.md` 以外 /
//! `_inbox` は 403）、候補の accept / reject（404・409 `page_exists`・git のコミット）、管理系の 401。
//!
//! **実ホームの `~/knowledge` には絶対に触らない**（`TestEnv` が tempdir の中に KB を作る）。
//! 外部ネットワークにも出ない（CLAUDE.md）。

mod common;

use common::*;
use serde_json::{Value, json};
use std::path::Path;

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(path, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(path, body, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn pu(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    put_json_with(path, body, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn env() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..EnvOptions::default()
    })
}

/// tempdir の中に KB を用意する（`celerisctl knowledge init` と同じ経路）。
fn item_paths(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["path"].as_str().unwrap_or("").to_string())
        .collect()
}

fn init_kb(root: &Path) {
    task_ops::knowledge::init(root).expect("knowledge init");
}

fn write_page(root: &Path, path: &str, body: &str) {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, body.as_bytes()).expect("write");
}

/// KB がまだ無いときは**何も作らず** `initialized: false`。変更系は 409 `knowledge_unavailable`。
#[tokio::test]
async fn an_uninitialized_knowledge_base_reads_empty_and_refuses_writes() {
    let env = env();
    let app = env.router();

    let tree = send(&app, g("/api/v1/knowledge/tree")).await;
    assert_eq!(tree.status.as_u16(), 200, "{}", tree.text());
    assert_eq!(tree.json()["initialized"], false);
    assert_eq!(tree.json()["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(tree.json()["inbox_count"], 0);

    let inbox = send(&app, g("/api/v1/knowledge/inbox")).await;
    assert_eq!(inbox.status.as_u16(), 200, "{}", inbox.text());
    assert_eq!(inbox.json()["initialized"], false);

    // 読み取りは**何も作らない**（ディレクトリすらできない）。
    assert!(!env.knowledge_root.exists(), "読み取りが KB を作ってはいけない");

    let put = send(
        &app,
        pu("/api/v1/knowledge/page", &json!({"path": "user/x.md", "body": "# x\n"})),
    )
    .await;
    assert_problem(&put, 409, "knowledge_unavailable");
    assert!(!env.knowledge_root.exists());
}

/// ツリーは索引を返し、`?scope=` と `?q=`（tag → title → 本文）で絞れる。
#[tokio::test]
async fn the_tree_lists_pages_and_filters_by_scope_and_query() {
    let env = env();
    init_kb(&env.knowledge_root);
    write_page(
        &env.knowledge_root,
        "environment/clusters/pegasus.md",
        "---\ntitle: pegasus の使い方\ntags: [hpc, cluster]\nscope: environment\nconfidence: high\n---\n\npjsub で投げる。\n",
    );
    write_page(
        &env.knowledge_root,
        "experience/2026/09/hpc.md",
        "---\ntitle: 計測の記録\nscope: experience\n---\n\npjsub の待ち行列が長い。\n",
    );
    task_ops::knowledge::reindex(&env.knowledge_root).expect("reindex");
    let app = env.router();

    let tree = send(&app, g("/api/v1/knowledge/tree")).await;
    assert_eq!(tree.status.as_u16(), 200, "{}", tree.text());
    let body = tree.json();
    assert_eq!(body["initialized"], true);
    assert_eq!(body["root"], env.knowledge_root.display().to_string());
    assert!(body["generated_at"].as_str().is_some_and(|s| s.contains('T')), "{body}");
    let paths = item_paths(&body);
    assert!(paths.iter().any(|p| p == "environment/clusters/pegasus.md"), "{paths:?}");
    assert!(paths.iter().any(|p| p == "user/profile.md"), "{paths:?}");
    // `_inbox` は索引に出ない。
    assert!(!paths.iter().any(|p| p.starts_with("_inbox/")), "{paths:?}");
    // 並びはパスの昇順。
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "{paths:?}");
    // 見出し（ディレクトリ）の一覧。
    let scopes: Vec<&str> = body["scopes"].as_array().expect("scopes").iter().filter_map(Value::as_str).collect();
    assert!(scopes.contains(&"environment/clusters"), "{scopes:?}");

    // `?scope=` で絞る（`init` の雛形の pegasus / sirius / fern03 がここに居る）。
    let scoped = send(&app, g("/api/v1/knowledge/tree?scope=environment/clusters")).await;
    let items = scoped.json()["items"].as_array().expect("items").clone();
    assert!(
        items.iter().all(|i| i["path"].as_str().unwrap_or("").starts_with("environment/clusters/")),
        "{}",
        scoped.text()
    );
    let pegasus = items
        .iter()
        .find(|i| i["path"] == "environment/clusters/pegasus.md")
        .unwrap_or_else(|| panic!("{}", scoped.text()));
    assert_eq!(pegasus["confidence"], "high");
    assert_eq!(pegasus["tags"][0], "hpc");
    assert_eq!(scoped.json()["scope"], "environment/clusters");
    // `user/` のページはこの scope には出ない。
    assert!(!items.iter().any(|i| i["path"] == "user/profile.md"), "{}", scoped.text());

    // `?q=`: 本文でしか当たらないページも拾う（全文一致）。
    let q = send(&app, g("/api/v1/knowledge/tree?q=pjsub")).await;
    let hits = item_paths(&q.json());
    assert!(hits.iter().any(|p| p == "environment/clusters/pegasus.md"), "{hits:?}");
    assert!(hits.iter().any(|p| p == "experience/2026/09/hpc.md"), "{hits:?}");
    // tag で当たるページだけが返り、当たらないページ（`experience`）は出ない。
    let cluster = send(&app, g("/api/v1/knowledge/tree?q=cluster")).await;
    let hits = item_paths(&cluster.json());
    assert!(hits.iter().any(|p| p == "environment/clusters/pegasus.md"), "{hits:?}");
    // tag で当たるページが先、本文でしか当たらないページ（README）は後ろ。
    assert!(
        hits[..3].iter().all(|p| p.starts_with("environment/clusters/")),
        "tag 一致が先: {hits:?}"
    );
    assert!(!hits.iter().any(|p| p.starts_with("experience/")), "{hits:?}");
}

/// ページは front matter を分けて描画し、履歴と etag を返す。生 HTML は捨てる。
#[tokio::test]
async fn a_page_is_rendered_with_its_front_matter_history_and_etag() {
    let env = env();
    init_kb(&env.knowledge_root);
    let app = env.router();

    // 人の編集（`PUT`）で 1 枚作る → 履歴 1 件。
    let created = send(
        &app,
        pu(
            "/api/v1/knowledge/page",
            &json!({
                "path": "environment/clusters/wisteria.md",
                "body": "---\ntitle: wisteria の使い方\ntags: [hpc]\nscope: environment\nsources: [human]\n---\n\n# pegasus\n\n<script>alert(1)</script>\n\n[t](celeris:task/01J1) と [[../../user/profile.md]]\n"
            }),
        ),
    )
    .await;
    assert_eq!(created.status.as_u16(), 200, "{}", created.text());
    assert_eq!(created.json()["unchanged"], false);
    let etag = created.json()["etag"].as_str().expect("etag").to_string();

    let page = send(&app, g("/api/v1/knowledge/page?path=environment/clusters/wisteria.md")).await;
    assert_eq!(page.status.as_u16(), 200, "{}", page.text());
    let body = page.json();
    assert_eq!(body["title"], "wisteria の使い方");
    assert_eq!(body["scope"], "environment");
    assert_eq!(body["tags"][0], "hpc");
    assert_eq!(body["sources"][0], "human");
    assert_eq!(body["etag"], etag);
    assert_eq!(body["too_large"], false);
    let html = body["html"].as_str().expect("html");
    assert!(!html.contains("<script"), "{html}");
    assert!(html.contains("href=\"/tasks/01J1\""), "{html}");
    assert!(html.contains("/knowledge?path=user/profile.md"), "{html}");
    assert!(body["raw"].as_str().is_some_and(|r| r.starts_with("---\n")), "{body}");
    let history = body["history"].as_array().expect("history");
    assert_eq!(history.len(), 1, "{body}");
    assert_eq!(history[0]["author"], "Celeris (human)");
    assert_eq!(history[0]["subject"], "knowledge: environment/clusters/wisteria.md");

    // 書いたら索引が作り直されている（`reindex` を別に呼ばなくてもツリーに出る）。
    let tree = send(&app, g("/api/v1/knowledge/tree?scope=environment")).await;
    let paths = item_paths(&tree.json());
    assert!(paths.iter().any(|p| p == "environment/clusters/wisteria.md"), "{paths:?}");

    // 無いページは 404。
    let missing = send(&app, g("/api/v1/knowledge/page?path=user/nope.md")).await;
    assert_problem(&missing, 404, "page_not_found");
}

/// `PUT` の境界: etag の 409、`..` の 403、`.md` 以外の 422、`_inbox` の 403、同じ中身の `unchanged`。
#[tokio::test]
async fn writing_a_page_checks_the_etag_and_the_path() {
    let env = env();
    init_kb(&env.knowledge_root);
    let app = env.router();

    let first = send(&app, pu("/api/v1/knowledge/page", &json!({"path": "user/notes.md", "body": "# メモ\n"}))).await;
    assert_eq!(first.status.as_u16(), 200, "{}", first.text());
    let etag = first.json()["etag"].as_str().expect("etag").to_string();

    // 既にあるのに etag 無しは 409（いまの値が載る）。
    let clash = send(&app, pu("/api/v1/knowledge/page", &json!({"path": "user/notes.md", "body": "# 別\n"}))).await;
    let problem = assert_problem(&clash, 409, "etag_mismatch");
    assert_eq!(problem["etag"], etag);

    // 正しい etag なら通る。同じ中身なら `unchanged`。
    let same = send(
        &app,
        pu("/api/v1/knowledge/page", &json!({"path": "user/notes.md", "body": "# メモ\n", "etag": etag})),
    )
    .await;
    assert_eq!(same.status.as_u16(), 200, "{}", same.text());
    assert_eq!(same.json()["unchanged"], true);

    // 根の外・`.md` 以外・`_inbox` は弾く。
    let escape = send(&app, pu("/api/v1/knowledge/page", &json!({"path": "../etc/passwd.md", "body": "x"}))).await;
    assert_problem(&escape, 403, "path_forbidden");
    let absolute = send(&app, pu("/api/v1/knowledge/page", &json!({"path": "/etc/passwd.md", "body": "x"}))).await;
    assert_problem(&absolute, 403, "path_forbidden");
    let not_md = send(&app, pu("/api/v1/knowledge/page", &json!({"path": "user/a.txt", "body": "x"}))).await;
    assert_problem(&not_md, 422, "validation");
    let inbox = send(&app, pu("/api/v1/knowledge/page", &json!({"path": "_inbox/x.md", "body": "x"}))).await;
    assert_problem(&inbox, 403, "path_forbidden");
    // 読み取りも同じ境界。
    assert_problem(&send(&app, g("/api/v1/knowledge/page?path=../x.md")).await, 403, "path_forbidden");
    assert_problem(&send(&app, g("/api/v1/knowledge/page?path=x.txt")).await, 422, "validation");
}

/// 候補（`_inbox`）の一覧と accept / reject。コミットは git に残る。
#[tokio::test]
async fn candidates_can_be_accepted_or_rejected() {
    let env = env();
    init_kb(&env.knowledge_root);
    // ワーカーが `celerisctl knowledge record` で入れた体（同じ関数を呼ぶ）。
    let one = task_ops::knowledge::record(
        &env.knowledge_root,
        &task_ops::knowledge::RecordRequest {
            title: "fern03 の使い方".into(),
            scope: "environment".into(),
            tags: vec!["server".into()],
            sources: vec!["task:01J1".into(), "url:https://example.com/x".into()],
            confidence: Some(task_core::Confidence::Medium),
            body: "ssh fern03 で入る。".into(),
            path: Some("environment/servers/fern03.md".into()),
        },
    )
    .expect("record");
    let app = env.router();

    let inbox = send(&app, g("/api/v1/knowledge/inbox")).await;
    assert_eq!(inbox.status.as_u16(), 200, "{}", inbox.text());
    let items = inbox.json()["items"].as_array().expect("items").clone();
    assert_eq!(items.len(), 1, "{}", inbox.text());
    assert_eq!(items[0]["id"], one.id);
    assert_eq!(items[0]["title"], "fern03 の使い方");
    assert_eq!(items[0]["target"], "environment/servers/fern03.md");
    assert_eq!(items[0]["target_exists"], false);
    assert_eq!(items[0]["confidence"], "medium");
    assert_eq!(items[0]["sources"][0], "task:01J1");
    assert!(items[0]["html"].as_str().is_some_and(|h| h.contains("ssh fern03")), "{}", inbox.text());
    // 候補はツリー（索引）には出ない。
    let tree = send(&app, g("/api/v1/knowledge/tree")).await;
    assert_eq!(tree.json()["inbox_count"], 1);
    let paths = item_paths(&tree.json());
    assert!(!paths.iter().any(|p| p.starts_with("_inbox/")), "{paths:?}");

    // 知らない id は 404、`/` を含む id は 403。
    assert_problem(
        &send(&app, p("/api/v1/knowledge/inbox/nope/accept", &json!({}))).await,
        404,
        "candidate_not_found",
    );
    // `%2F` はデコードされて `/` を含む id になるので、境界で 403 になる。
    assert_problem(
        &send(&app, p("/api/v1/knowledge/inbox/..%2F..%2Fetc/reject", &json!({}))).await,
        403,
        "path_forbidden",
    );

    // accept は正本に移してコミットする。
    let accepted = send(&app, p(&format!("/api/v1/knowledge/inbox/{}/accept", one.id), &json!({}))).await;
    assert_eq!(accepted.status.as_u16(), 200, "{}", accepted.text());
    assert_eq!(accepted.json()["path"], "environment/servers/fern03.md");
    assert!(accepted.json()["etag"].as_str().is_some());
    let page = send(&app, g("/api/v1/knowledge/page?path=environment/servers/fern03.md")).await;
    assert_eq!(page.status.as_u16(), 200, "{}", page.text());
    assert_eq!(page.json()["title"], "fern03 の使い方");
    // `_inbox` 専用の `path:` は落ちる。
    assert!(!page.json()["raw"].as_str().unwrap_or("").contains("\npath:"), "{}", page.text());
    assert_eq!(page.json()["history"].as_array().map(Vec::len), Some(1));
    assert_eq!(send(&app, g("/api/v1/knowledge/inbox")).await.json()["items"].as_array().map(Vec::len), Some(0));
    // 同じ id はもう無い。
    assert_problem(
        &send(&app, p(&format!("/api/v1/knowledge/inbox/{}/accept", one.id), &json!({}))).await,
        404,
        "candidate_not_found",
    );

    // 宛先が既にあれば 409（`overwrite: true` なら通る）。
    let two = task_ops::knowledge::record(
        &env.knowledge_root,
        &task_ops::knowledge::RecordRequest {
            title: "fern03 の使い方".into(),
            scope: "environment".into(),
            sources: vec!["human".into()],
            body: "別の版。".into(),
            path: Some("environment/servers/fern03.md".into()),
            ..task_ops::knowledge::RecordRequest::default()
        },
    )
    .expect("record");
    let listed = send(&app, g("/api/v1/knowledge/inbox")).await;
    assert_eq!(listed.json()["items"][0]["target_exists"], true);
    assert_problem(
        &send(&app, p(&format!("/api/v1/knowledge/inbox/{}/accept", two.id), &json!({}))).await,
        409,
        "page_exists",
    );
    let forced = send(
        &app,
        p(&format!("/api/v1/knowledge/inbox/{}/accept", two.id), &json!({"overwrite": true})),
    )
    .await;
    assert_eq!(forced.status.as_u16(), 200, "{}", forced.text());

    // reject は捨てる。
    let three = task_ops::knowledge::record(
        &env.knowledge_root,
        &task_ops::knowledge::RecordRequest {
            title: "捨てる候補".into(),
            scope: "user".into(),
            sources: vec!["human".into()],
            body: "いらない。".into(),
            ..task_ops::knowledge::RecordRequest::default()
        },
    )
    .expect("record");
    let rejected = send(&app, p(&format!("/api/v1/knowledge/inbox/{}/reject", three.id), &json!({}))).await;
    assert_eq!(rejected.status.as_u16(), 200, "{}", rejected.text());
    assert_eq!(rejected.json()["id"], three.id);
    assert!(rejected.json()["sha"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(!env.knowledge_root.join(&three.path).exists());
    assert_problem(
        &send(&app, p(&format!("/api/v1/knowledge/inbox/{}/reject", three.id), &json!({}))).await,
        404,
        "candidate_not_found",
    );
}

/// 変更系は管理系（ADR-0013 D11）。トークン無しは 401、読み取りは通る。
#[tokio::test]
async fn the_write_endpoints_are_admin_only() {
    let env = env();
    init_kb(&env.knowledge_root);
    let app = env.router();

    // 読み取りはトークン無しでも通る（`token_file` 設定時の共通規則に従う）。
    let anonymous_tree = send(&app, get("/api/v1/knowledge/tree")).await;
    assert_eq!(anonymous_tree.status.as_u16(), 401, "{}", anonymous_tree.text());

    // 変更系はトークンがあっても管理系のヘッダが要る（ここでは Bearer が管理系の資格）。
    let no_token = send(
        &app,
        post_json("/api/v1/knowledge/inbox/x/accept", &json!({})),
    )
    .await;
    assert_eq!(no_token.status.as_u16(), 401, "{}", no_token.text());
    let put_no_token = send(
        &app,
        put_json_with("/api/v1/knowledge/page", &json!({"path": "user/a.md", "body": "x"}), &[]),
    )
    .await;
    assert_eq!(put_no_token.status.as_u16(), 401, "{}", put_no_token.text());
}
