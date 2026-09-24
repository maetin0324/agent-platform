//! Repository documentation lifecycle. Git snapshots are evidence; authority is explicit overlay.
use crate::changes::{GIT_WRITE_TIMEOUT, git};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Canonical,
    Architecture,
    Reference,
    Decision,
    ActivePlan,
    Historical,
    Generated,
    Residue,
    Duplicate,
    #[default]
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Observe,
    Conservative,
    Managed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub mode: Mode,
    #[serde(default)]
    pub categories: BTreeMap<String, Category>,
    #[serde(default)]
    pub authority: BTreeMap<String, String>,
    #[serde(default)]
    pub generated_sources: BTreeMap<String, String>,
    #[serde(default = "interval")]
    pub interval_hours: u64,
}
fn interval() -> u64 {
    168
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: Mode::Observe,
            categories: BTreeMap::new(),
            authority: BTreeMap::new(),
            generated_sources: BTreeMap::new(),
            interval_hours: interval(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub path: String,
    pub title: String,
    pub hash: String,
    pub category: Category,
    pub confidence: f32,
    pub evidence: Vec<String>,
    pub findings: Vec<String>,
    pub excerpt: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audit {
    pub revision: String,
    pub documents: Vec<Document>,
    pub conventions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Action {
    Rewrite {
        path: String,
        body: String,
    },
    Move {
        path: String,
        destination: String,
    },
    Archive {
        path: String,
        destination: String,
    },
    Delete {
        path: String,
    },
    Merge {
        paths: Vec<String>,
        destination: String,
        body: String,
    },
    Index {
        path: String,
        body: String,
    },
    ChooseCanonical {
        path: String,
        alternatives: Vec<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcilePlan {
    pub revision: String,
    pub expected: BTreeMap<String, String>,
    pub actions: Vec<Action>,
    pub rationale: Vec<String>,
}
pub fn digest(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
fn run(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo, args, GIT_WRITE_TIMEOUT).ok_or("git unavailable")?;
    if out.ok {
        Ok(out.stdout.trim_end().to_string())
    } else {
        Err(out.why())
    }
}
fn read_snapshot(repo: &Path, revision: &str, path: &str) -> Option<String> {
    let size = run(repo, &["cat-file", "-s", &format!("{revision}:{path}")])
        .ok()?
        .parse::<usize>()
        .ok()?;
    if size > crate::docs::MAX_PAGE_BYTES {
        return None;
    }
    let out = git(
        repo,
        &["show", &format!("{revision}:{path}")],
        GIT_WRITE_TIMEOUT,
    )?;
    out.ok.then_some(out.stdout)
}
fn safe(raw: &str) -> Result<String, String> {
    let p = crate::docs::page_path("", raw).map_err(|e| e.to_string())?;
    if p != raw || p.split('/').any(|s| s.starts_with('.')) {
        return Err("noncanonical or hidden document path".into());
    }
    Ok(p)
}
/// Read only committed regular Markdown files. Never reads symlink targets or changes the index.
pub fn audit(repo: &Path, reference: &str) -> Result<Audit, String> {
    let revision = run(
        repo,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )?;
    let tree = run(repo, &["ls-tree", "-rz", "--full-tree", &revision])?;
    let mut documents = Vec::new();
    let mut all = BTreeSet::new();
    let entries: Vec<_> = tree
        .split('\0')
        .filter_map(|line| line.split_once('\t'))
        .collect();
    for (_, path) in &entries {
        all.insert(path.to_string());
    }
    for (meta, path) in entries {
        if !meta.starts_with("100")
            || !path.to_ascii_lowercase().ends_with(".md")
            || safe(path).is_err()
        {
            continue;
        }
        if documents.len() >= crate::docs::MAX_TREE_ITEMS {
            return Err("document inventory limit exceeded".into());
        }
        let blob = meta.split_whitespace().nth(2).ok_or("missing blob hash")?;
        let size = run(repo, &["cat-file", "-s", blob])?
            .parse::<usize>()
            .map_err(|error| error.to_string())?;
        if size > crate::docs::MAX_PAGE_BYTES {
            documents.push(Document {
                path: path.into(),
                title: path.rsplit('/').next().unwrap_or(path).into(),
                hash: format!("git-blob:{blob}"),
                category: Category::Unknown,
                confidence: 0.0,
                evidence: vec![format!(
                    "content not loaded: {size} bytes exceeds audit read limit"
                )],
                findings: vec!["large_document".into()],
                excerpt: String::new(),
            });
            continue;
        }
        let raw = read_snapshot(repo, &revision, path)
            .ok_or_else(|| format!("unreadable document: {path}"))?;
        let lower = path.to_ascii_lowercase();
        let body = raw.to_ascii_lowercase();
        let (category, why) =
            if body.contains("do not edit") || body.contains("automatically generated") {
                (Category::Generated, "explicit generated marker")
            } else if lower.contains("/adr/") {
                (Category::Decision, "ADR directory convention")
            } else if lower.contains("architecture") {
                (Category::Architecture, "architecture filename convention")
            } else if lower.contains("reference") {
                (Category::Reference, "reference filename convention")
            } else if body.contains("status: completed") || body.contains("status: superseded") {
                (Category::Historical, "explicit completed/superseded status")
            } else if body.contains("status: active") {
                (Category::ActivePlan, "explicit active status")
            } else if lower.contains("experiment")
                || lower.contains("report")
                || lower.contains("debug")
            {
                (
                    Category::Residue,
                    "task/experiment filename; needs human assessment",
                )
            } else {
                (Category::Unknown, "location does not establish authority")
            };
        let mut findings = Vec::new();
        if raw.len() > 32 * 1024 {
            findings.push("large_document".into())
        }
        if category == Category::ActivePlan {
            let timestamp = run(repo, &["log", "-1", "--format=%ct", &revision, "--", path])?
                .parse::<i64>()
                .unwrap_or(0);
            if time::OffsetDateTime::now_utc().unix_timestamp() - timestamp > 90 * 86400 {
                findings.push("stale_active_plan".into())
            }
        }
        if category == Category::Generated {
            findings.push("generated_drift_unverified: no generator baseline".into())
        }
        for event in pulldown_cmark::Parser::new(&raw) {
            if let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) = event
            {
                let link = dest_url.split('#').next().unwrap_or("");
                if link.is_empty() || link.contains(':') || link.starts_with('/') {
                    continue;
                }
                if let Some(target) = crate::docs::resolve_relative("", path, link)
                    && !all.contains(&target)
                {
                    findings.push(format!("broken_link: {link}"))
                }
            }
        }
        documents.push(Document {
            path: path.into(),
            title: crate::docs::title_of(&raw, path),
            hash: digest(raw.as_bytes()),
            category,
            confidence: if category == Category::Unknown {
                0.0
            } else {
                0.7
            },
            evidence: vec![why.into()],
            findings,
            excerpt: raw.chars().take(4000).collect(),
        });
    }
    documents.sort_by(|a, b| a.path.cmp(&b.path));
    let mut titles: BTreeMap<String, usize> = BTreeMap::new();
    for doc in &documents {
        *titles.entry(doc.title.to_lowercase()).or_default() += 1;
    }
    for doc in &mut documents {
        if titles[&doc.title.to_lowercase()] > 1 {
            doc.findings
                .push("duplicate_title: canonical authority ambiguous".into());
            doc.evidence
                .push("same title occurs in multiple documents".into());
            doc.category = Category::Duplicate;
        }
    }
    let conventions = documents
        .iter()
        .filter(|d| {
            d.path.eq_ignore_ascii_case("README.md")
                || d.path.eq_ignore_ascii_case("CONTRIBUTING.md")
                || d.path.ends_with("AGENTS.md")
        })
        .map(|d| format!("{}: {}", d.path, d.title))
        .collect();
    Ok(Audit {
        revision,
        documents,
        conventions,
    })
}
pub fn proposal(audit: &Audit) -> ReconcilePlan {
    ReconcilePlan {
        revision: audit.revision.clone(),
        expected: audit
            .documents
            .iter()
            .map(|d| (d.path.clone(), d.hash.clone()))
            .collect(),
        actions: Vec::new(),
        rationale: audit
            .documents
            .iter()
            .filter(|d| !d.findings.is_empty() || d.category == Category::Unknown)
            .map(|d| format!("{}: {:?}; {}", d.path, d.category, d.findings.join(", ")))
            .collect(),
    }
}
pub fn bounded_review(audit: &Audit, pages: usize, chars: usize) -> String {
    let mut out = String::new();
    for d in audit
        .documents
        .iter()
        .filter(|d| !d.findings.is_empty())
        .take(pages.min(10))
    {
        let text = format!(
            "\n{} {:?}\n{}\n{}\n",
            d.path,
            d.category,
            d.findings.join(", "),
            d.excerpt
        );
        out.extend(text.chars().take(chars.saturating_sub(out.chars().count())));
        if out.chars().count() >= chars {
            break;
        }
    }
    out
}
fn state_path(state: &Path, id: &str) -> PathBuf {
    state.join("repository-docs").join(digest(id.as_bytes()))
}
pub fn save_policy(state: &Path, id: &str, policy: &Policy) -> Result<(), String> {
    for path in policy
        .categories
        .keys()
        .chain(policy.authority.keys())
        .chain(policy.generated_sources.keys())
    {
        safe(path)?;
    }
    if policy.interval_hours == 0 {
        return Err("interval must be positive".into());
    }
    let dir = state_path(state, id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("policy.json"),
        serde_json::to_vec_pretty(policy).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
pub fn load_policy(state: &Path, id: &str) -> Result<Policy, String> {
    match std::fs::read(state_path(state, id).join("policy.json")) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Policy::default()),
        Err(error) => Err(error.to_string()),
    }
}
pub fn plan_id(plan: &ReconcilePlan) -> Result<String, String> {
    Ok(digest(
        &serde_json::to_vec(plan).map_err(|e| e.to_string())?,
    ))
}
/// Called only from a human-facing approval endpoint/CLI, never by maintenance.
pub fn approve_plan(state: &Path, id: &str, plan: &ReconcilePlan) -> Result<(), String> {
    let dir = state_path(state, id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join(format!("approval-{}.json", plan_id(plan)?)),
        serde_json::to_vec(plan).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
/// Apply an exact human-approved plan on a new worktree and leave the commit for normal review/merge.
pub fn apply_plan(
    repo: &Path,
    default_branch: &str,
    worktree: &Path,
    plan: &ReconcilePlan,
    state: &Path,
    id: &str,
) -> Result<String, String> {
    let approved =
        std::fs::read(state_path(state, id).join(format!("approval-{}.json", plan_id(plan)?)))
            .map_err(|_| "human approval required")?;
    if approved != serde_json::to_vec(plan).map_err(|e| e.to_string())? {
        return Err("approval mismatch".into());
    }
    if !run(repo, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err("repository has uncommitted edits".into());
    }
    if run(repo, &["rev-parse", default_branch])? != plan.revision {
        return Err("stale approval: branch changed".into());
    }
    if worktree.exists() {
        return Err("isolated worktree must not exist".into());
    }
    let current = audit(repo, default_branch)?;
    let expected: BTreeMap<_, _> = current
        .documents
        .iter()
        .map(|d| (d.path.clone(), d.hash.clone()))
        .collect();
    if expected != plan.expected {
        return Err("stale document snapshot".into());
    }
    // Construct the entire final edit set before creating a worktree. No partial validation writes.
    let mut edits: BTreeMap<String, Option<String>> = BTreeMap::new();
    let existing = |path: &str| -> Result<String, String> {
        safe(path)?;
        if !plan.expected.contains_key(path) {
            return Err(format!("missing input: {path}"));
        }
        read_snapshot(repo, &plan.revision, path).ok_or("missing document".into())
    };
    for action in &plan.actions {
        match action {
            Action::Rewrite { path, body } => {
                existing(path)?;
                edits.insert(safe(path)?, Some(body.clone()));
            }
            Action::Index { path, body } => {
                safe(path)?;
                if plan.expected.contains_key(path) {
                    return Err("index destination exists".into());
                }
                edits.insert(path.clone(), Some(body.clone()));
            }
            Action::Delete { path } => {
                existing(path)?;
                edits.insert(path.clone(), None);
            }
            Action::Move { path, destination } | Action::Archive { path, destination } => {
                let body = existing(path)?;
                safe(destination)?;
                if plan.expected.contains_key(destination) {
                    return Err("destination exists".into());
                }
                edits.insert(path.clone(), None);
                edits.insert(destination.clone(), Some(body));
            }
            Action::Merge {
                paths,
                destination,
                body,
            } => {
                safe(destination)?;
                if paths.is_empty() {
                    return Err("merge inputs required".into());
                }
                for path in paths {
                    existing(path)?;
                    edits.insert(path.clone(), None);
                }
                if plan.expected.contains_key(destination) && !paths.contains(destination) {
                    return Err("merge overwrites unrelated document".into());
                }
                edits.insert(destination.clone(), Some(body.clone()));
            }
            Action::ChooseCanonical { .. } => {
                return Err("authority decisions belong in the approved policy overlay".into());
            }
        }
    }
    if edits.is_empty() {
        return Err("plan contains no edits".into());
    }
    for body in edits.values().flatten() {
        if body.len() > crate::docs::MAX_PAGE_BYTES {
            return Err("document too large".into());
        }
    }
    // Preflight paths against all tracked entries, including non-document symlinks.
    let tree = run(repo, &["ls-tree", "-rz", "--full-tree", &plan.revision])?;
    let entries: Vec<_> = tree
        .split('\0')
        .filter_map(|line| line.split_once('\t'))
        .collect();
    for (path, body) in &edits {
        for (meta, tracked) in &entries {
            if (*tracked == path || path.starts_with(&format!("{tracked}/")))
                && meta.starts_with("120000")
            {
                return Err("symlink destination".into());
            }
            if body.is_some() && *tracked == path && !plan.expected.contains_key(path) {
                return Err("destination exists outside document inventory".into());
            }
        }
    }
    let branch = format!("docs-reconcile/{}", &plan_id(plan)?[..16]);
    run(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &worktree.to_string_lossy(),
            &plan.revision,
        ],
    )?;
    for (path, body) in edits {
        let target = worktree.join(&path);
        // Do not follow any tracked symlink in a destination's parent chain.
        let mut ancestor = target.parent();
        while let Some(p) = ancestor {
            if p == worktree {
                break;
            }
            if p.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err("symlink destination".into());
            }
            ancestor = p.parent();
        }
        if target
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("symlink destination".into());
        }
        if let Some(body) = body {
            std::fs::create_dir_all(target.parent().ok_or("missing parent")?)
                .map_err(|e| e.to_string())?;
            std::fs::write(target, body).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(target).map_err(|e| e.to_string())?;
        }
    }
    run(worktree, &["add", "--all"])?;
    run(
        worktree,
        &[
            "-c",
            "user.name=Celeris",
            "-c",
            "user.email=celeris@local",
            "commit",
            "-m",
            "文書の承認済み整理案を隔離ブランチへ適用",
        ],
    )?;
    run(worktree, &["rev-parse", "HEAD"])
}
/// Automated publication only permits explicitly canonical, human-useful current documentation.
pub fn may_publish(category: Category, human_useful: bool, explicitly_approved: bool) -> bool {
    human_useful
        && explicitly_approved
        && matches!(
            category,
            Category::Canonical | Category::Architecture | Category::Reference | Category::Decision
        )
}

pub fn state_root() -> PathBuf {
    std::env::var_os("CELERIS_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/celeris")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        crate::docs::init_docs_repo(&repo, "Fixture").unwrap();
        for (p, b) in [
            ("docs/a.md", "# Repeated\n[missing](missing.md)\n"),
            ("docs/b.md", "# Repeated\n"),
            ("docs/unknown.md", "# Undecided\n"),
            ("docs/adr/1.md", "# Decision\n"),
            ("docs/old.md", "# Old\nstatus: superseded\n"),
            ("AGENTS.md", "# Instructions\n"),
        ] {
            let p = repo.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b).unwrap();
        }
        run(&repo, &["add", "."]).unwrap();
        run(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@local",
                "commit",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        (dir, repo)
    }
    #[test]
    fn audit_is_read_only_and_authority_stays_unknown() {
        let (_dir, repo) = fixture();
        std::fs::write(repo.join("docs/unknown.md"), "human dirty content").unwrap();
        std::fs::write(repo.join("untracked.md"), "human draft").unwrap();
        let status = run(&repo, &["status", "--porcelain"]).unwrap();
        let first = audit(&repo, "main").unwrap();
        let second = audit(&repo, "main").unwrap();
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        assert_eq!(status, run(&repo, &["status", "--porcelain"]).unwrap());
        assert_eq!(
            std::fs::read_to_string(repo.join("docs/unknown.md")).unwrap(),
            "human dirty content"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("untracked.md")).unwrap(),
            "human draft"
        );
        assert_eq!(
            first
                .documents
                .iter()
                .find(|d| d.path == "docs/unknown.md")
                .unwrap()
                .category,
            Category::Unknown
        );
        assert!(
            first
                .documents
                .iter()
                .any(|d| d.category == Category::Duplicate && !d.evidence.is_empty())
        );
        assert!(
            first
                .documents
                .iter()
                .any(|d| d.category == Category::Historical)
        );
        assert!(
            first
                .documents
                .iter()
                .any(|d| d.findings.iter().any(|f| f.starts_with("broken_link")))
        );
        assert!(bounded_review(&first, 1, 50).chars().count() <= 50);
    }
    #[test]
    fn exact_human_approval_and_isolated_worktree_are_required() {
        let (dir, repo) = fixture();
        let state = dir.path().join("state");
        let wt = dir.path().join("wt");
        let audit = audit(&repo, "main").unwrap();
        let mut plan = proposal(&audit);
        plan.actions.push(Action::Delete {
            path: "docs/b.md".into(),
        });
        assert!(
            apply_plan(&repo, "main", &wt, &plan, &state, "r")
                .unwrap_err()
                .contains("approval")
        );
        approve_plan(&state, "r", &plan).unwrap();
        assert!(apply_plan(&repo, "main", &repo, &plan, &state, "r").is_err());
        std::fs::write(repo.join("untracked.md"), "draft").unwrap();
        assert!(
            apply_plan(&repo, "main", &wt, &plan, &state, "r")
                .unwrap_err()
                .contains("uncommitted")
        );
        std::fs::remove_file(repo.join("untracked.md")).unwrap();
        let mut altered = plan.clone();
        altered.actions.push(Action::Delete {
            path: "docs/a.md".into(),
        });
        assert!(apply_plan(&repo, "main", &wt, &altered, &state, "r").is_err());
        let sha = apply_plan(&repo, "main", &wt, &plan, &state, "r").unwrap();
        assert_ne!(sha, audit.revision);
        assert!(repo.join("docs/b.md").exists());
        assert!(!wt.join("docs/b.md").exists());
        assert_eq!(run(&repo, &["rev-parse", "main"]).unwrap(), audit.revision);
    }
    #[test]
    fn stale_approval_rejected_overlay_does_not_change_repo() {
        let (dir, repo) = fixture();
        let state = dir.path().join("state");
        let before = run(&repo, &["status", "--porcelain"]).unwrap();
        let p = Policy {
            mode: Mode::Managed,
            ..Policy::default()
        };
        save_policy(&state, "r", &p).unwrap();
        assert_eq!(load_policy(&state, "r").unwrap().mode, Mode::Managed);
        assert_eq!(before, run(&repo, &["status", "--porcelain"]).unwrap());
        let mut plan = proposal(&audit(&repo, "main").unwrap());
        plan.actions.push(Action::Delete {
            path: "docs/a.md".into(),
        });
        approve_plan(&state, "r", &plan).unwrap();
        std::fs::write(repo.join("docs/a.md"), "# Changed").unwrap();
        run(&repo, &["add", "."]).unwrap();
        run(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@local",
                "commit",
                "-m",
                "change",
            ],
        )
        .unwrap();
        assert!(
            apply_plan(&repo, "main", &dir.path().join("wt"), &plan, &state, "r")
                .unwrap_err()
                .contains("stale")
        );
        assert!(!may_publish(Category::Residue, true, true));
        assert!(!may_publish(Category::Unknown, true, true));
        assert!(!may_publish(Category::Canonical, true, false));
        assert!(may_publish(Category::Canonical, true, true));
    }
}

/// Apply adopted metadata, and flag a generated output only when its declared source changed
/// after the output's latest commit. This is a drift candidate, not proof of generator output.
pub fn audit_with_policy(repo: &Path, reference: &str, policy: &Policy) -> Result<Audit, String> {
    let mut result = audit(repo, reference)?;
    for doc in &mut result.documents {
        if let Some(category) = policy.categories.get(&doc.path) {
            doc.category = *category;
            doc.confidence = 1.0;
            doc.evidence.push("adopted repository policy".into());
        }
        if let Some(authority) = policy.authority.get(&doc.path) {
            doc.evidence
                .push(format!("explicit authority: {authority}"));
        }
        if let Some(source) = policy.generated_sources.get(&doc.path) {
            if Path::new(source).is_absolute()
                || Path::new(source)
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("unsafe generated source path".into());
            }
            let output_commit = run(
                repo,
                &[
                    "log",
                    "-1",
                    "--format=%H",
                    &result.revision,
                    "--",
                    &doc.path,
                ],
            )?;
            let changes = run(
                repo,
                &[
                    "log",
                    "--format=%H",
                    &format!("{output_commit}..{}", result.revision),
                    "--",
                    source,
                ],
            )?;
            doc.evidence
                .push(format!("declared generator source: {source}"));
            if !changes.is_empty() {
                doc.findings
                    .push("generated_drift_candidate: source changed since output commit".into());
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod extended_tests {
    use super::*;
    #[test]
    fn declared_generator_drift_and_all_classifications() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        crate::docs::init_docs_repo(&repo, "Fixture").unwrap();
        for (p, b) in [
            ("docs/architecture.md", "# Architecture\n"),
            ("docs/reference.md", "# Reference\n"),
            ("docs/report.md", "# Experiment\n"),
            ("docs/generated.md", "# Generated\ndo not edit\n"),
            ("docs/plan.md", "# Work\nstatus: active\n"),
            ("generate.rs", "// generator\n"),
        ] {
            std::fs::write(repo.join(p), b).unwrap();
        }
        run(&repo, &["add", "."]).unwrap();
        run(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@local",
                "commit",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        std::fs::write(repo.join("generate.rs"), "// changed generator\n").unwrap();
        run(&repo, &["add", "."]).unwrap();
        run(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@local",
                "commit",
                "-m",
                "generator changed",
            ],
        )
        .unwrap();
        let mut policy = Policy::default();
        policy
            .categories
            .insert("docs/README.md".into(), Category::Canonical);
        policy
            .generated_sources
            .insert("docs/generated.md".into(), "generate.rs".into());
        let audit = audit_with_policy(&repo, "main", &policy).unwrap();
        for category in [
            Category::Canonical,
            Category::Architecture,
            Category::Reference,
            Category::Residue,
            Category::Generated,
            Category::ActivePlan,
        ] {
            assert!(
                audit.documents.iter().any(|d| d.category == category),
                "{category:?}"
            );
        }
        assert!(audit.documents.iter().any(|d| {
            d.findings
                .iter()
                .any(|f| f.starts_with("generated_drift_candidate"))
        }));
    }
    #[test]
    fn approved_merge_move_and_index_leave_default_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        crate::docs::init_docs_repo(&repo, "Fixture").unwrap();
        let before = audit(&repo, "main").unwrap();
        let mut plan = proposal(&before);
        plan.actions.push(Action::Move {
            path: "docs/README.md".into(),
            destination: "docs/guide.md".into(),
        });
        plan.actions.push(Action::Index {
            path: "docs/index.md".into(),
            body: "# Index\n[Guide](guide.md)\n".into(),
        });
        let state = dir.path().join("state");
        approve_plan(&state, "r", &plan).unwrap();
        let wt = dir.path().join("wt");
        apply_plan(&repo, "main", &wt, &plan, &state, "r").unwrap();
        assert!(wt.join("docs/guide.md").exists());
        assert!(wt.join("docs/index.md").exists());
        assert!(repo.join("docs/README.md").exists());
        assert!(!repo.join("docs/index.md").exists());
    }
    #[test]
    fn traversal_and_symlink_destinations_are_rejected_before_writes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        crate::docs::init_docs_repo(&repo, "Fixture").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), repo.join("escape")).unwrap();
            run(&repo, &["add", "escape"]).unwrap();
            run(
                &repo,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@local",
                    "commit",
                    "-m",
                    "symlink fixture",
                ],
            )
            .unwrap();
        }
        let state = dir.path().join("state");
        let wt = dir.path().join("wt");
        for path in ["../outside.md", "escape/outside.md"] {
            let mut plan = proposal(&audit(&repo, "main").unwrap());
            plan.actions.push(Action::Index {
                path: path.into(),
                body: "# no".into(),
            });
            approve_plan(&state, "r", &plan).unwrap();
            assert!(apply_plan(&repo, "main", &wt, &plan, &state, "r").is_err());
            assert!(!wt.exists());
            assert!(!dir.path().join("outside.md").exists());
        }
        let audit = audit(&repo, "main").unwrap();
        assert!(bounded_review(&audit, 10, 100).is_empty());
    }
    #[test]
    fn oversized_document_is_inventoried_without_loading_or_blocking_other_pages() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        crate::docs::init_docs_repo(&repo, "Fixture").unwrap();
        let large = "x".repeat(crate::docs::MAX_PAGE_BYTES + 1);
        std::fs::write(repo.join("docs/huge.md"), &large).unwrap();
        run(&repo, &["add", "."]).unwrap();
        run(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@local",
                "commit",
                "-m",
                "large fixture",
            ],
        )
        .unwrap();
        std::fs::write(repo.join("untracked.md"), "human draft").unwrap();
        let before = run(&repo, &["status", "--porcelain"]).unwrap();
        let first = audit(&repo, "main").unwrap();
        let second = audit(&repo, "main").unwrap();
        let doc = first
            .documents
            .iter()
            .find(|doc| doc.path == "docs/huge.md")
            .unwrap();
        assert!(
            doc.findings
                .iter()
                .any(|finding| finding == "large_document")
        );
        assert_eq!(doc.category, Category::Unknown);
        assert!(doc.excerpt.is_empty());
        assert!(doc.hash.starts_with("git-blob:"));
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        assert!(
            first
                .documents
                .iter()
                .any(|doc| doc.path == "docs/README.md")
        );
        assert_eq!(before, run(&repo, &["status", "--porcelain"]).unwrap());
        assert_eq!(
            std::fs::read_to_string(repo.join("docs/huge.md")).unwrap(),
            large
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("untracked.md")).unwrap(),
            "human draft"
        );
    }
}
