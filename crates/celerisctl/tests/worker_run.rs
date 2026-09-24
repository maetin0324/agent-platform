//! `celerisctl worker run` の統合テスト（ADR-0012 D4）。実バイナリ（`CARGO_BIN_EXE_celerisctl`）を
//! `fake` アダプタ設定で起動し、ネットワークに出ずに done/question/error/provider_failure/
//! `--provider`・`--adapter` の分岐・running タスクの `--workspace` 要求を確認する。
//! DB への書き込みが一切無いこと（`events_for` の件数・タスクの status が変わらない）も検証する。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use task_core::{
    Budget, Check, Criterion, Event, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier,
    WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

fn sample_task(status: Status, workspace: WorkspaceSpec) -> Task {
    let now = OffsetDateTime::now_utc();
    Task {
        routing: None,
        mode: Default::default(),
        skills: Vec::new(),
        repos: Vec::new(),
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "do something".to_string(),
        objective: "do it".to_string(),
        acceptance: vec![Criterion {
            text: "ok".to_string(),
            check: Check::Command {
                cmd: "true".to_string(),
                expect_exit: 0,
            },
        }],
        inputs: vec![],
        depends_on: vec![],
        status,
        priority: 0,
        worker_hint: WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        },
        workspace,
        budget: Budget {
            max_turns: 10,
            max_wall_secs: 30,
            max_retries: 1,
        },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: None,
        genre: None,
        aggregate: false,
        project_id: None,
        milestone_id: None,
        assignee: None,
        conversation: None,
        labels: Vec::new(),
        category: Default::default(),
    }
}

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fake script");
    path
}

/// `[adapters.fake]` の `command` と `[[providers]]` を組み合わせた `config.toml` を書く。
fn write_config(dir: &Path, script: &Path, providers_toml: &str) -> PathBuf {
    let workspace_root = dir.join("workspaces");
    let workspace_root_str = workspace_root.display().to_string();
    let script_str = script.display().to_string();
    let text = format!(
        "workspace_root = {workspace_root_str:?}\n\n[adapters.fake]\ncommand = [\"sh\", {script_str:?}]\n\n{providers_toml}\n"
    );
    let path = dir.join("config.toml");
    std::fs::write(&path, text).expect("write config");
    path
}

fn run_celerisctl(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_celerisctl"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .expect("run celerisctl")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// done: progress / artifact を逐次出し、`context.answers` に事前の `Answered` イベントが載り、
/// DB には一切書き込まれない（events_for の件数・status が不変）。
#[test]
fn done_prints_progress_artifact_and_result_without_touching_the_store() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("celeris.sqlite3");
    // `prepare` が artifacts/ を作った後にスクリプトが動くので、成果物はスクリプト内で書けばよい
    // （`artifact::resolve` は実在チェックをするため、`artifact` メッセージより前に書く）。
    let script = write_script(
        tmp.path(),
        "done.sh",
        r#"cat > stdin.json
echo '{"type":"progress","msg":"working"}'
echo hi > artifacts/out.txt
echo '{"type":"artifact","name":"out","path":"artifacts/out.txt"}'
echo '{"type":"done","summary":"ok","evidence":[]}'
"#,
    );

    let providers = "[[providers]]\nid = \"fake-local\"\nadapter = \"fake\"\n";
    let config_path = write_config(tmp.path(), &script, providers);
    let workspace_dir = tmp.path().join("ws");

    let store = SqliteStore::open(&db_path).expect("open store");
    let task = sample_task(
        Status::Ready,
        WorkspaceSpec::Local {
            path: PathBuf::from("unused"),
            mode: None,
        },
    );
    store.insert(&task).expect("insert task");
    store
        .append_event(
            task.id,
            &Event::Answered {
                question: "q?".to_string(),
                answer: "a!".to_string(),
            },
        )
        .expect("append answered");

    let events_before = store.events_for(task.id).expect("events_for before");

    let out = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
            "--workspace",
            workspace_dir.to_str().expect("utf8"),
        ],
    );

    let stdout = stdout_of(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("progress: working"), "{stdout}");
    assert!(stdout.contains("artifact: out"), "{stdout}");
    assert!(stdout.contains("result: {\"type\":\"done\""), "{stdout}");

    let stdin_json =
        std::fs::read_to_string(workspace_dir.join("stdin.json")).expect("read stdin.json");
    assert!(
        stdin_json.contains(r#""answers":[{"question":"q?","answer":"a!"}]"#),
        "{stdin_json}"
    );

    let events_after = store.events_for(task.id).expect("events_for after");
    assert_eq!(events_after.len(), events_before.len());
    let fetched = store.get(task.id).expect("get").expect("task exists");
    assert_eq!(fetched.status, Status::Ready);
}

/// question: exit 3, `result: {"type":"question"...}`。
#[test]
fn question_exits_three() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("celeris.sqlite3");
    let script = write_script(
        tmp.path(),
        "question.sh",
        r#"cat > /dev/null
echo '{"type":"question","text":"which one?"}'
"#,
    );
    let providers = "[[providers]]\nid = \"fake-local\"\nadapter = \"fake\"\n";
    let config_path = write_config(tmp.path(), &script, providers);

    let store = SqliteStore::open(&db_path).expect("open store");
    let task = sample_task(
        Status::Ready,
        WorkspaceSpec::Local {
            path: PathBuf::from("unused"),
            mode: None,
        },
    );
    store.insert(&task).expect("insert task");

    let out = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
            "--workspace",
            tmp.path().join("ws").to_str().expect("utf8"),
        ],
    );

    let stdout = stdout_of(&out);
    assert_eq!(
        out.status.code(),
        Some(3),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains(r#"result: {"type":"question""#), "{stdout}");
}

/// `--provider` は指定した行の env を使う。省略時は設定表の先頭行。
#[test]
fn provider_selects_the_matching_account_env() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("celeris.sqlite3");
    let script = write_script(
        tmp.path(),
        "account.sh",
        "cat > /dev/null\necho \"{\\\"type\\\":\\\"progress\\\",\\\"msg\\\":\\\"account=$ACCOUNT\\\"}\"\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'\n",
    );
    let providers = "[[providers]]\nid = \"acct-a\"\nadapter = \"fake\"\nenv = { ACCOUNT = \"a\" }\n\n[[providers]]\nid = \"acct-b\"\nadapter = \"fake\"\nenv = { ACCOUNT = \"b\" }\n";
    let config_path = write_config(tmp.path(), &script, providers);

    let store = SqliteStore::open(&db_path).expect("open store");
    let task = sample_task(
        Status::Ready,
        WorkspaceSpec::Local {
            path: PathBuf::from("unused"),
            mode: None,
        },
    );
    store.insert(&task).expect("insert task");

    let out = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
            "--provider",
            "acct-b",
            "--workspace",
            tmp.path().join("ws-b").to_str().expect("utf8"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout_of(&out).contains("account=b"), "{}", stdout_of(&out));

    let out = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
            "--workspace",
            tmp.path().join("ws-default").to_str().expect("utf8"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout_of(&out).contains("account=a"), "{}", stdout_of(&out));
}

/// running/reviewing のタスクは `--workspace` 無しでは拒否し、指定すれば実行できる。
#[test]
fn running_task_requires_explicit_workspace() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("celeris.sqlite3");
    let script = write_script(
        tmp.path(),
        "done.sh",
        "cat > /dev/null\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'\n",
    );
    let providers = "[[providers]]\nid = \"fake-local\"\nadapter = \"fake\"\n";
    let config_path = write_config(tmp.path(), &script, providers);

    let store = SqliteStore::open(&db_path).expect("open store");
    let task = sample_task(
        Status::Running,
        WorkspaceSpec::Local {
            path: PathBuf::from("running-task-ws"),
            mode: None,
        },
    );
    store.insert(&task).expect("insert task");

    let without_workspace = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
        ],
    );
    assert_eq!(
        without_workspace.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&without_workspace.stderr)
    );

    let with_workspace = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
            "--workspace",
            tmp.path().join("separate-ws").to_str().expect("utf8"),
        ],
    );
    assert_eq!(
        with_workspace.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&with_workspace.stderr)
    );
}

/// 存在しないプロバイダ ID は exit 1。
#[test]
fn unknown_provider_id_errors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("celeris.sqlite3");
    let script = write_script(
        tmp.path(),
        "done.sh",
        "cat > /dev/null\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'\n",
    );
    let providers = "[[providers]]\nid = \"fake-local\"\nadapter = \"fake\"\n";
    let config_path = write_config(tmp.path(), &script, providers);

    let store = SqliteStore::open(&db_path).expect("open store");
    let task = sample_task(
        Status::Ready,
        WorkspaceSpec::Local {
            path: PathBuf::from("unused"),
            mode: None,
        },
    );
    store.insert(&task).expect("insert task");

    let out = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
            "--provider",
            "nope",
            "--workspace",
            tmp.path().join("ws").to_str().expect("utf8"),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `error.provider_failure` 付きのワーカー error は、アダプタの `Err` 経由で
/// `provider_failure` を保ったまま exit 4 になる（ADR-0010 D5）。
#[test]
fn error_with_provider_failure_exits_four_and_preserves_provider_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("celeris.sqlite3");
    let script = write_script(
        tmp.path(),
        "throttled.sh",
        r#"cat > /dev/null
echo '{"type":"error","message":"429","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":7}}'
"#,
    );
    let providers = "[[providers]]\nid = \"fake-local\"\nadapter = \"fake\"\n";
    let config_path = write_config(tmp.path(), &script, providers);

    let store = SqliteStore::open(&db_path).expect("open store");
    let task = sample_task(
        Status::Ready,
        WorkspaceSpec::Local {
            path: PathBuf::from("unused"),
            mode: None,
        },
    );
    store.insert(&task).expect("insert task");

    let out = run_celerisctl(
        &db_path,
        &[
            "worker",
            "run",
            "--config",
            config_path.to_str().expect("utf8"),
            "--task",
            &task.id.to_string(),
            "--workspace",
            tmp.path().join("ws").to_str().expect("utf8"),
        ],
    );

    let stdout = stdout_of(&out);
    assert_eq!(
        out.status.code(),
        Some(4),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(r#""provider_failure":{"kind":"throttled","retry_after_secs":7}"#),
        "{stdout}"
    );
}

/// Exercise the actual CLI, account environment and final --model without an LLM service.
#[cfg(unix)]
#[test]
fn fixed_account_and_quota_routing_reach_the_cli_without_mutating_the_store() {
    use std::os::unix::fs::PermissionsExt;
    for (fixed, flag, utilization, stale, binding, expected, error) in [
        ("b", None, None, false, "fixture-cheap", "frontier", ""),
        (
            "b",
            Some("b"),
            Some(0.1),
            false,
            "fixture-cheap",
            "frontier",
            "",
        ),
        ("b", None, Some(0.8), false, "fixture-cheap", "standard", ""),
        ("b", None, Some(0.95), false, "fixture-cheap", "cheap", ""),
        (
            "",
            Some("b"),
            Some(0.95),
            false,
            "fixture-cheap",
            "cheap",
            "",
        ),
        ("b", None, Some(0.95), true, "fixture-cheap", "frontier", ""),
        (
            "missing-fixed-account",
            None,
            None,
            false,
            "fixture-cheap",
            "",
            "no eligible account",
        ),
        (
            "b",
            Some("a"),
            None,
            false,
            "fixture-cheap",
            "",
            "conflicts",
        ),
        (
            "b",
            None,
            Some(0.97),
            false,
            "fixture-cheap",
            "",
            "no eligible account",
        ),
        (
            "b",
            None,
            Some(0.95),
            false,
            "bad model",
            "",
            "executable model ID",
        ),
        (
            "",
            Some("b"),
            Some(0.97),
            false,
            "fixture-cheap",
            "",
            "execution deferred",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("accounts");
        for id in ["a", "b"] {
            std::fs::create_dir_all(root.join(id)).unwrap();
            std::fs::write(root.join(id).join("auth.json"), "{}").unwrap();
        }
        if let Some(u) = utilization {
            let now = OffsetDateTime::now_utc().unix_timestamp();
            let window = task_core::RateWindow {
                utilization: u,
                resets_at: now + 3600,
            };
            let mut book = task_dispatch::AccountBook::load(&root.join(".celeris-usage.json"));
            book.record_observation(
                "b",
                task_core::RateLimitObservation {
                    five_hour: Some(window),
                    seven_day: Some(window),
                    status: None,
                    resets_at: None,
                    observed_at: now - if stale { 600 } else { 0 },
                },
                task_dispatch::ObservationSource::Run,
            );
            book.save().unwrap();
        }
        let book_before = std::fs::read(root.join(".celeris-usage.json")).ok();
        let script = write_script(
            tmp.path(),
            "codex-stub",
            r#"#!/bin/sh
printf '%s\n' "$CODEX_HOME" > account.txt
printf '%s\n' "$@" > args.txt
mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fixed_line = if fixed.is_empty() {
            String::new()
        } else {
            format!("account_id = {fixed:?}")
        };
        let config = tmp.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                r#"
workspace_root = {:?}
[accounts]
codex_dir = {:?}
[[providers]]
id = "gpt"
adapter = "codex"
account_pool = true
{fixed_line}
[adapters.codex]
command = {:?}
[providers.tier_models.frontier]
name = "astra"
model_id = "fixture-frontier"
[providers.tier_models.standard]
name = "sol"
model_id = "fixture-standard"
[providers.tier_models.cheap]
name = "luna"
model_id = {binding:?}
"#,
                tmp.path().join("workspaces"),
                root,
                script
            ),
        )
        .unwrap();
        let ws = tmp.path().join("ws");
        let db = tmp.path().join("tasks.db");
        let store = SqliteStore::open(&db).unwrap();
        let mut task = sample_task(
            Status::Ready,
            WorkspaceSpec::Local {
                path: ws.clone(),
                mode: None,
            },
        );
        task.worker_hint.tier = Tier::Frontier;
        store.insert(&task).unwrap();
        let id = task.id.to_string();
        let mut args = vec![
            "worker",
            "run",
            "--config",
            config.to_str().unwrap(),
            "--task",
            &id,
            "--provider",
            "gpt",
        ];
        if let Some(flag) = flag {
            args.extend(["--account", flag]);
        }
        let out = run_celerisctl(&db, &args);
        let stdout = stdout_of(&out);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if error.is_empty() {
            assert!(out.status.success(), "{stdout}\n{stderr}");
            let argv = std::fs::read_to_string(ws.join("args.txt")).unwrap();
            assert!(
                argv.contains(&format!("--model\nfixture-{expected}\n")),
                "{argv}"
            );
            assert_eq!(
                std::fs::read_to_string(ws.join("account.txt"))
                    .unwrap()
                    .trim(),
                root.join("b").to_str().unwrap()
            );
            assert!(
                stdout.contains(&format!("model=fixture-{expected}")),
                "{stdout}"
            );
            assert!(stdout.contains("difficulty=Frontier"), "{stdout}");
            if utilization.is_none() || stale {
                assert!(stdout.contains("remaining unknown"), "{stdout}");
            }
        } else {
            assert!(!out.status.success(), "{stdout}");
            assert!(stderr.contains(error), "expected {error}: {stderr}");
            assert!(!ws.join("args.txt").exists(), "CLI must not start");
        }
        assert_eq!(
            std::fs::read(root.join(".celeris-usage.json")).ok(),
            book_before
        );
        assert_eq!(store.get(task.id).unwrap().unwrap(), task);
        assert!(store.events_for(task.id).unwrap().is_empty());
    }
}
