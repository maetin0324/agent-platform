//! ADR-0033 D1/D2（Phase 23）: `GET /org`・`POST/PATCH/DELETE /org`・`/projects`・`/milestones`。
//!
//! 見るもの: 正常系、組織の編集が管理系であること（トークンあり・`token_file` 未設定の**両方**で 401）、
//! 404、409（使用中の削除・重複 id）、422（組織の検証違反）、`POST /tasks` が
//! `project_id` / `milestone_id` / `assignee` を任意で受けること。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{GenreSpec, RoleSpec, Status, TaskKind, TaskStore, Tier};

fn auth() -> [(&'static str, String); 1] {
    [("authorization", format!("Bearer {TOKEN}"))]
}

fn headers<'a>(pairs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    pairs.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// トークンを設定した構成では**全ての**要求にトークンが要る（共通ガード）。管理系かどうかとは別の話なので、
/// 下のテストは token 付きの要求をこの小道具で組む。
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

fn pa(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    patch_json_with(
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

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        roles: vec![
            RoleSpec {
                id: "literature-reader".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("paperqa".into()),
                max_turns: Some(5),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "implementer".into(),
                tier: Some(Tier::Standard),
                ..RoleSpec::default()
            },
        ],
        genres: vec![
            GenreSpec {
                id: "literature".into(),
                description: "関連研究の調査".into(),
                default_role: Some("literature-reader".into()),
                roles: vec!["literature-reader".into()],
                ..GenreSpec::default()
            },
            GenreSpec {
                id: "coding".into(),
                description: "コードを書く".into(),
                default_role: Some("implementer".into()),
                roles: vec!["implementer".into()],
                ..GenreSpec::default()
            },
        ],
        ..Default::default()
    })
}

/// 秘書 → 研究部 → 関連研究調査課 を API から作る。
async fn seed_org(app: &axum::Router) {
    let a = auth();
    let h = headers(&a);
    for body in [
        json!({"id": "secretary", "name": "秘書", "kind": "secretary", "brief": "案件を受け取る"}),
        json!({"id": "research", "name": "研究部", "kind": "department", "parent_id": "secretary"}),
        json!({"id": "research-survey", "name": "関連研究調査課", "kind": "section",
               "parent_id": "research", "genre": "literature", "position": 3}),
    ] {
        let resp = send(app, post_json_with("/api/v1/org", &body, &h)).await;
        assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    }
}

#[tokio::test]
async fn org_can_be_created_listed_patched_and_deleted() {
    let env = env_with_token();
    let app = env.router();

    // 最初は空。
    let resp = send(&app, g("/api/v1/org")).await;
    assert_eq!(resp.status.as_u16(), 200);
    assert_eq!(resp.json()["items"], json!([]));

    seed_org(&app).await;
    let resp = send(&app, g("/api/v1/org")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 3);
    // position 昇順（同値は id 昇順）。木は GUI が parent_id で組む。
    let ids: Vec<&str> = items
        .iter()
        .map(|n| n["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, vec!["research", "secretary", "research-survey"]);
    let section = &items[2];
    assert_eq!(section["kind"], "section");
    assert_eq!(section["parent_id"], "research");
    assert_eq!(section["genre"], "literature");

    // Location が付く。
    let resp = send(
        &app,
        p(
            "/api/v1/org",
            &json!({"id": "coding", "name": "コーディング部", "kind": "department", "parent_id": "secretary"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201);
    assert_eq!(resp.header("location"), Some("/api/v1/org/coding"));

    // PATCH は書いた項目だけを変える。`genre: null` は「分野なし」。
    let resp = send(
        &app,
        pa(
            "/api/v1/org/research-survey",
            &json!({"name": "文献調査課"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let node = resp.json();
    assert_eq!(node["name"], "文献調査課");
    assert_eq!(
        node["genre"], "literature",
        "an omitted field keeps its value"
    );
    assert_eq!(node["position"], 3);

    let resp = send(
        &app,
        pa("/api/v1/org/research-survey", &json!({"genre": null})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200);
    assert!(
        resp.json().get("genre").is_none(),
        "null clears the genre: {}",
        resp.text()
    );

    // DELETE は 204、消えたら 404。
    let resp = send(&app, d("/api/v1/org/research-survey")).await;
    assert_eq!(resp.status.as_u16(), 204, "{}", resp.text());
    let resp = send(&app, d("/api/v1/org/research-survey")).await;
    assert_problem(&resp, 404, "org_node_not_found");
    let resp = send(&app, pa("/api/v1/org/ghost", &json!({"name": "x"}))).await;
    assert_problem(&resp, 404, "org_node_not_found");
}

/// ADR-0033 D1: 使用中（未終了のタスクを抱えている / 子を持つ）ノードの削除は 409。
#[tokio::test]
async fn deleting_a_node_that_still_has_work_is_a_conflict() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let mut task = new_task(TaskKind::Execute, Status::Ready);
    task.assignee = Some("research-survey".into());
    env.seed(&task);

    let resp = send(&app, d("/api/v1/org/research-survey")).await;
    let problem = assert_problem(&resp, 409, "org_node_in_use");
    assert!(
        problem["detail"]
            .as_str()
            .expect("detail")
            .contains("research-survey"),
        "{problem}"
    );

    // 子を持つ部も消せない。
    let resp = send(&app, d("/api/v1/org/research")).await;
    assert_problem(&resp, 409, "org_node_in_use");

    // タスクが終端になれば消せる。
    env.store
        .apply_transition(task.id, task_core::Trigger::Cancel, None)
        .expect("cancel");
    let resp = send(&app, d("/api/v1/org/research-survey")).await;
    assert_eq!(resp.status.as_u16(), 204, "{}", resp.text());
}

/// 秘書は 1 つだけ・親は既存・id の形・同じ id の再作成（409）。
#[tokio::test]
async fn org_validation_is_reported_as_422_and_duplicate_ids_as_409() {
    let env = env_with_token();
    let app = env.router();
    let a = auth();
    let h = headers(&a);
    seed_org(&app).await;

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/org",
            &json!({"id": "boss", "name": "二人目", "kind": "secretary"}),
            &h,
        ),
    )
    .await;
    let problem = assert_problem(&resp, 422, "validation");
    assert!(
        problem.to_string().contains("only one secretary"),
        "{problem}"
    );

    let resp = send(
        &app,
        p(
            "/api/v1/org",
            &json!({"id": "lost", "name": "迷子", "kind": "section", "parent_id": "nobody"}),
        ),
    )
    .await;
    assert_problem(&resp, 422, "validation");

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/org",
            &json!({"id": "UPPER", "name": "x", "kind": "department", "parent_id": "secretary"}),
            &h,
        ),
    )
    .await;
    assert_problem(&resp, 422, "validation");

    // 部の下に部は置けない（secretary > department > section）。
    let resp = send(
        &app,
        post_json_with(
            "/api/v1/org",
            &json!({"id": "sub", "name": "課の下", "kind": "department", "parent_id": "research"}),
            &h,
        ),
    )
    .await;
    assert_problem(&resp, 422, "validation");

    // 監査 L-1: `genre` は `[[genres]]` にあるものだけ（POST も PATCH も 422）。
    let resp = send(
        &app,
        post_json_with(
            "/api/v1/org",
            &json!({"id": "ghost-genre", "name": "?", "kind": "section", "parent_id": "research", "genre": "bogus"}),
            &h,
        ),
    )
    .await;
    let problem = assert_problem(&resp, 422, "validation");
    assert!(problem.to_string().contains("unknown genre"), "{problem}");
    let resp = send(
        &app,
        pa("/api/v1/org/research-survey", &json!({"genre": "bogus"})),
    )
    .await;
    assert_problem(&resp, 422, "validation");
    // 設定にある分野は通る。
    let resp = send(
        &app,
        pa("/api/v1/org/research-survey", &json!({"genre": "coding"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());

    // 既にある id は 409（更新は PATCH）。
    let resp = send(
        &app,
        post_json_with("/api/v1/org", &json!({"id": "research", "name": "また研究部", "kind": "department", "parent_id": "secretary"}), &h),
    )
    .await;
    assert_problem(&resp, 409, "org_node_exists");
    // 失敗しても既存は壊れない。
    let resp = send(&app, g("/api/v1/org")).await;
    assert_eq!(resp.json()["items"].as_array().expect("items").len(), 3);
}

/// 管理系（POST/PATCH/DELETE /org）はトークンを設定した構成でも 401 になる（無いトークン）。
#[tokio::test]
async fn org_admin_endpoints_require_a_token() {
    let env = env_with_token();
    let app = env.router();

    let body = json!({"id": "secretary", "name": "秘書", "kind": "secretary"});
    assert_problem(
        &send(&app, post_json_with("/api/v1/org", &body, &[])).await,
        401,
        "unauthorized",
    );
    assert_problem(
        &send(
            &app,
            patch_json_with("/api/v1/org/secretary", &json!({"name": "x"}), &[]),
        )
        .await,
        401,
        "unauthorized",
    );
    assert_problem(
        &send(&app, delete_with("/api/v1/org/secretary", &[])).await,
        401,
        "unauthorized",
    );
    // 監査 M-4: 案件の作成も管理系（トークンありの構成でも、トークン無しの要求は 401）。
    assert_problem(
        &send(
            &app,
            post_json_with(
                "/api/v1/projects",
                &json!({"title": "t", "request": "r"}),
                &[],
            ),
        )
        .await,
        401,
        "unauthorized",
    );
    // 同じ構成でも、トークンを出せば通る。
    let a = auth();
    let h = headers(&a);
    assert_eq!(
        send(&app, get_with("/api/v1/org", &h))
            .await
            .status
            .as_u16(),
        200
    );
}

/// ADR-0017 D1 の規律: `token_file` 未設定（loopback 限定）の構成でも管理系は 401。
#[tokio::test]
async fn org_admin_endpoints_require_a_token_when_token_file_is_not_configured() {
    let env = TestEnv::with(EnvOptions {
        token: None,
        ..Default::default()
    });
    let app = env.router();

    let body = json!({"id": "secretary", "name": "秘書", "kind": "secretary"});
    assert_problem(
        &send(&app, post_json_with("/api/v1/org", &body, &[])).await,
        401,
        "unauthorized",
    );
    assert_problem(
        &send(
            &app,
            patch_json_with("/api/v1/org/secretary", &json!({"name": "x"}), &[]),
        )
        .await,
        401,
        "unauthorized",
    );
    assert_problem(
        &send(&app, delete_with("/api/v1/org/secretary", &[])).await,
        401,
        "unauthorized",
    );
    // 監査 M-4: 案件の作成も管理系（直後に秘書の run を起こすため）。
    assert_problem(
        &send(
            &app,
            post_json("/api/v1/projects", &json!({"title": "t", "request": "r"})),
        )
        .await,
        401,
        "unauthorized",
    );
    // 同じ構成でも読み取りは通る（管理系ではない）。
    assert_eq!(send(&app, get("/api/v1/org")).await.status.as_u16(), 200);
    assert_eq!(
        send(&app, get("/api/v1/projects")).await.status.as_u16(),
        200
    );
}

#[tokio::test]
async fn projects_and_milestones_round_trip_through_the_api() {
    let env = env_with_token();
    let app = env.router();

    let resp = send(&app, g("/api/v1/projects")).await;
    assert_eq!(resp.json()["items"], json!([]));

    let resp = send(
        &app,
        p(
            "/api/v1/projects",
            &json!({"title": "Pluvio の新テーマ", "request": "Pluvio を基盤に用いた新たな研究テーマの模索、検証"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    let project = resp.json();
    let project_id = project["id"].as_str().expect("id").to_string();
    assert_eq!(
        project["status"], "proposed",
        "a new project waits for the human"
    );
    assert_eq!(
        resp.header("location"),
        Some(format!("/api/v1/projects/{project_id}").as_str())
    );

    // 空の title / request は 422。
    let resp = send(
        &app,
        p("/api/v1/projects", &json!({"title": "  ", "request": "r"})),
    )
    .await;
    assert_problem(&resp, 422, "validation");

    // 途中目標。`seq` は案件ごとの通し番号。
    let first = send(
        &app,
        p(
            &format!("/api/v1/projects/{project_id}/milestones"),
            &json!({"title": "関連研究を棚卸し"}),
        ),
    )
    .await;
    assert_eq!(first.status.as_u16(), 201, "{}", first.text());
    assert_eq!(first.json()["seq"], 1);
    assert_eq!(first.json()["status"], "proposed");
    let second = send(
        &app,
        p(
            &format!("/api/v1/projects/{project_id}/milestones"),
            &json!({"title": "小さな検証", "description": "1 日で回る規模", "status": "approved"}),
        ),
    )
    .await;
    assert_eq!(second.json()["seq"], 2);
    assert_eq!(second.json()["status"], "approved");
    let milestone_id = second.json()["id"].as_str().expect("id").to_string();

    // 案件の状態変更と、途中目標の状態変更（SPEC §7 のアジャイル）。
    let resp = send(
        &app,
        pa(
            &format!("/api/v1/projects/{project_id}"),
            &json!({"status": "active"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["status"], "active");
    let resp = send(
        &app,
        pa(
            &format!("/api/v1/milestones/{milestone_id}"),
            &json!({"status": "reached"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["status"], "reached");

    // 未知の状態は 400（本文の解析で落ちる）。
    let resp = send(
        &app,
        pa(
            &format!("/api/v1/projects/{project_id}"),
            &json!({"status": "bogus"}),
        ),
    )
    .await;
    assert_problem(&resp, 400, "bad_request");

    // 404: 無い案件・無い途中目標・ULID でない id。
    let missing = "01J9ZX5T3K8Q7W6V5R4P3N2M1H";
    assert_problem(
        &send(&app, g(&format!("/api/v1/projects/{missing}"))).await,
        404,
        "project_not_found",
    );
    assert_problem(
        &send(&app, g("/api/v1/projects/not-a-ulid")).await,
        404,
        "project_not_found",
    );
    assert_problem(
        &send(
            &app,
            p(
                &format!("/api/v1/projects/{missing}/milestones"),
                &json!({"title": "x"}),
            ),
        )
        .await,
        404,
        "project_not_found",
    );
    assert_problem(
        &send(
            &app,
            pa(
                &format!("/api/v1/milestones/{missing}"),
                &json!({"status": "reached"}),
            ),
        )
        .await,
        404,
        "milestone_not_found",
    );
}

/// ADR-0039 D1（Phase 43）: 案件の作業場所を `POST` / `PATCH` で受け、`GET` で返す。
/// `[[clusters]]` に無いクラスタは 422、`Local` の `~` は celeris の `$HOME` で展開して保存する。
#[tokio::test]
async fn a_project_can_carry_the_workspace_where_its_code_lives() {
    let env = env_with_token();
    let app = env.router();

    // 1. 作るときに指定できる（`Remote`）。
    let resp = send(
        &app,
        p(
            "/api/v1/projects",
            &json!({
                "title": "Pluvio の PoC",
                "request": "DPU オフロードの PoC",
                "workspace": {"kind": "remote", "cluster": "pegasus", "path": "/work/NBB/rmaeda/workspace/rust/benchfs"}
            }),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    let project_id = resp.json()["id"].as_str().expect("id").to_string();
    assert_eq!(
        resp.json()["workspace"],
        json!({"kind": "remote", "cluster": "pegasus", "path": "/work/NBB/rmaeda/workspace/rust/benchfs"})
    );

    // `GET /projects/{id}` にも出る。
    let resp = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["project"]["workspace"]["cluster"], "pegasus");

    // 2. 後から `PATCH` で差し替えられる（`status` は省略できる）。`~` は展開される。
    let home = std::env::var("HOME").expect("HOME");
    let resp = send(
        &app,
        pa(
            &format!("/api/v1/projects/{project_id}"),
            &json!({"workspace": {"kind": "local", "path": "~/workspace/rust/pluvio-poc"}}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(
        resp.json()["workspace"],
        json!({"kind": "local", "path": format!("{home}/workspace/rust/pluvio-poc")})
    );

    // 3. `null` で消せる。作業場所を決めていない案件には `workspace` が出ない。
    let resp = send(
        &app,
        pa(
            &format!("/api/v1/projects/{project_id}"),
            &json!({"workspace": null}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert!(resp.json().get("workspace").is_none(), "{}", resp.text());

    // 4. `[[clusters]]` に無いクラスタは 422（POST も PATCH も）。
    assert_problem(
        &send(
            &app,
            p(
                "/api/v1/projects",
                &json!({"title": "t", "request": "r", "workspace": {"kind": "remote", "cluster": "nope", "path": "/x"}}),
            ),
        )
        .await,
        422,
        "validation",
    );
    assert_problem(
        &send(
            &app,
            pa(
                &format!("/api/v1/projects/{project_id}"),
                &json!({"workspace": {"kind": "remote", "cluster": "nope", "path": "/x"}}),
            ),
        )
        .await,
        422,
        "validation",
    );

    // 5. 何も書かない PATCH は 422（これまでは `status` 必須だった）。
    assert_problem(
        &send(
            &app,
            pa(&format!("/api/v1/projects/{project_id}"), &json!({})),
        )
        .await,
        422,
        "validation",
    );
    // 作業場所を書かない案件は従来どおり（`workspace` は出ない）。
    let resp = send(
        &app,
        p("/api/v1/projects", &json!({"title": "t", "request": "r"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    assert!(resp.json().get("workspace").is_none(), "{}", resp.text());
}

/// ADR-0041 D1（Phase 49）: `kind = local` の作業場所は `mode`（`"worktree"` 既定 / `"shared"`）を持てる。
/// 省略したら応答にも出ない（Phase 48 までと 1 バイトも変わらない）。
#[tokio::test]
async fn a_local_project_workspace_can_choose_the_worktree_mode() {
    let env = env_with_token();
    let app = env.router();

    // 1. 省略すると保存も応答も従来どおり（既定は `worktree`）。
    let resp = send(
        &app,
        p(
            "/api/v1/projects",
            &json!({"title": "t", "request": "r", "workspace": {"kind": "local", "path": "/srv/repo"}}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    let project_id = resp.json()["id"].as_str().expect("id").to_string();
    assert_eq!(
        resp.json()["workspace"],
        json!({"kind": "local", "path": "/srv/repo"})
    );

    // 2. `mode` を書けばそのまま往復する。
    let resp = send(
        &app,
        pa(
            &format!("/api/v1/projects/{project_id}"),
            &json!({"workspace": {"kind": "local", "path": "/srv/repo", "mode": "shared"}}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(
        resp.json()["workspace"],
        json!({"kind": "local", "path": "/srv/repo", "mode": "shared"})
    );
    let resp = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await;
    assert_eq!(resp.json()["project"]["workspace"]["mode"], "shared");

    let resp = send(
        &app,
        pa(
            &format!("/api/v1/projects/{project_id}"),
            &json!({"workspace": {"kind": "local", "path": "/srv/repo", "mode": "worktree"}}),
        ),
    )
    .await;
    assert_eq!(
        resp.json()["workspace"]["mode"],
        "worktree",
        "{}",
        resp.text()
    );

    // 3. 知らない `mode` は受け付けない。
    let resp = send(
        &app,
        p(
            "/api/v1/projects",
            &json!({"title": "t", "request": "r", "workspace": {"kind": "local", "path": "/srv/repo", "mode": "bogus"}}),
        ),
    )
    .await;
    assert!(
        resp.status.is_client_error(),
        "{} {}",
        resp.status,
        resp.text()
    );
}

/// `GET /projects/{id}` は案件 + 途中目標 + 仕事の木（その案件のタスクだけ）を返す。
#[tokio::test]
async fn project_detail_returns_the_milestones_and_the_work_tree() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let project: Value = send(
        &app,
        p("/api/v1/projects", &json!({"title": "t", "request": "r"})),
    )
    .await
    .json();
    let project_id = project["id"].as_str().expect("id").to_string();
    let milestone: Value = send(
        &app,
        p(
            &format!("/api/v1/projects/{project_id}/milestones"),
            &json!({"title": "m1"}),
        ),
    )
    .await
    .json();
    let milestone_id = milestone["id"].as_str().expect("id").to_string();

    // ADR-0033 D2: `POST /tasks` は project_id / milestone_id / assignee を任意で受ける。
    let parent: Value = send(
        &app,
        p(
            "/api/v1/tasks",
            &json!({
                "title": "調べる", "objective": "関連研究を洗う",
                "acceptance": [{"type": "human", "text": "読んだ"}],
                "project_id": project_id, "milestone_id": milestone_id, "assignee": "research-survey",
            }),
        ),
    )
    .await
    .json();
    assert_eq!(parent["project_id"], Value::String(project_id.clone()));
    assert_eq!(parent["assignee"], "research-survey");
    // assignee のノードの分野 → default_role → 役割の既定が効く（解決は決定的）。
    assert_eq!(parent["genre"], "literature");
    assert_eq!(parent["worker_hint"]["tier"], "cheap");
    assert_eq!(parent["worker_hint"]["adapter"], "paperqa");
    assert_eq!(parent["budget"]["max_turns"], 5);

    let child: Value = send(
        &app,
        p(
            "/api/v1/tasks",
            &json!({
                "title": "まとめる", "objective": "報告を書く",
                "acceptance": [{"type": "human", "text": "読んだ"}],
                "project_id": project_id,
                "parent": parent["id"],
                "depends_on": [parent["id"]],
            }),
        ),
    )
    .await
    .json();

    // 案件に属さないタスクは仕事の木に出ない。
    let outsider = send(
        &app,
        p(
            "/api/v1/tasks",
            &json!({"title": "無関係", "objective": "o", "acceptance": [{"type": "human", "text": "x"}]}),
        ),
    )
    .await;
    assert_eq!(outsider.status.as_u16(), 201);

    // `GET /tasks?project=` も同じ絞り込み。Phase 24（ADR-0033 D4）から、案件を作った直後に
    // 秘書への対話用タスクが 1 件できるので、この案件のタスクは 2 + 1 件。
    let listed = send(&app, g(&format!("/api/v1/tasks?project={project_id}"))).await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text());
    assert_eq!(listed.json()["total"], 3);
    assert_problem(
        &send(&app, g("/api/v1/tasks?project=nope")).await,
        400,
        "bad_request",
    );

    let resp = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let detail = resp.json();
    assert_eq!(detail["project"]["id"], Value::String(project_id));
    assert_eq!(
        detail["milestones"].as_array().expect("milestones").len(),
        1
    );
    let tasks = detail["tasks"].as_array().cloned().expect("tasks");
    assert_eq!(tasks.len(), 3, "only the tasks of this project: {detail}");
    assert_eq!(
        tasks
            .iter()
            .filter(|t| t["title"].as_str().is_some_and(|t| t.starts_with("対話: ")))
            .count(),
        1,
        "Phase 24: 秘書への最初の対話が 1 件入る"
    );
    let child_view = tasks
        .iter()
        .find(|t| t["id"] == child["id"])
        .expect("child in the tree");
    assert_eq!(child_view["parent_id"], parent["id"]);
    assert_eq!(child_view["depends_on"], json!([parent["id"]]));
    // ADR-0044 D1（Phase 53）: `POST /tasks` で人が作ったタスクは `ready`。
    assert_eq!(child_view["status"], "ready");
    assert!(
        child_view.get("assignee").is_some_and(|v| v.is_null()),
        "{child_view}"
    );
    // GUI 監査 H4（Phase 29）: 人が見る本体の仕事には `support` が付かない。
    assert!(
        child_view.get("support").is_some_and(|v| v.is_null()),
        "{child_view}"
    );
    let parent_view = tasks
        .iter()
        .find(|t| t["id"] == parent["id"])
        .expect("parent in the tree");
    assert_eq!(parent_view["assignee"], "research-survey");
    assert_eq!(parent_view["milestone_id"], Value::String(milestone_id));
    // 秘書への対話用タスクは `support = "conversation"`。
    let conversation_view = tasks
        .iter()
        .find(|t| t["title"].as_str().is_some_and(|t| t.starts_with("対話: ")))
        .expect("conversation task in the tree");
    assert_eq!(conversation_view["support"], "conversation");
}

/// 知らない `assignee` / 無い案件を付けた `POST /tasks` は 422（作られない）。
#[tokio::test]
async fn posting_a_task_with_an_unknown_assignee_or_project_is_rejected() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let resp = send(
        &app,
        p(
            "/api/v1/tasks",
            &json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "x"}], "assignee": "nobody"}),
        ),
    )
    .await;
    assert_problem(&resp, 422, "validation");

    let resp = send(
        &app,
        p(
            "/api/v1/tasks",
            &json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "x"}],
                    "project_id": "01J9ZX5T3K8Q7W6V5R4P3N2M1H"}),
        ),
    )
    .await;
    assert_problem(&resp, 422, "validation");

    // 3 つとも省略した従来の本文はそのまま通る（互換）。
    let resp = send(
        &app,
        p("/api/v1/tasks", &json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "x"}]})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201);
    let task = resp.json();
    assert!(task.get("assignee").is_none(), "{task}");
    assert!(task.get("project_id").is_none(), "{task}");
}
