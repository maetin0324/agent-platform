//! `celerisctl org migrate-v2`（ADR-0046 D7）。
//!
//! Phase 58 までの日本語の木（`secretary` / `coding` / `research` / `infra` …）を、ADR-0046 D7 の
//! 英語の木（`cos` / `engineering` / `research` / `operations` …）へ**決定的に**写す。
//!
//! やること:
//! 1. `org_nodes` の id を写像する（`coding-frontend` と `coding-poc` は `software-engineering` に**合流**）。
//! 2. ノード id を参照する全ての表（`tasks.assignee` と `tasks.json` の中、`messages` / `reports` /
//!    `approvals` / `standing_rules`）を同じ写像で書き換える。
//! 3. 長期記憶（`<memory>/<node>/`）のディレクトリを移す。合流する側は `notes.md` を**追記**で統合し、
//!    元のディレクトリは backups に退避する。
//! 4. 種（`org_include` の `[[org]]`）に居て DB に無いノード（`operations` / `cluster-hpc` /
//!    `monitoring-automation`）を profile 付きで作る。既にあるノードの profile も種の値で埋める
//!    （**profile が空のノードだけ**。人が GUI で書いた profile は上書きしない）。
//! 5. 逆写像を `<db の隣>/backups/org-v1-map.json` に書く（`--rollback` がこれを読んで元に戻す）。
//!
//! LLM は使わない。`--dry-run` は何も書かずに計画だけを出す。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use celeris::config::Config;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use task_core::{OrgNode, SqliteStore, TaskStore};
use time::OffsetDateTime;

use crate::error::CliError;
use crate::outln;

/// ADR-0046 D7 の写像（決定的。順序も固定）。左が Phase 58 までの id、右が新しい id。
pub const V1_TO_V2: [(&str, &str); 11] = [
    ("secretary", "cos"),
    ("coding", "engineering"),
    ("coding-frontend", "software-engineering"),
    ("coding-performance", "systems-performance"),
    ("coding-poc", "software-engineering"),
    ("research", "research"),
    ("research-survey", "literature-research"),
    ("research-web", "web-research"),
    ("research-writing", "scientific-writing"),
    ("research-data", "experiment-data"),
    ("infra", "infrastructure"),
];

/// 逆写像のファイル名（`<db の隣>/backups/` に置く）。
pub const BACKUP_FILE: &str = "org-v1-map.json";

/// ノード id を持つ表と列（`tasks` は `assignee` 列と `json` の中の両方）。
const NODE_TABLES: [(&str, &str); 5] = [
    ("tasks", "assignee"),
    ("messages", "node_id"),
    ("reports", "node_id"),
    ("approvals", "node_id"),
    ("standing_rules", "node_id"),
];

#[derive(Subcommand, Debug)]
pub enum OrgCommand {
    /// Phase 58 までの木を ADR-0046 D7 の木へ写す。
    MigrateV2(MigrateV2Args),
}

#[derive(clap::Args, Debug)]
pub struct MigrateV2Args {
    /// 設定ファイル（`[memory] dir` と `org_include` の種を読む）。
    #[arg(long)]
    pub config: PathBuf,
    /// 何も書かずに、やることだけを出す。
    #[arg(long)]
    pub dry_run: bool,
    /// 直前の `migrate-v2` を元に戻す（`backups/org-v1-map.json` を読む）。
    #[arg(long)]
    pub rollback: bool,
    /// 逆写像の置き場（既定は DB と同じディレクトリの `backups/`）。
    #[arg(long)]
    pub backup_dir: Option<PathBuf>,
}

/// `backups/org-v1-map.json` の中身。`--rollback` はこれだけを見て元に戻す。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationBackup {
    pub version: u32,
    pub created_at: String,
    /// 適用した写像（旧 id → 新 id）。
    pub map: BTreeMap<String, String>,
    /// 移行前の `org_nodes`（全行。`--rollback` はこれで作り直す）。
    pub nodes: Vec<OrgNode>,
    /// 移行で**新しく作った**ノードの id（`--rollback` で消す）。
    pub created: Vec<String>,
    /// 書き換えた行（表 / 主キー / 列 / 元の値）。
    pub rows: Vec<RowChange>,
    /// 移した記憶のディレクトリ（`from` → `to`）。
    pub memory_moves: Vec<MemoryMove>,
    /// 追記で統合した記憶（`dest` の元の長さ。`--rollback` はここまで切り詰める）。
    pub memory_appends: Vec<MemoryAppend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowChange {
    pub table: String,
    pub id: String,
    pub column: String,
    pub old: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAppend {
    /// 追記された先のファイル。
    pub dest: PathBuf,
    /// 追記する前の長さ（バイト）。
    pub original_len: u64,
    /// 退避した元のディレクトリ（`--rollback` で戻す）。
    pub saved_from: PathBuf,
    /// 元のディレクトリの本来の場所。
    pub original_dir: PathBuf,
}

pub fn run(store: &SqliteStore, db_path: &Path, command: OrgCommand) -> Result<ExitCode, CliError> {
    match command {
        OrgCommand::MigrateV2(args) => {
            let config = Config::load(&args.config)
                .map_err(|e| CliError::msg(format!("config {}: {e}", args.config.display())))?;
            let backup_dir = args
                .backup_dir
                .clone()
                .unwrap_or_else(|| db_path.parent().unwrap_or(Path::new(".")).join("backups"));
            if args.rollback {
                rollback(store, &backup_dir, args.dry_run)
            } else {
                migrate(store, &config, &backup_dir, args.dry_run)
            }
        }
    }
}

/// 写像（旧 id → 新 id）。知らない id はそのまま（触らない）。
pub fn mapping() -> BTreeMap<String, String> {
    V1_TO_V2
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn memory_dir(config: &Config) -> Option<PathBuf> {
    config.memory.as_ref().map(|m| m.dir.clone())
}

fn migrate(
    store: &SqliteStore,
    config: &Config,
    backup_dir: &Path,
    dry_run: bool,
) -> Result<ExitCode, CliError> {
    let map = mapping();
    let before = store.org_list()?;
    if before.is_empty() {
        outln!(
            "org_nodes is empty; nothing to migrate (the seed will be planted on the next start)"
        );
        return Ok(ExitCode::SUCCESS);
    }
    let seed: Vec<OrgNode> = config.org_nodes(OffsetDateTime::now_utc());
    let renames: Vec<(String, String)> = before
        .iter()
        .filter_map(|n| map.get(&n.id).map(|to| (n.id.clone(), to.clone())))
        .collect();
    let existing_after: Vec<String> = before
        .iter()
        .map(|n| map.get(&n.id).cloned().unwrap_or_else(|| n.id.clone()))
        .collect();
    let created: Vec<String> = seed
        .iter()
        .filter(|s| !existing_after.iter().any(|id| id == &s.id))
        .map(|s| s.id.clone())
        .collect();

    outln!(
        "org migrate-v2 (ADR-0046 D7){}",
        if dry_run { " [dry-run]" } else { "" }
    );
    for (from, to) in &renames {
        if from == to {
            outln!("  keep   {from}");
        } else if renames.iter().filter(|(_, t)| t == to).count() > 1 {
            outln!("  merge  {from} -> {to}");
        } else {
            outln!("  rename {from} -> {to}");
        }
    }
    for id in &created {
        outln!("  create {id}");
    }
    let unmapped: Vec<&OrgNode> = before.iter().filter(|n| !map.contains_key(&n.id)).collect();
    for node in &unmapped {
        outln!(
            "  keep   {} (not in the ADR-0046 D7 map; left as is)",
            node.id
        );
    }
    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // ---- 1. 参照している行を書き換える（記録を取りながら）----
    let sqlite = store;
    let mut rows: Vec<RowChange> = Vec::new();
    for (table, column) in NODE_TABLES {
        for (from, to) in &renames {
            if from == to {
                continue;
            }
            for id in sqlite.migrate_node_refs(table, column, from, to)? {
                rows.push(RowChange {
                    table: table.to_string(),
                    id,
                    column: column.to_string(),
                    old: from.clone(),
                });
            }
        }
    }
    outln!(
        "  rewrote {} row(s) that referenced an old node id",
        rows.len()
    );

    // ---- 2. 記憶のディレクトリ ----
    let mut memory_moves: Vec<MemoryMove> = Vec::new();
    let mut memory_appends: Vec<MemoryAppend> = Vec::new();
    if let Some(dir) = memory_dir(config) {
        let saved_root = backup_dir.join("memory-v1");
        for (from, to) in &renames {
            if from == to {
                continue;
            }
            let src = dir.join(from);
            if !src.is_dir() {
                continue;
            }
            let dest = dir.join(to);
            if !dest.exists() {
                std::fs::create_dir_all(dir.join("."))
                    .map_err(|e| CliError::msg(format!("memory dir: {e}")))?;
                std::fs::rename(&src, &dest).map_err(|e| {
                    CliError::msg(format!("move {} -> {}: {e}", src.display(), dest.display()))
                })?;
                memory_moves.push(MemoryMove {
                    from: src,
                    to: dest,
                });
                continue;
            }
            // 合流: `notes.md` を追記で統合し、元のディレクトリは backups に退避する。
            let notes = dest.join("notes.md");
            let original_len = std::fs::metadata(&notes).map(|m| m.len()).unwrap_or(0);
            let source_notes = src.join("notes.md");
            if source_notes.is_file() {
                let text = std::fs::read_to_string(&source_notes)
                    .map_err(|e| CliError::msg(format!("read {}: {e}", source_notes.display())))?;
                let mut merged = if original_len > 0 {
                    std::fs::read_to_string(&notes).unwrap_or_default()
                } else {
                    String::new()
                };
                if !merged.is_empty() && !merged.ends_with('\n') {
                    merged.push('\n');
                }
                merged.push_str(&format!(
                    "\n<!-- merged from {from} (celerisctl org migrate-v2) -->\n"
                ));
                merged.push_str(&text);
                std::fs::create_dir_all(&dest).map_err(|e| CliError::msg(format!("mkdir: {e}")))?;
                std::fs::write(&notes, merged)
                    .map_err(|e| CliError::msg(format!("write {}: {e}", notes.display())))?;
            }
            std::fs::create_dir_all(&saved_root)
                .map_err(|e| CliError::msg(format!("mkdir: {e}")))?;
            let saved = saved_root.join(from);
            std::fs::rename(&src, &saved).map_err(|e| {
                CliError::msg(format!(
                    "save {} -> {}: {e}",
                    src.display(),
                    saved.display()
                ))
            })?;
            memory_appends.push(MemoryAppend {
                dest: notes,
                original_len,
                saved_from: saved,
                original_dir: src,
            });
        }
        outln!(
            "  memory: {} moved, {} merged",
            memory_moves.len(),
            memory_appends.len()
        );
    }

    // ---- 3. `org_nodes` を作り直す（親が先。旧い木は消す）----
    let now = OffsetDateTime::now_utc();
    let mut next: Vec<OrgNode> = Vec::new();
    for node in &before {
        let new_id = map
            .get(&node.id)
            .cloned()
            .unwrap_or_else(|| node.id.clone());
        if next.iter().any(|n: &OrgNode| n.id == new_id) {
            continue; // 合流した 2 つめ（`coding-poc`）は捨てる。
        }
        let seeded = seed.iter().find(|s| s.id == new_id);
        next.push(OrgNode {
            id: new_id.clone(),
            parent_id: match seeded {
                Some(s) => s.parent_id.clone(),
                None => node
                    .parent_id
                    .as_ref()
                    .map(|p| map.get(p).cloned().unwrap_or_else(|| p.clone())),
            },
            name: seeded
                .map(|s| s.name.clone())
                .unwrap_or_else(|| node.name.clone()),
            kind: seeded.map(|s| s.kind).unwrap_or(node.kind),
            genre: seeded
                .and_then(|s| s.genre.clone())
                .or_else(|| node.genre.clone()),
            brief: match seeded {
                Some(s) if !s.brief.trim().is_empty() => s.brief.clone(),
                _ => node.brief.clone(),
            },
            // 人が GUI で書いた profile は上書きしない（空のときだけ種で埋める）。
            profile: if node.profile.is_empty() {
                seeded.map(|s| s.profile.clone()).unwrap_or_default()
            } else {
                node.profile.clone()
            },
            position: seeded.map(|s| s.position).unwrap_or(node.position),
            created_at: node.created_at,
            updated_at: now,
        });
    }
    for id in &created {
        if let Some(s) = seed.iter().find(|s| &s.id == id) {
            next.push(s.clone());
        }
    }
    // 親が先に入るよう、種の並び（secretary → department → section）に揃える。
    next.sort_by_key(|n| match n.kind {
        task_core::OrgKind::Secretary => 0,
        task_core::OrgKind::Department => 1,
        task_core::OrgKind::Section => 2,
    });
    sqlite.replace_org_nodes(&next)?;
    outln!("  org_nodes: {} node(s)", next.len());

    // ---- 4. 逆写像を書く ----
    let backup = MigrationBackup {
        version: 1,
        created_at: now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now.unix_timestamp().to_string()),
        map: renames.iter().cloned().collect(),
        nodes: before,
        created,
        rows,
        memory_moves,
        memory_appends,
    };
    std::fs::create_dir_all(backup_dir)
        .map_err(|e| CliError::msg(format!("mkdir {}: {e}", backup_dir.display())))?;
    let path = backup_dir.join(BACKUP_FILE);
    let json = serde_json::to_string_pretty(&backup).map_err(|e| CliError::msg(e.to_string()))?;
    std::fs::write(&path, format!("{json}\n"))
        .map_err(|e| CliError::msg(format!("write {}: {e}", path.display())))?;
    outln!("  wrote {}", path.display());
    outln!("done. roll back with: celerisctl org migrate-v2 --config <config> --rollback");
    Ok(ExitCode::SUCCESS)
}

fn rollback(store: &SqliteStore, backup_dir: &Path, dry_run: bool) -> Result<ExitCode, CliError> {
    let path = backup_dir.join(BACKUP_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        CliError::msg(format!(
            "read {}: {e} (migrate-v2 was never run here?)",
            path.display()
        ))
    })?;
    let backup: MigrationBackup = serde_json::from_str(&text)
        .map_err(|e| CliError::msg(format!("{}: {e}", path.display())))?;
    outln!(
        "org migrate-v2 --rollback ({} from {}){}",
        path.display(),
        backup.created_at,
        if dry_run { " [dry-run]" } else { "" }
    );
    outln!(
        "  restore {} node(s), {} row(s), {} memory move(s), {} memory merge(s)",
        backup.nodes.len(),
        backup.rows.len(),
        backup.memory_moves.len(),
        backup.memory_appends.len()
    );
    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }
    let sqlite = store;
    // `replace_org_nodes` は親が先に来る並びを要求する。`backup.nodes` は移行前の `org_list()` の
    // スナップショット（`position ASC, id ASC` の並び）なので、同じ position のノードが混じっている
    // と親が後に来ることがある。`migrate()` の `next.sort_by_key` と同じ規律で並べ直す。
    let mut restored_nodes = backup.nodes.clone();
    restored_nodes.sort_by_key(|n| match n.kind {
        task_core::OrgKind::Secretary => 0,
        task_core::OrgKind::Department => 1,
        task_core::OrgKind::Section => 2,
    });
    // 行を元に戻す（新しい値は写像で分かるので、記録した「元の値」をそのまま書き戻す）。
    for change in &backup.rows {
        sqlite.restore_node_ref(&change.table, &change.column, &change.id, &change.old)?;
    }
    // 記憶を戻す（追記は切り詰め、退避したディレクトリを戻す。移動は逆向き）。
    for append in &backup.memory_appends {
        if append.dest.is_file() {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&append.dest)
                .map_err(|e| CliError::msg(format!("open {}: {e}", append.dest.display())))?;
            file.set_len(append.original_len)
                .map_err(|e| CliError::msg(format!("truncate {}: {e}", append.dest.display())))?;
            if append.original_len == 0 {
                let _ = std::fs::remove_file(&append.dest);
            }
        }
        if append.saved_from.is_dir() && !append.original_dir.exists() {
            std::fs::rename(&append.saved_from, &append.original_dir).map_err(|e| {
                CliError::msg(format!("restore {}: {e}", append.original_dir.display()))
            })?;
        }
    }
    for mv in backup.memory_moves.iter().rev() {
        if mv.to.is_dir() && !mv.from.exists() {
            std::fs::rename(&mv.to, &mv.from)
                .map_err(|e| CliError::msg(format!("restore {}: {e}", mv.from.display())))?;
        }
    }
    sqlite.replace_org_nodes(&restored_nodes)?;
    outln!("done. the tree is back to the shape before migrate-v2");
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_map_is_deterministic_and_merges_the_poc_section() {
        let map = mapping();
        assert_eq!(map.get("secretary").map(String::as_str), Some("cos"));
        assert_eq!(
            map.get("coding-frontend").map(String::as_str),
            Some("software-engineering")
        );
        assert_eq!(
            map.get("coding-poc").map(String::as_str),
            Some("software-engineering")
        );
        assert_eq!(map.get("research").map(String::as_str), Some("research"));
        assert_eq!(map.get("infra").map(String::as_str), Some("infrastructure"));
        assert_eq!(map.len(), V1_TO_V2.len(), "写像の左辺は全部ちがう id");
        assert_eq!(
            map.values()
                .filter(|v| v.as_str() == "software-engineering")
                .count(),
            2,
            "coding-frontend と coding-poc は同じ行き先に合流する"
        );
    }
}
