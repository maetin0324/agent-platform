//! ADR-0017（Phase 11）: `POST/PATCH/DELETE /providers...` のファイル書き込み部分（`reload`/`check` は taskd 側の
//! 実行が要るので `tests/e2e/tests/provider_admin_scenarios.rs` で実バイナリを使って検証する）。
//! ここでは task-api だけで完結する部分 — 認証・検証・409/404・**id のパストラバーサル防止**（監査で発見した穴の回帰テスト）
//! ・変更系すべてへの `Origin` 拒否（同じく監査で発見した穴の回帰テスト）を確認する。

mod common;

use common::*;
use serde_json::json;

/// `providers_dir` に使う一時ディレクトリを別に用意する（`TestEnv` 自身の一時ディレクトリとは独立。
/// 戻り値の `TempDir` を drop すると消えるので、呼び出し側で `TestEnv` と同じスコープに保持すること）。
fn env_with_providers_dir() -> (TestEnv, tempfile::TempDir, std::path::PathBuf) {
    let providers_tmp = tempfile::tempdir().expect("tempdir");
    let dir = providers_tmp.path().join("providers.d");
    std::fs::create_dir_all(&dir).unwrap();
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        providers_dir: Some(dir.clone()),
        ..Default::default()
    });
    (env, providers_tmp, dir)
}

fn auth() -> String {
    format!("Bearer {TOKEN}")
}

#[tokio::test]
async fn create_requires_token_validates_id_and_adapter_and_rejects_duplicates() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();

    // トークン無しは 401（loopback でも。ADR-0017 D1）。
    let resp = send(&app, post_json("/api/v1/providers", &json!({"id": "x", "adapter": "fake"}))).await;
    assert_problem(&resp, 401, "unauthorized");

    let auth = auth();
    // 不正な id / adapter は 400。
    let resp = send(&app, post_json_with("/api/v1/providers", &json!({"id": "../x", "adapter": "fake"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 400, "bad_request");
    let resp = send(&app, post_json_with("/api/v1/providers", &json!({"id": "x", "adapter": "bogus"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 400, "bad_request");

    // 正常作成。
    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "acct-b", "adapter": "fake", "env": {"K": "secret-value"}}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    assert_eq!(resp.header("location"), Some("/api/v1/providers/acct-b"));
    assert_eq!(resp.json()["env_keys"], json!(["K"]));
    assert!(resp.json().get("env").is_none());
    assert!(!resp.text().contains("secret-value"));
    assert!(dir.join("acct-b.toml").exists());

    // 重複 id は 409。
    let resp = send(&app, post_json_with("/api/v1/providers", &json!({"id": "acct-b", "adapter": "fake"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "provider_exists");
}

/// 監査で発見: `PATCH`/`DELETE` が id の検証を通さず、`..` で `providers_dir` の外のファイルを読み書き・削除できた。
#[tokio::test]
async fn patch_and_delete_reject_path_traversal_ids_without_touching_files_outside_providers_dir() {
    let (env, providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();

    // providers_dir の外（親ディレクトリ）に被害者ファイルを置く。
    let victim = providers_tmp.path().join("victim.toml");
    std::fs::write(&victim, "id = \"victim\"\nadapter = \"fake\"\nsecret = \"should-not-move\"\n").unwrap();

    for traversal_id in ["..%2Fvictim", "..%2F..%2Fvictim"] {
        let path = format!("/api/v1/providers/{traversal_id}");
        let resp = send(&app, patch_json_with(&path, &json!({"concurrency": 2}), &[("authorization", &auth)])).await;
        assert_problem(&resp, 404, "provider_not_found");
        let resp = send(&app, delete_with(&path, &[("authorization", &auth)])).await;
        assert_problem(&resp, 404, "provider_not_found");
    }

    // 被害者ファイルは無傷、providers_dir の外に何も作られていない。
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "id = \"victim\"\nadapter = \"fake\"\nsecret = \"should-not-move\"\n");
    assert!(!dir.join("victim.toml").exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "providers_dir must stay empty");
}

#[tokio::test]
async fn patch_updates_only_given_fields_and_unknown_id_is_404() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();
    std::fs::write(dir.join("acct-b.toml"), "id = \"acct-b\"\nadapter = \"fake\"\nconcurrency = 1\nmodel = \"m1\"\n").unwrap();

    let resp = send(&app, patch_json_with("/api/v1/providers/acct-b", &json!({"concurrency": 5}), &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    assert_eq!((v["concurrency"].clone(), v["model"].clone()), (json!(5), json!("m1")), "unspecified fields are kept");

    let resp = send(&app, patch_json_with("/api/v1/providers/does-not-exist", &json!({"concurrency": 2}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 404, "provider_not_found");
    let resp = send(&app, delete_with("/api/v1/providers/does-not-exist", &[("authorization", &auth)])).await;
    assert_problem(&resp, 404, "provider_not_found");
}

#[tokio::test]
async fn delete_removes_the_file_and_second_delete_is_404() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();
    std::fs::write(dir.join("acct-b.toml"), "id = \"acct-b\"\nadapter = \"fake\"\n").unwrap();

    let resp = send(&app, delete_with("/api/v1/providers/acct-b", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(!dir.join("acct-b.toml").exists());

    let resp = send(&app, delete_with("/api/v1/providers/acct-b", &[("authorization", &auth)])).await;
    assert_problem(&resp, 404, "provider_not_found");
}

/// 監査で発見: `Origin` 拒否が `POST` だけに掛かっていて `PATCH`/`DELETE` を素通りしていた（回帰テスト）。
#[tokio::test]
async fn patch_and_delete_reject_requests_carrying_an_origin_header() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();
    std::fs::write(dir.join("acct-b.toml"), "id = \"acct-b\"\nadapter = \"fake\"\n").unwrap();

    let resp = send(
        &app,
        patch_json_with(
            "/api/v1/providers/acct-b",
            &json!({"concurrency": 2}),
            &[("authorization", &auth), ("origin", "http://evil.example")],
        ),
    )
    .await;
    assert_problem(&resp, 403, "origin_forbidden");

    let resp = send(&app, delete_with("/api/v1/providers/acct-b", &[("authorization", &auth), ("origin", "http://evil.example")])).await;
    assert_problem(&resp, 403, "origin_forbidden");

    // Origin 無しなら通る（ファイルはまだ残っている）。
    let resp = send(&app, delete_with("/api/v1/providers/acct-b", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
}

#[tokio::test]
async fn admin_endpoints_are_unavailable_without_providers_dir_configured() {
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    });
    let app = env.router();
    let auth = auth();
    let resp = send(&app, post_json_with("/api/v1/providers", &json!({"id": "x", "adapter": "fake"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "providers_admin_unavailable");
}

/// ADR-0026 D7: `adapter = "acp"` は `POST /providers` の既知アダプタに入っている。
#[tokio::test]
async fn create_accepts_the_acp_adapter() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "opencode-qwen", "adapter": "acp", "tiers": ["standard"], "model": "qwen-local/qwen3.8-27b"}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    assert_eq!(resp.json()["adapter"], json!("acp"));
    let written = std::fs::read_to_string(dir.join("opencode-qwen.toml")).unwrap();
    assert!(written.contains("adapter = \"acp\""), "{written}");
    // ADR-0026 D7: 管理 API は command/args を書かない。
    assert!(!written.contains("command"), "{written}");
    assert!(!written.contains("args"), "{written}");
}

/// ADR-0026 D7: `command`/`args` は管理 API から書けない。`POST`/`PATCH` の本文にあれば拒否する
/// （実行するコマンドを HTTP から差し替えられないようにする。`providers.d/<id>.toml` は人が直接編集する）。
#[tokio::test]
async fn create_and_patch_reject_command_and_args_in_the_body() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "opencode-qwen", "adapter": "acp", "command": "opencode"}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
    assert!(!dir.join("opencode-qwen.toml").exists(), "create must not write a file when rejected");

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "opencode-qwen", "adapter": "acp", "args": ["acp", "--verbose"]}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");

    // 既存の行に対する PATCH も同様に拒否し、ファイルは変わらない。
    std::fs::write(
        dir.join("opencode-qwen.toml"),
        "id = \"opencode-qwen\"\nadapter = \"acp\"\ncommand = \"opencode\"\nargs = [\"acp\"]\n",
    )
    .unwrap();
    let before = std::fs::read_to_string(dir.join("opencode-qwen.toml")).unwrap();
    let resp = send(
        &app,
        patch_json_with(
            "/api/v1/providers/opencode-qwen",
            &json!({"command": "goose"}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
    let after = std::fs::read_to_string(dir.join("opencode-qwen.toml")).unwrap();
    assert_eq!(before, after, "a rejected PATCH must not touch the file");

    // command/args を持たない PATCH は通り、既存の値（人が書いた分）はファイル上に残る。
    let resp = send(
        &app,
        patch_json_with(
            "/api/v1/providers/opencode-qwen",
            &json!({"concurrency": 2}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let written = std::fs::read_to_string(dir.join("opencode-qwen.toml")).unwrap();
    assert!(written.contains("command = \"opencode\""), "{written}");
    assert!(written.contains("args = [\"acp\"]"), "{written}");
}

/// ADR-0027 D3: `settings`（`adapter = "paperqa"` の行の PaperQA 設定ファイルの上書き）も `command`/`args` と
/// 同じく管理 API からは書けない。`POST`/`PATCH` の本文にあれば拒否し、人が直接書いた値は PATCH の往復でも残る。
#[tokio::test]
async fn create_and_patch_reject_settings_in_the_body() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "paperqa-qwen", "adapter": "paperqa", "settings": "/settings/qwen-local"}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
    assert!(!dir.join("paperqa-qwen.toml").exists(), "create must not write a file when rejected");

    // 既存の行に対する PATCH も同様に拒否し、ファイルは変わらない。人が直接編集した settings は残る。
    std::fs::write(
        dir.join("paperqa-qwen.toml"),
        "id = \"paperqa-qwen\"\nadapter = \"paperqa\"\nsettings = \"/settings/qwen-local\"\n",
    )
    .unwrap();
    let before = std::fs::read_to_string(dir.join("paperqa-qwen.toml")).unwrap();
    let resp = send(
        &app,
        patch_json_with(
            "/api/v1/providers/paperqa-qwen",
            &json!({"settings": "/settings/other"}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
    let after = std::fs::read_to_string(dir.join("paperqa-qwen.toml")).unwrap();
    assert_eq!(before, after, "a rejected PATCH must not touch the file");

    // settings を持たない PATCH は通り、既存の値（人が書いた分）はファイル上に残る。
    let resp = send(
        &app,
        patch_json_with(
            "/api/v1/providers/paperqa-qwen",
            &json!({"concurrency": 2}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let written = std::fs::read_to_string(dir.join("paperqa-qwen.toml")).unwrap();
    assert!(written.contains("settings = \"/settings/qwen-local\""), "{written}");
}

/// ADR-0030 D2: `env_from_secrets`（環境変数名 → `[secrets]` の秘密 id）も `command`/`args`/`settings` と
/// 同じく管理 API からは書けない。`POST`/`PATCH` の本文にあれば拒否し、人が直接書いた値は PATCH の往復でも残る。
#[tokio::test]
async fn create_and_patch_reject_env_from_secrets_in_the_body() {
    let (env, _providers_tmp, dir) = env_with_providers_dir();
    let app = env.router();
    let auth = auth();

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "ldr-tavily", "adapter": "local-deep-research", "env_from_secrets": {"LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY": "tavily"}}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
    assert!(!dir.join("ldr-tavily.toml").exists(), "create must not write a file when rejected");

    // 既存の行に対する PATCH も同様に拒否し、ファイルは変わらない。人が直接編集した env_from_secrets は残る。
    std::fs::write(
        dir.join("ldr-tavily.toml"),
        "id = \"ldr-tavily\"\nadapter = \"local-deep-research\"\n[env_from_secrets]\nLDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = \"tavily\"\n",
    )
    .unwrap();
    let before = std::fs::read_to_string(dir.join("ldr-tavily.toml")).unwrap();
    let resp = send(
        &app,
        patch_json_with(
            "/api/v1/providers/ldr-tavily",
            &json!({"env_from_secrets": {"LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY": "other"}}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
    let after = std::fs::read_to_string(dir.join("ldr-tavily.toml")).unwrap();
    assert_eq!(before, after, "a rejected PATCH must not touch the file");

    // env_from_secrets を持たない PATCH は通り、既存の値（人が書いた分）はファイル上に残る。
    let resp = send(
        &app,
        patch_json_with(
            "/api/v1/providers/ldr-tavily",
            &json!({"concurrency": 2}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let written = std::fs::read_to_string(dir.join("ldr-tavily.toml")).unwrap();
    assert!(written.contains("LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = \"tavily\""), "{written}");
}
