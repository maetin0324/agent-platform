//! ADR-0046 D7 / §4-5: `celerisctl org migrate-v2` の往復（forward → rollback → 元と同一）。
//!
//! 外部ネットワークには出ない。DB も記憶も tempdir の中だけ（CLAUDE.md の禁止事項）。
//! `celerisctl` はバイナリなので、ここではサブプロセスとして実行する（本物の CLI の経路を通す）。

use std::path::Path;
use std::process::Command;

use task_core::{
    Budget, OrgKind, OrgNode, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier, WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

/// Phase 58 までの組織（`config/org.example.toml` の Phase 58 版の id）。
const V1_NODES: [(&str, Option<&str>, OrgKind); 8] = [
    ("secretary", None, OrgKind::Secretary),
    ("coding", Some("secretary"), OrgKind::Department),
    ("coding-frontend", Some("coding"), OrgKind::Section),
    ("coding-performance", Some("coding"), OrgKind::Section),
    ("coding-poc", Some("coding"), OrgKind::Section),
    ("research", Some("secretary"), OrgKind::Department),
    ("research-survey", Some("research"), OrgKind::Section),
    ("infra", Some("secretary"), OrgKind::Department),
];

struct Env {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    db: std::path::PathBuf,
    config: std::path::PathBuf,
    backups: std::path::PathBuf,
    memory: std::path::PathBuf,
}

fn org_toml() -> String {
    // 新しい seed（ADR-0046 D7）。リポジトリの `config/org.example.toml` をそのまま使う。
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    std::fs::read_to_string(repo.join("config/org.example.toml")).unwrap_or_else(|e| panic!("org.example.toml: {e}"))
}

fn setup() -> Env {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("memory")).unwrap_or_else(|e| panic!("memory: {e}"));
    std::fs::write(root.join("org.toml"), org_toml()).unwrap_or_else(|e| panic!("org.toml: {e}"));
    let config = root.join("config.toml");
    std::fs::write(
        &config,
        r#"
db = "celeris.sqlite3"
workspace_root = "ws"
org_include = "org.toml"

[memory]
dir = "memory"

[conversation]
genre = "conversation"

[[harnesses]]
id = "conversation"
description = "人と話す"
tier = "standard"
conversation = true

[[harnesses]]
id = "plan"
description = "分解する"
tier = "standard"

[[harnesses]]
id = "coding"
description = "コードを書く"
tier = "standard"

[[harnesses]]
id = "data-analysis"
description = "データを整える"
tier = "standard"

[[harnesses]]
id = "writing"
description = "書く"
tier = "standard"

[[harnesses]]
id = "literature"
description = "論文を読む"
tier = "standard"

[[harnesses]]
id = "web-research"
description = "Web を調べる"
tier = "standard"
"#,
    )
    .unwrap_or_else(|e| panic!("config: {e}"));
    Env {
        db: root.join("celeris.sqlite3"),
        backups: root.join("backups"),
        memory: root.join("memory"),
        config,
        root,
        _dir: dir,
    }
}

fn seed(env: &Env) -> (Vec<OrgNode>, TaskId) {
    let store = SqliteStore::open(&env.db).unwrap_or_else(|e| panic!("open: {e}"));
    let now = OffsetDateTime::now_utc();
    let nodes: Vec<OrgNode> = V1_NODES
        .iter()
        .map(|(id, parent, kind)| OrgNode {
            id: (*id).to_string(),
            parent_id: parent.map(str::to_string),
            name: format!("{id} の人"),
            kind: *kind,
            genre: None,
            brief: format!("{id} の一言"),
            profile: Default::default(),
            position: 0,
            created_at: now,
            updated_at: now,
        })
        .collect();
    store.org_seed(&nodes).unwrap_or_else(|e| panic!("seed: {e}"));

    // `coding-poc` に割り当てられたタスク（合流の側）。
    let id = TaskId::new();
    let task = Task {
        id,
        parent_id: None,
        kind: TaskKind::Execute,
        title: "やること".into(),
        objective: "目的".into(),
        acceptance: vec![],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Draft,
        priority: 10,
        worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
        workspace: WorkspaceSpec::local(env.root.join("ws")),
        repos: vec![],
        budget: Budget { max_turns: 1, max_wall_secs: 1, max_retries: 0 },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: None,
        genre: Some("coding".into()),
        aggregate: false,
        project_id: None,
        milestone_id: None,
        assignee: Some("coding-poc".into()),
        labels: vec![],
        category: Default::default(),
        skills: vec![],
        mode: Default::default(),
        conversation: None,
    };
    store.create_task(&task, vec![]).unwrap_or_else(|e| panic!("create: {e}"));

    // 記憶: `coding-frontend` は移動、`coding-poc` は合流（追記）。
    for (node, text) in [("coding-frontend", "front のメモ\n"), ("coding-poc", "poc のメモ\n")] {
        let dir = env.memory.join(node);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(dir.join("notes.md"), text).unwrap_or_else(|e| panic!("notes: {e}"));
    }
    (nodes, id)
}

fn celerisctl(env: &Env, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_celerisctl"))
        .arg("--db")
        .arg(&env.db)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run celerisctl: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "celerisctl {args:?} failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn snapshot(env: &Env) -> (Vec<OrgNode>, Option<String>) {
    let store = SqliteStore::open(&env.db).unwrap_or_else(|e| panic!("open: {e}"));
    let nodes = store.org_list().unwrap_or_else(|e| panic!("list: {e}"));
    let assignee = store
        .list(None)
        .unwrap_or_else(|e| panic!("tasks: {e}"))
        .into_iter()
        .next()
        .and_then(|t| t.assignee);
    (nodes, assignee)
}

/// ADR-0046 §4-5: tempdir の DB と記憶で往復（forward → rollback → 元と同一）。
#[test]
fn migrate_v2_maps_the_tree_and_rollback_restores_it_exactly() {
    let env = setup();
    let (before_nodes, _task) = seed(&env);
    let (before, before_assignee) = snapshot(&env);
    assert_eq!(before.len(), before_nodes.len());
    assert_eq!(before_assignee.as_deref(), Some("coding-poc"));

    // --dry-run は何も変えない。
    let out = celerisctl(
        &env,
        &[
            "org",
            "migrate-v2",
            "--config",
            &env.config.to_string_lossy(),
            "--backup-dir",
            &env.backups.to_string_lossy(),
            "--dry-run",
        ],
    );
    assert!(out.contains("rename secretary -> cos"), "{out}");
    assert!(out.contains("merge  coding-poc -> software-engineering"), "{out}");
    assert!(out.contains("create operations"), "{out}");
    assert_eq!(snapshot(&env).0, before, "dry-run は DB を変えない");
    assert!(!env.backups.join("org-v1-map.json").exists());

    // 本番。
    let out = celerisctl(
        &env,
        &[
            "org",
            "migrate-v2",
            "--config",
            &env.config.to_string_lossy(),
            "--backup-dir",
            &env.backups.to_string_lossy(),
        ],
    );
    assert!(out.contains("wrote"), "{out}");
    let (after, after_assignee) = snapshot(&env);
    let ids: Vec<&str> = after.iter().map(|n| n.id.as_str()).collect();
    for expected in [
        "cos",
        "engineering",
        "software-engineering",
        "systems-performance",
        "research",
        "literature-research",
        "operations",
        "infrastructure",
        "cluster-hpc",
        "monitoring-automation",
    ] {
        assert!(ids.contains(&expected), "{expected} が無い: {ids:?}");
    }
    assert!(!ids.contains(&"secretary"), "{ids:?}");
    assert!(!ids.contains(&"coding-poc"), "合流したので消える: {ids:?}");
    // 参照も書き換わる（`tasks.assignee` と `json` の中の両方）。
    assert_eq!(after_assignee.as_deref(), Some("software-engineering"));
    // 新しいノードには種の profile が入る。
    let cos = after.iter().find(|n| n.id == "cos").unwrap_or_else(|| panic!("cos"));
    assert_eq!(cos.profile.harnesses.allowed, vec!["conversation".to_string(), "plan".to_string()]);
    let ops = after.iter().find(|n| n.id == "operations").unwrap_or_else(|| panic!("operations"));
    assert_eq!(ops.profile.tools, vec!["docker".to_string()]);
    // 記憶: 移動と追記。
    let merged = std::fs::read_to_string(env.memory.join("software-engineering/notes.md"))
        .unwrap_or_else(|e| panic!("merged notes: {e}"));
    assert!(merged.contains("front のメモ"), "{merged}");
    assert!(merged.contains("poc のメモ"), "{merged}");
    assert!(!env.memory.join("coding-frontend").exists());
    assert!(!env.memory.join("coding-poc").exists());

    // 逆向き。
    let out = celerisctl(
        &env,
        &[
            "org",
            "migrate-v2",
            "--config",
            &env.config.to_string_lossy(),
            "--backup-dir",
            &env.backups.to_string_lossy(),
            "--rollback",
        ],
    );
    assert!(out.contains("the tree is back"), "{out}");
    let (restored, restored_assignee) = snapshot(&env);
    let restored_ids: Vec<&str> = restored.iter().map(|n| n.id.as_str()).collect();
    let before_ids: Vec<&str> = before.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(restored_ids, before_ids, "往復して元の木に戻る");
    assert_eq!(restored_assignee.as_deref(), Some("coding-poc"));
    // 記憶も戻る。
    assert_eq!(
        std::fs::read_to_string(env.memory.join("coding-frontend/notes.md")).unwrap_or_default(),
        "front のメモ\n"
    );
    assert_eq!(
        std::fs::read_to_string(env.memory.join("coding-poc/notes.md")).unwrap_or_default(),
        "poc のメモ\n"
    );
    assert!(!env.memory.join("software-engineering").exists());
}

/// ADR-0046 D3 / §4-2: `celerisctl config to-harnesses` は旧い設定を `[[harnesses]]` に写す。
#[test]
fn config_to_harnesses_prints_the_new_shape_from_the_legacy_one() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        r#"
db = "celeris.sqlite3"
workspace_root = "ws"

[[roles]]
id = "implementer"
tier = "standard"
max_turns = 20
instructions = "あなたは実装担当。"

[[roles]]
id = "secretary"
tier = "standard"
instructions = "あなたは人の秘書。"

[[genres]]
id = "coding"
description = "コードを書く"
default_role = "implementer"
roles = ["implementer"]

[[genres]]
id = "secretary"
description = "人と話す"
default_role = "secretary"
roles = ["secretary"]
"#,
    )
    .unwrap_or_else(|e| panic!("config: {e}"));
    let out = Command::new(env!("CARGO_BIN_EXE_celerisctl"))
        .args(["config", "to-harnesses", "--config"])
        .arg(&config)
        .output()
        .unwrap_or_else(|e| panic!("run: {e}"));
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("[[harnesses]]"), "{text}");
    assert!(text.contains("id = \"coding\""), "{text}");
    assert!(text.contains("adapter") || text.contains("tier = \"standard\""), "{text}");
    assert!(text.contains("max_turns = 20"), "{text}");
    // ADR-0046 D6: 対話用分野は `conversation` という id で書き出し、`[conversation]` も出す。
    assert!(text.contains("id = \"conversation\""), "{text}");
    assert!(text.contains("conversation = true"), "{text}");
    assert!(text.contains("[conversation]\ngenre = \"conversation\""), "{text}");
    // 貼り替え方の注意書き。
    assert!(text.contains("`[[genres]]` と `[[roles]]` の節を全部消す"), "{text}");
}
