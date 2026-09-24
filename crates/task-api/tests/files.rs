//! api.md §8.7（ファイル）: パス検査（`../x`・絶対パス・外への symlink・不正な run_id → 403）、404、Range / offset、
//! Content-Type の閉じた表、`download`、`X-Celeris-Sha256(-Current)`、成果物一覧。

mod common;

use std::path::Path;

use common::*;
use sha2::{Digest, Sha256};
use task_core::{ArtifactRef, Event, Status, Task, TaskKind, TaskStore, WorkspaceSpec};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn write(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write");
}

fn produced(env: &TestEnv, task: &Task, run_id: &str, path: &str, sha: &str) {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    env.store
        .append_event(
            task.id,
            &Event::ArtifactProduced {
                run_id: run_id.to_string(),
                artifact: ArtifactRef {
                    name,
                    path: path.to_string(),
                    sha256: sha.to_string(),
                    kind: "file".into(),
                    declared: true,
                },
            },
        )
        .expect("append artifact");
}

struct Fixture {
    env: TestEnv,
    task: Task,
    run_id: String,
}

fn fixture() -> Fixture {
    let env = TestEnv::new();
    let task = new_task(TaskKind::Execute, Status::Running);
    env.seed(&task);
    let run_id = ulid::Ulid::new().to_string();
    let run_dir = env.workspace(&task).join("runs").join(&run_id);
    write(&run_dir.join("stdout.jsonl"), b"0123456789");
    write(
        &run_dir.join("result.json"),
        b"{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}\n",
    );
    Fixture { env, task, run_id }
}

#[tokio::test]
async fn run_logs_are_served_with_closed_content_types() {
    let Fixture { env, task, run_id } = fixture();
    write(
        &env.workspace(&task)
            .join("runs")
            .join(&run_id)
            .join("stderr.log"),
        b"warn\n",
    );
    let app = env.router();

    let stdout = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{run_id}/stdout", task.id)),
    )
    .await;
    assert_eq!(stdout.status, 200, "{}", stdout.text());
    assert_eq!(stdout.body, b"0123456789");
    assert_eq!(
        stdout.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        stdout.header("content-disposition"),
        Some("inline; filename=\"stdout.jsonl\"; filename*=UTF-8''stdout.jsonl")
    );
    assert_eq!(stdout.header("x-celeris-size"), Some("10"));
    assert_eq!(stdout.header("content-length"), Some("10"));
    assert_eq!(stdout.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(stdout.header("x-celeris-sha256"), None);

    let stderr = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{run_id}/stderr", task.id)),
    )
    .await;
    assert_eq!(stderr.status, 200);
    assert_eq!(
        stderr.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(stderr.body, b"warn\n");

    let result = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/runs/{run_id}/result?download=1",
            task.id
        )),
    )
    .await;
    assert_eq!(result.status, 200);
    assert_eq!(result.header("content-type"), Some("application/json"));
    assert!(
        result
            .header("content-disposition")
            .is_some_and(|v| v.starts_with("attachment; filename=\"result.json\""))
    );
}

#[tokio::test]
async fn run_ids_that_are_not_ulids_are_forbidden() {
    let Fixture { env, task, .. } = fixture();
    let app = env.router();
    let lower = ulid::Ulid::new().to_string().to_ascii_lowercase();
    for run_id in [
        "%2E%2E",
        "..",
        "abc",
        lower.as_str(),
        "%2E%2E%2F%2E%2E%2Fetc",
        "01J9ZX5T3K8Q7W6V5R4P3N2M1I",
    ] {
        let resp = send(
            &app,
            get(&format!("/api/v1/tasks/{}/runs/{run_id}/stdout", task.id)),
        )
        .await;
        assert_problem(&resp, 403, "path_forbidden");
    }
}

#[tokio::test]
async fn missing_runs_files_workspaces_and_tasks_are_404() {
    let Fixture { env, task, run_id } = fixture();
    let app = env.router();

    let unknown_run = ulid::Ulid::new().to_string();
    let resp = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/runs/{unknown_run}/stdout",
            task.id
        )),
    )
    .await;
    assert_problem(&resp, 404, "run_not_found");

    let resp = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{run_id}/stderr", task.id)),
    )
    .await;
    assert_problem(&resp, 404, "file_not_found");

    let resp = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/runs/{run_id}/stdout",
            task_core::TaskId::new()
        )),
    )
    .await;
    assert_problem(&resp, 404, "task_not_found");

    let mut remote = new_task(TaskKind::Execute, Status::Running);
    remote.workspace = WorkspaceSpec::Remote {
        cluster: "hpc".into(),
        path: "/scratch/x".into(),
        mode: None,
    };
    env.store.create_task(&remote, vec![]).expect("create");
    // ADR-0018 D1: Remote の run ファイルは写し `workspace_root/<task_id>` にある。写しがまだ無ければ 404。
    let resp = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{run_id}/stdout", remote.id)),
    )
    .await;
    let problem = assert_problem(&resp, 404, "file_not_found");
    assert_eq!(problem["detail"], "workspace directory does not exist");

    let no_workspace = new_task(TaskKind::Execute, Status::Running);
    env.store
        .create_task(&no_workspace, vec![])
        .expect("create");
    let resp = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/runs/{run_id}/stdout",
            no_workspace.id
        )),
    )
    .await;
    assert_problem(&resp, 404, "file_not_found");

    // ディレクトリはファイルとして返さない。
    std::fs::create_dir_all(
        env.workspace(&task)
            .join("runs")
            .join(&run_id)
            .join("stderr.log"),
    )
    .expect("mkdir");
    let resp = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{run_id}/stderr", task.id)),
    )
    .await;
    assert_problem(&resp, 403, "path_forbidden");
}

#[tokio::test]
async fn symlinked_run_directory_outside_the_workspace_is_forbidden() {
    let Fixture { env, task, .. } = fixture();
    let app = env.router();
    let outside = env.dir.path().join("outside-run");
    write(&outside.join("stdout.jsonl"), b"secret");
    let linked_run = ulid::Ulid::new().to_string();
    std::os::unix::fs::symlink(
        &outside,
        env.workspace(&task).join("runs").join(&linked_run),
    )
    .expect("symlink");
    let resp = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/runs/{linked_run}/stdout",
            task.id
        )),
    )
    .await;
    assert_problem(&resp, 403, "path_forbidden");
    assert!(!resp.text().contains("secret"));
}

#[tokio::test]
async fn artifact_paths_escaping_the_workspace_are_forbidden_but_listed() {
    let Fixture { env, task, run_id } = fixture();
    let app = env.router();
    let ws = env.workspace(&task);
    let outside = env.workspace_root.join("outside.txt");
    write(&outside, b"outside secret");
    write(&ws.join("artifacts/ok.txt"), b"hello");
    std::os::unix::fs::symlink(&outside, ws.join("artifacts/link.txt")).expect("symlink");

    produced(&env, &task, &run_id, "artifacts/ok.txt", &sha256(b"hello"));
    produced(&env, &task, &run_id, "../outside.txt", "x");
    produced(
        &env,
        &task,
        &run_id,
        outside.to_str().expect("utf-8 path"),
        "x",
    );
    produced(&env, &task, &run_id, "artifacts/link.txt", "x");
    produced(&env, &task, &run_id, "artifacts/missing.txt", "x");

    let ok = send(&app, get(&format!("/api/v1/tasks/{}/artifacts/0", task.id))).await;
    assert_eq!(ok.status, 200, "{}", ok.text());
    assert_eq!(ok.body, b"hello");
    for idx in 1..=3 {
        let resp = send(
            &app,
            get(&format!("/api/v1/tasks/{}/artifacts/{idx}", task.id)),
        )
        .await;
        assert_problem(&resp, 403, "path_forbidden");
        assert!(!resp.text().contains("outside secret"));
    }
    assert_problem(
        &send(&app, get(&format!("/api/v1/tasks/{}/artifacts/4", task.id))).await,
        404,
        "file_not_found",
    );
    assert_problem(
        &send(&app, get(&format!("/api/v1/tasks/{}/artifacts/5", task.id))).await,
        404,
        "artifact_not_found",
    );

    let list = send(&app, get(&format!("/api/v1/tasks/{}/artifacts", task.id))).await;
    assert_eq!(list.status, 200, "{}", list.text());
    let items = list.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 5);
    assert_eq!(items[0]["idx"], 0);
    assert_eq!(items[0]["run_id"], run_id);
    assert!(items[0]["ts"].as_str().is_some());
    assert_eq!(items[0]["exists"], true);
    assert_eq!(items[0]["forbidden"], false);
    assert_eq!(items[0]["size"], 5);
    assert_eq!(items[0]["sha256_current"], sha256(b"hello"));
    assert_eq!(items[0]["sha256_matches"], true);
    assert_eq!(items[0]["artifact"]["path"], "artifacts/ok.txt");
    for item in &items[1..=3] {
        assert_eq!(
            (item["exists"].as_bool(), item["forbidden"].as_bool()),
            (Some(false), Some(true)),
            "{item}"
        );
        assert!(
            item["size"].is_null()
                && item["sha256_current"].is_null()
                && item["sha256_matches"].is_null()
        );
    }
    assert_eq!(
        (
            items[4]["exists"].as_bool(),
            items[4]["forbidden"].as_bool()
        ),
        (Some(false), Some(false))
    );
    assert!(items[4]["sha256_matches"].is_null());

    let missing = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/artifacts",
            task_core::TaskId::new()
        )),
    )
    .await;
    assert_problem(&missing, 404, "task_not_found");
}

/// ADR-0036 D4: 共有 workspace のタスクの成果物は `.taskd/artifacts/<task_id>/…` という workspace 相対の
/// パスで記録される。GUI の読み取り API（`GET /tasks/{id}/artifacts/{idx}` と一覧）はそのまま読めること。
#[tokio::test]
async fn per_task_artifact_paths_under_dot_celeris_are_served() {
    let Fixture { env, task, run_id } = fixture();
    let app = env.router();
    let rel = format!(".taskd/artifacts/{}/report.md", task.id);
    write(&env.workspace(&task).join(&rel), b"# report");
    produced(&env, &task, &run_id, &rel, &sha256(b"# report"));

    let resp = send(&app, get(&format!("/api/v1/tasks/{}/artifacts/0", task.id))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.body, b"# report");

    let list = send(&app, get(&format!("/api/v1/tasks/{}/artifacts", task.id))).await;
    let items = list.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["artifact"]["path"], rel);
    assert_eq!(items[0]["exists"], true);
    assert_eq!(items[0]["forbidden"], false);
    assert_eq!(items[0]["sha256_matches"], true);
}

#[tokio::test]
async fn range_and_offset_requests_follow_the_spec() {
    let Fixture { env, task, run_id } = fixture();
    let app = env.router();
    let path = format!("/api/v1/tasks/{}/runs/{run_id}/stdout", task.id);

    let partial = send(&app, get_with(&path, &[("range", "bytes=2-5")])).await;
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, b"2345");
    assert_eq!(partial.header("content-range"), Some("bytes 2-5/10"));
    assert_eq!(partial.header("content-length"), Some("4"));
    assert_eq!(partial.header("x-celeris-size"), Some("10"));
    assert_eq!(partial.header("accept-ranges"), Some("bytes"));

    let suffix = send(&app, get_with(&path, &[("range", "bytes=-3")])).await;
    assert_eq!(
        (suffix.status.as_u16(), suffix.body.as_slice()),
        (206, b"789".as_slice())
    );
    let open_end = send(&app, get_with(&path, &[("range", "bytes=7-")])).await;
    assert_eq!(
        (open_end.status.as_u16(), open_end.body.as_slice()),
        (206, b"789".as_slice())
    );

    for range in ["bytes=10-", "bytes=0-1,4-5", "bytes=5-2", "bytes=x-y"] {
        let resp = send(&app, get_with(&path, &[("range", range)])).await;
        assert_problem(&resp, 416, "range_not_satisfiable");
        assert_eq!(resp.header("content-range"), Some("bytes */10"), "{range}");
        assert_eq!(resp.header("x-celeris-size"), Some("10"));
    }

    let offset = send(&app, get(&format!("{path}?offset=4"))).await;
    assert_eq!(
        (offset.status.as_u16(), offset.body.as_slice()),
        (200, b"456789".as_slice())
    );
    let window = send(&app, get(&format!("{path}?offset=4&length=2"))).await;
    assert_eq!(
        (window.status.as_u16(), window.body.as_slice()),
        (200, b"45".as_slice())
    );
    let at_end = send(&app, get(&format!("{path}?offset=10"))).await;
    assert_eq!(at_end.status, 200);
    assert!(at_end.body.is_empty());
    assert_eq!(at_end.header("x-celeris-size"), Some("10"));
    let past_end = send(&app, get(&format!("{path}?offset=11"))).await;
    assert_problem(&past_end, 416, "range_not_satisfiable");

    let both = send(
        &app,
        get_with(&format!("{path}?offset=1"), &[("range", "bytes=0-1")]),
    )
    .await;
    assert_problem(&both, 400, "bad_request");

    // 追尾: ファイルが伸びたら続きだけ取れる。
    let stdout = env
        .workspace(&task)
        .join("runs")
        .join(&run_id)
        .join("stdout.jsonl");
    std::fs::write(&stdout, b"0123456789abc").expect("append");
    let tail = send(&app, get(&format!("{path}?offset=10"))).await;
    assert_eq!(
        (tail.body.as_slice(), tail.header("x-celeris-size")),
        (b"abc".as_slice(), Some("13"))
    );
}

#[tokio::test]
async fn artifact_content_types_never_include_active_types() {
    let Fixture { env, task, run_id } = fixture();
    let app = env.router();
    let ws = env.workspace(&task);
    let cases = [
        ("artifacts/report.html", "application/octet-stream"),
        ("artifacts/icon.svg", "application/octet-stream"),
        ("artifacts/app.mjs", "application/octet-stream"),
        ("artifacts/notes.md", "text/markdown; charset=utf-8"),
        ("artifacts/plot.png", "image/png"),
        ("artifacts/bench.json", "application/json"),
        ("artifacts/main.rs", "text/plain; charset=utf-8"),
    ];
    for (path, _) in &cases {
        write(&ws.join(path), b"<script>alert(1)</script>");
        produced(
            &env,
            &task,
            &run_id,
            path,
            &sha256(b"<script>alert(1)</script>"),
        );
    }
    for (idx, (path, content_type)) in cases.iter().enumerate() {
        let resp = send(
            &app,
            get(&format!("/api/v1/tasks/{}/artifacts/{idx}", task.id)),
        )
        .await;
        assert_eq!(resp.status, 200, "{path}");
        assert_eq!(resp.header("content-type"), Some(*content_type), "{path}");
        assert_eq!(resp.header("x-content-type-options"), Some("nosniff"));
    }
    let download = send(
        &app,
        get(&format!("/api/v1/tasks/{}/artifacts/0?download=1", task.id)),
    )
    .await;
    assert_eq!(
        download.header("content-disposition"),
        Some("attachment; filename=\"report.html\"; filename*=UTF-8''report.html")
    );
    let inline = send(
        &app,
        get(&format!("/api/v1/tasks/{}/artifacts/0?download=0", task.id)),
    )
    .await;
    assert!(
        inline
            .header("content-disposition")
            .is_some_and(|v| v.starts_with("inline;"))
    );
    let bad = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/artifacts/0?download=yes",
            task.id
        )),
    )
    .await;
    assert_problem(&bad, 400, "bad_request");
}

#[tokio::test]
async fn sha256_current_changes_when_the_artifact_is_modified() {
    let Fixture { env, task, run_id } = fixture();
    let app = env.router();
    let file = env.workspace(&task).join("artifacts/bench.json");
    write(&file, b"{\"ns\":1}");
    let recorded = sha256(b"{\"ns\":1}");
    produced(&env, &task, &run_id, "artifacts/bench.json", &recorded);
    let path = format!("/api/v1/tasks/{}/artifacts/0", task.id);

    let before = send(&app, get(&path)).await;
    assert_eq!(before.header("x-celeris-sha256"), Some(recorded.as_str()));
    assert_eq!(
        before.header("x-celeris-sha256-current"),
        Some(recorded.as_str())
    );

    std::fs::write(&file, b"{\"ns\":2}").expect("modify");
    let after = send(&app, get(&path)).await;
    assert_eq!(after.header("x-celeris-sha256"), Some(recorded.as_str()));
    assert_eq!(
        after.header("x-celeris-sha256-current"),
        Some(sha256(b"{\"ns\":2}").as_str())
    );

    // Range 付きでもハッシュはファイル全体。
    let ranged = send(&app, get_with(&path, &[("range", "bytes=0-0")])).await;
    assert_eq!(ranged.status, 206);
    assert_eq!(
        ranged.header("x-celeris-sha256-current"),
        Some(sha256(b"{\"ns\":2}").as_str())
    );

    let list = send(&app, get(&format!("/api/v1/tasks/{}/artifacts", task.id)))
        .await
        .json();
    assert_eq!(list["items"][0]["sha256_matches"], false);
    assert_eq!(list["items"][0]["sha256_current"], sha256(b"{\"ns\":2}"));
}
