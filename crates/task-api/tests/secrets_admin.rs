//! ADR-0030（Phase 20）: `GET/PUT/DELETE /secrets...`。値が応答にもログにも出ないこと、`used_by` の導出、
//! 401（トークン無し。loopback でも）、409（`[secrets]` 未設定）、404（無い id）、422（空/空白の値）を確認する。

mod common;

use std::collections::HashMap;

use common::*;
use serde_json::json;
use task_api::SecretUse;

fn auth() -> String {
    format!("Bearer {TOKEN}")
}

fn env_with_secrets_dir() -> (TestEnv, tempfile::TempDir, std::path::PathBuf) {
    let secrets_tmp = tempfile::tempdir().expect("tempdir");
    let dir = secrets_tmp.path().join("secrets");
    std::fs::create_dir_all(&dir).unwrap();
    let mut secret_usage = HashMap::new();
    secret_usage.insert(
        "tavily".to_string(),
        vec![
            SecretUse { scope: "adapter".into(), name: "local-deep-research".into(), env: "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY".into() },
            SecretUse { scope: "provider".into(), name: "ldr-tavily".into(), env: "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY".into() },
        ],
    );
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        secrets_dir: Some(dir.clone()),
        secret_usage,
        ..Default::default()
    });
    (env, secrets_tmp, dir)
}

#[tokio::test]
async fn all_three_endpoints_require_a_token_even_on_loopback() {
    let (env, _tmp, _dir) = env_with_secrets_dir();
    let app = env.router();

    let resp = send(&app, get("/api/v1/secrets")).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": "tvly-abc"}), &[])).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, delete_with("/api/v1/secrets/tavily", &[])).await;
    assert_problem(&resp, 401, "unauthorized");
}

/// ADR-0017 M3 の規律は「`token_file` が**無くても**管理系は 401」。上のテストはトークンを設定した
/// 構成なので、`token_file` 未設定（loopback だけの構成）の側も確かめる（`accounts_admin.rs` と同じ形）。
#[tokio::test]
async fn all_three_endpoints_require_a_token_when_token_file_is_not_configured() {
    let secrets_tmp = tempfile::tempdir().expect("tempdir");
    let dir = secrets_tmp.path().join("secrets");
    std::fs::create_dir_all(&dir).unwrap();
    let env = TestEnv::with(EnvOptions { token: None, secrets_dir: Some(dir), ..Default::default() });
    let app = env.router();

    let resp = send(&app, get("/api/v1/secrets")).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": "tvly-abc"}), &[])).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, delete_with("/api/v1/secrets/tavily", &[])).await;
    assert_problem(&resp, 401, "unauthorized");
}

/// 監査指摘 D-6: 型違いの本文でも値のリテラルを応答に反射させない（`read_json` の 400 は serde の
/// エラー文をそのまま返すので、`PUT /secrets/{id}` だけは本文を見ないメッセージに差し替えている）。
#[tokio::test]
async fn put_does_not_reflect_the_value_in_a_parse_error() {
    let (env, _tmp, dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();

    for body in [json!({"value": 1234567890123u64}), json!({"value": ["tvly-leak-me"]}), json!({"walue": "x"})] {
        let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &body, &[("authorization", &auth)])).await;
        assert_problem(&resp, 400, "bad_request");
        let text = resp.text();
        assert!(!text.contains("1234567890123"), "value leaked in parse error: {text}");
        assert!(!text.contains("tvly-leak-me"), "value leaked in parse error: {text}");
    }
    assert!(!dir.join("tavily").exists(), "a rejected PUT must not write a file");
}

#[tokio::test]
async fn all_three_endpoints_are_409_when_secrets_is_not_configured() {
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    });
    let app = env.router();
    let auth = auth();

    let resp = send(&app, get_with("/api/v1/secrets", &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "secrets_unavailable");

    let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": "tvly-abc"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "secrets_unavailable");

    let resp = send(&app, delete_with("/api/v1/secrets/tavily", &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "secrets_unavailable");
}

#[tokio::test]
async fn put_creates_a_0600_file_and_never_returns_the_value() {
    let (env, _tmp, dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();

    let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": "tvly-super-secret"}), &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["id"], json!("tavily"));
    assert!(body["updated_at"].is_string());
    assert_eq!(body["fingerprint"].as_str().expect("fingerprint").len(), 8);
    assert!(!resp.text().contains("tvly-super-secret"), "value leaked in PUT response: {}", resp.text());

    let path = dir.join("tavily");
    assert!(path.is_file());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "tvly-super-secret");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // 一時ファイルは残らない。
    let entries: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(entries, vec!["tavily".to_string()]);
}

#[tokio::test]
async fn put_replaces_an_existing_secret() {
    let (env, _tmp, dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();

    let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": "first"}), &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200);
    let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": "second"}), &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(std::fs::read_to_string(dir.join("tavily")).unwrap(), "second");
}

#[tokio::test]
async fn put_rejects_empty_or_whitespace_only_values() {
    let (env, _tmp, dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();

    for value in ["", "   ", "\n\t "] {
        let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": value}), &[("authorization", &auth)])).await;
        assert_problem(&resp, 422, "validation");
    }
    assert!(!dir.join("tavily").exists(), "a rejected PUT must not write a file");
}

/// `id` はファイル名に使う。無効な形（パストラバーサル含む）は `PATCH/DELETE /providers/{id}` と同じ規約で
/// 404 `secret_not_found` にする（本文を見る前に判定する）。
#[tokio::test]
async fn put_and_delete_reject_invalid_ids_as_not_found() {
    let (env, secrets_tmp, dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();

    let victim = secrets_tmp.path().join("victim");
    std::fs::write(&victim, "should-not-move").unwrap();

    for traversal_id in ["..%2Fvictim", "..%2F..%2Fvictim"] {
        let path = format!("/api/v1/secrets/{traversal_id}");
        let resp = send(&app, put_json_with(&path, &json!({"value": "x"}), &[("authorization", &auth)])).await;
        assert_problem(&resp, 404, "secret_not_found");
        let resp = send(&app, delete_with(&path, &[("authorization", &auth)])).await;
        assert_problem(&resp, 404, "secret_not_found");
    }
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "should-not-move");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "secrets dir must stay empty");
}

#[tokio::test]
async fn delete_removes_the_file_and_second_delete_is_404() {
    let (env, _tmp, dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();
    std::fs::write(dir.join("tavily"), "v").unwrap();

    let resp = send(&app, delete_with("/api/v1/secrets/tavily", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json(), json!({}));
    assert!(!dir.join("tavily").exists());

    let resp = send(&app, delete_with("/api/v1/secrets/tavily", &[("authorization", &auth)])).await;
    assert_problem(&resp, 404, "secret_not_found");
}

#[tokio::test]
async fn get_lists_items_with_used_by_and_never_the_value() {
    let (env, _tmp, dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();

    // まだ値が無くても、設定（`env_from_secrets`）が参照している id は「未設定」として並ぶ
    // （GUI が「鍵を入れる場所」を出せるように。updated_at と fingerprint は null）。
    let resp = send(&app, get_with("/api/v1/secrets", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["dir"], json!(dir.display().to_string()));
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["id"], json!("tavily"));
    assert!(items[0]["updated_at"].is_null(), "{items:?}");
    assert!(items[0]["fingerprint"].is_null(), "{items:?}");
    assert_eq!(items[0]["used_by"].as_array().expect("used_by").len(), 2);

    // `tavily` は secret_usage に設定されている（adapter と provider の両方から使われる）。
    // `exa` は使われていない秘密として書く。
    std::fs::write(dir.join("tavily"), "tvly-abc\n").unwrap();
    std::fs::write(dir.join("exa"), "exa-xyz").unwrap();

    let resp = send(&app, get_with("/api/v1/secrets", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    let tavily = items.iter().find(|i| i["id"] == "tavily").expect("tavily item");
    assert!(tavily["updated_at"].is_string());
    assert_eq!(tavily["fingerprint"].as_str().expect("fingerprint").len(), 8);
    let used_by = tavily["used_by"].as_array().expect("used_by array");
    assert_eq!(used_by.len(), 2);
    assert!(used_by.iter().any(|u| u["scope"] == "adapter" && u["name"] == "local-deep-research" && u["env"] == "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"));
    assert!(used_by.iter().any(|u| u["scope"] == "provider" && u["name"] == "ldr-tavily"));

    let exa = items.iter().find(|i| i["id"] == "exa").expect("exa item");
    assert_eq!(exa["used_by"], json!([]), "exa is not referenced by any env_from_secrets in this test config");

    // 値はどこにも出ない。
    let text = resp.text();
    assert!(!text.contains("tvly-abc"), "value leaked in GET /secrets: {text}");
    assert!(!text.contains("exa-xyz"), "value leaked in GET /secrets: {text}");
}

/// `GET /config` にも秘密の値は出ない（`env_from_secrets` の値は秘密 id への参照であって値そのものではないが、
/// 念のためレスポンス全体に PUT した値が現れないことを確認する）。
#[tokio::test]
async fn secret_values_never_appear_in_config_or_secrets_responses() {
    let (env, _tmp, _dir) = env_with_secrets_dir();
    let app = env.router();
    let auth = auth();

    let resp = send(&app, put_json_with("/api/v1/secrets/tavily", &json!({"value": "tvly-should-never-leak"}), &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200);

    let resp = send(&app, get_with("/api/v1/secrets", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200);
    assert!(!resp.text().contains("tvly-should-never-leak"));

    let resp = send(&app, get_with("/api/v1/config", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(!resp.text().contains("tvly-should-never-leak"));
}
