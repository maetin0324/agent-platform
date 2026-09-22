//! ADR-0059 D6（Phase 99）: `PUT /clusters/{id}/settings`（クラスタの作業ディレクトリの DB 上書き）。
//! `GET /clusters` に `work_dir` / `work_dir_source` が出ること、絶対パス・`~`/`~/…` だけ許す検証、
//! `null` で上書きを消すこと、未知の cluster id は 404、トークン無しは 401 を確認する。
//! `tests/common::config_view()` は `id = "pegasus"`（設定ファイルには `work_dir` を持たない）。

mod common;

use common::*;
use serde_json::json;

const CLUSTER: &str = "pegasus";

fn cluster_view<'a>(resp: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    resp["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("cluster {id} not in GET /clusters"))
}

#[tokio::test]
async fn get_clusters_has_no_work_dir_until_one_is_set() {
    let env = admin_env();
    let app = env.router();
    let resp = send(&app, get_admin("/api/v1/clusters")).await;
    assert_eq!(resp.status, http_status(200));
    let body = resp.json();
    let cluster = cluster_view(&body, CLUSTER);
    assert!(cluster["work_dir"].is_null(), "{cluster}");
    assert!(cluster["work_dir_source"].is_null(), "{cluster}");
}

#[tokio::test]
async fn put_settings_requires_a_token() {
    let env = admin_env();
    let app = env.router();
    let resp = send(
        &app,
        put_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/settings"),
            &json!({"work_dir": "/work/NBB/rmaeda"}),
            &[],
        ),
    )
    .await;
    assert_eq!(resp.status, http_status(401));
}

#[tokio::test]
async fn put_settings_on_an_unknown_cluster_is_404() {
    let env = admin_env();
    let app = env.router();
    let resp = send(
        &app,
        put_json_with(
            "/api/v1/clusters/no-such-cluster/settings",
            &json!({"work_dir": "/work/x"}),
            &admin_headers(),
        ),
    )
    .await;
    assert_eq!(resp.status, http_status(404));
}

#[tokio::test]
async fn put_settings_rejects_a_relative_path() {
    let env = admin_env();
    let app = env.router();
    let resp = send(
        &app,
        put_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/settings"),
            &json!({"work_dir": "relative/path"}),
            &admin_headers(),
        ),
    )
    .await;
    assert_eq!(resp.status, http_status(422));
    let body = resp.json();
    assert_eq!(body["errors"][0]["field"], "work_dir");
}

#[tokio::test]
async fn put_settings_accepts_absolute_and_tilde_paths_and_shows_up_in_get_clusters() {
    let env = admin_env();
    let app = env.router();

    // 絶対パス。
    let resp = send(
        &app,
        put_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/settings"),
            &json!({"work_dir": "/work/NBB/rmaeda"}),
            &admin_headers(),
        ),
    )
    .await;
    assert_eq!(resp.status, http_status(200), "{}", resp.text());
    let put_body = resp.json();
    assert_eq!(put_body["cluster_id"], CLUSTER);
    assert_eq!(put_body["work_dir"], "/work/NBB/rmaeda");

    let resp = send(&app, get_admin("/api/v1/clusters")).await;
    let body = resp.json();
    let cluster = cluster_view(&body, CLUSTER);
    assert_eq!(cluster["work_dir"], "/work/NBB/rmaeda");
    assert_eq!(cluster["work_dir_source"], "settings");

    // `~/…` も許可する（celeris は保存するだけで展開しない。ADR-0059 D2/D6）。
    let resp = send(
        &app,
        put_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/settings"),
            &json!({"work_dir": "~/work"}),
            &admin_headers(),
        ),
    )
    .await;
    assert_eq!(resp.status, http_status(200));

    // `null` で上書きを消す（設定ファイルには `work_dir` が無いので `null`/`null` に戻る）。
    let resp = send(
        &app,
        put_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/settings"),
            &json!({"work_dir": null}),
            &admin_headers(),
        ),
    )
    .await;
    assert_eq!(resp.status, http_status(200));
    let resp = send(&app, get_admin("/api/v1/clusters")).await;
    let body = resp.json();
    let cluster = cluster_view(&body, CLUSTER);
    assert!(cluster["work_dir"].is_null(), "{cluster}");
    assert!(cluster["work_dir_source"].is_null(), "{cluster}");
}

fn http_status(code: u16) -> axum::http::StatusCode {
    axum::http::StatusCode::from_u16(code).expect("valid status code")
}
