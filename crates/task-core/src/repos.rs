//! 案件のリポジトリ（ADR-0043 D1 / D2）。純粋なデータ定義と決定的な検証だけを置く
//! （I/O・LLM 呼び出しはしない。ADR-0001 D2 / DESIGN 原則 1）。
//!
//! ADR-0039 は案件に作業場所を 1 つだけ持たせた（`projects.workspace`）。しかし 1 案件が
//! 複数のリポジトリ（コードの `benchfs` と論文の `benchfs-paper`、git ではないデータの置き場）を
//! 跨ぐことがある。そこで案件は **`project_repos` を複数持つ**ようにし、タスクはそのうち使うものを
//! `Task.repos` で選ぶ。`is_primary` のリポジトリが「主なリポジトリ」で、`Project.workspace` は
//! その写しを返す（GUI の後方互換）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::model::WorkspaceSpec;
use crate::org::ProjectId;

/// リポジトリの一意識別子（ULID）。`TaskId` / `ProjectId` と同じ形。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct RepoId(#[schemars(with = "String")] pub Ulid);

impl RepoId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for RepoId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for RepoId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// リポジトリの種類（ADR-0043 D1）。`git` はタスクごとに worktree を切る。`dir` は
/// シンボリックリンクで見せる（コピーしない。大きいデータを想定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepoKind {
    Git,
    Dir,
}

impl RepoKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoKind::Git => "git",
            RepoKind::Dir => "dir",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "git" => Some(RepoKind::Git),
            "dir" => Some(RepoKind::Dir),
            _ => None,
        }
    }
}

/// 実行環境（ADR-0043 D1 / D3）。`auto` は `workspace.toml` の `[run] mode` に従い、無ければ `host`。
/// **`container` は ADR-0043 A3 の工事**（この Phase では読むだけで、実行には使わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepoRun {
    #[default]
    Auto,
    Host,
    Container,
}

impl RepoRun {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoRun::Auto => "auto",
            RepoRun::Host => "host",
            RepoRun::Container => "container",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(RepoRun::Auto),
            "host" => Some(RepoRun::Host),
            "container" => Some(RepoRun::Container),
            _ => None,
        }
    }
}

/// リモートのリポジトリの同期方式（ADR-0043 D1。ADR-0019 の `sync` と同じ語彙）。
/// 省略（`None`）は既定の `worktree`（ADR-0019 の (a)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepoSync {
    Worktree,
    Rsync,
    None,
}

impl RepoSync {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoSync::Worktree => "worktree",
            RepoSync::Rsync => "rsync",
            RepoSync::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "worktree" => Some(RepoSync::Worktree),
            "rsync" => Some(RepoSync::Rsync),
            "none" => Some(RepoSync::None),
            _ => None,
        }
    }
}

/// 案件のリポジトリ 1 件（`project_repos` の 1 行。ADR-0043 D1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRepo {
    pub id: RepoId,
    pub project_id: ProjectId,
    /// 案件の中で一意の slug（既定はディレクトリ名）。タスクの作業場所では
    /// `<workspace_root>/<task_id>/repos/<name>/` というディレクトリ名になるので、
    /// `valid_repo_name` を通ったものだけを受ける。
    pub name: String,
    pub kind: RepoKind,
    /// `{"kind":"local","path":"/abs"}` / `{"kind":"remote","cluster":"pegasus","path":"/abs"}`。
    /// `Local` の `~` は保存する前に展開する（ADR-0039 D5）。
    pub location: WorkspaceSpec,
    /// git のみ。無ければ検出（origin/HEAD → main → master）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// remote のみ。省略は既定の `worktree`（ADR-0019 の (a)）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<RepoSync>,
    #[serde(default)]
    pub run: RepoRun,
    /// 案件の「主なリポジトリ」。1 案件に 1 つ（成果物と文書の既定の置き場。ADR-0044 D7）。
    #[serde(default)]
    pub is_primary: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
}

/// タスクが使うリポジトリの参照（`tasks.repos_json` の 1 要素。ADR-0043 D2）。
/// `name` も持つのは、リポジトリの行が消えた後でも前置きと GUI が名前を出せるようにするため。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoRef {
    pub repo_id: RepoId,
    pub name: String,
}

impl RepoRef {
    pub fn of(repo: &ProjectRepo) -> Self {
        Self {
            repo_id: repo.id,
            name: repo.name.clone(),
        }
    }
}

/// リポジトリの検証の失敗（ADR-0043 D1）。ストアが `StoreError::Invalid` に包み、API は 422 にする。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepoError {
    #[error(
        "repo name must be a lowercase slug ([a-z0-9._-], 1..64 chars, not starting with '.' or '-'): {0:?}"
    )]
    InvalidName(String),
    #[error("repo name {0:?} is already used in this project")]
    DuplicateName(String),
    #[error("repo location path must be absolute (after ~ expansion): {0:?}")]
    RelativePath(String),
    #[error("default_branch is only meaningful for kind = git")]
    BranchOnDir,
    #[error("sync is only meaningful for a remote location")]
    SyncOnLocal,
    #[error(
        "remote repositories with sync = \"none\" and run = \"remote\" are not supported yet (ADR-0043 D7)"
    )]
    RemoteBUnsupported,
    #[error("task repo {0:?} is not one of this project's repositories")]
    UnknownName(String),
    #[error("a task cannot mix a remote repository with other repositories yet (ADR-0043 D2)")]
    MixedLocalAndRemote,
}

/// 案件内で一意の slug。`repos/<name>/` というディレクトリ名になるので、ここが境界である
/// （`..`・`/`・空文字・先頭の `.` を通さない）。
pub fn valid_repo_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.'
        })
        && !name.starts_with('.')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name != "."
        && name != ".."
}

/// パスの末尾から既定の `name` を作る（ADR-0043 D1「既定はディレクトリ名」）。
/// 使えない文字は `-` に潰し、大文字は小文字にする。何も残らなければ `"repo"`。
pub fn default_repo_name(location: &WorkspaceSpec) -> String {
    let path = match location {
        WorkspaceSpec::Local { path, .. } => path,
        WorkspaceSpec::Remote { path, .. } => path,
    };
    let raw = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    slugify_repo_name(&raw)
}

/// `default_repo_name` の中身（テストと migration の backfill が使う）。
pub fn slugify_repo_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').trim_start_matches('.').to_string();
    let out: String = out.chars().take(64).collect();
    let out = out.trim_end_matches('-').to_string();
    if valid_repo_name(&out) {
        out
    } else {
        "repo".to_string()
    }
}

/// `existing`（その案件の既存の行。`repo.id` の行があればそれも含む）に対して `repo` を upsert して
/// よいかを決定的に検証する（ADR-0043 D1 / D7）。LLM も I/O も使わない。
pub fn validate_upsert(existing: &[ProjectRepo], repo: &ProjectRepo) -> Result<(), RepoError> {
    if !valid_repo_name(&repo.name) {
        return Err(RepoError::InvalidName(repo.name.clone()));
    }
    if existing
        .iter()
        .any(|r| r.name == repo.name && r.id != repo.id)
    {
        return Err(RepoError::DuplicateName(repo.name.clone()));
    }
    match &repo.location {
        WorkspaceSpec::Local { path, .. } => {
            if !path.is_absolute() {
                return Err(RepoError::RelativePath(path.to_string_lossy().into_owned()));
            }
            if repo.sync.is_some() {
                return Err(RepoError::SyncOnLocal);
            }
        }
        WorkspaceSpec::Remote { path, .. } => {
            if !path.is_absolute() {
                return Err(RepoError::RelativePath(path.to_string_lossy().into_owned()));
            }
            // ADR-0043 D7: リモート (b)（ログインノードで編集も実行も）は予約。この Phase では 422。
            if repo.sync == Some(RepoSync::None) {
                return Err(RepoError::RemoteBUnsupported);
            }
        }
    }
    if repo.kind == RepoKind::Dir && repo.default_branch.is_some() {
        return Err(RepoError::BranchOnDir);
    }
    Ok(())
}

/// ADR-0043 D2: タスクが選んだリポジトリの名前を、その案件のリポジトリに突き合わせる（決定的）。
///
/// - 知らない名前は `UnknownName`（API は 422、計画は差し戻し）
/// - **リモートのリポジトリを他と混ぜたタスクは `MixedLocalAndRemote`**。リモートは従来の
///   ADR-0018 / ADR-0019 の経路（手元の写し + rsync）をそのまま使うので、この Phase では
///   1 つだけのときに限る
pub fn resolve_task_repos(
    available: &[ProjectRepo],
    names: &[String],
) -> Result<Vec<RepoRef>, RepoError> {
    let mut out = Vec::with_capacity(names.len());
    let mut remote = 0usize;
    for name in names {
        let name = name.trim();
        let Some(repo) = available.iter().find(|r| r.name == name) else {
            return Err(RepoError::UnknownName(name.to_string()));
        };
        if matches!(repo.location, WorkspaceSpec::Remote { .. }) {
            remote += 1;
        }
        if !out.iter().any(|r: &RepoRef| r.repo_id == repo.id) {
            out.push(RepoRef::of(repo));
        }
    }
    if remote > 0 && out.len() > 1 {
        return Err(RepoError::MixedLocalAndRemote);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo(name: &str, location: WorkspaceSpec) -> ProjectRepo {
        ProjectRepo {
            id: RepoId::new(),
            project_id: ProjectId::new(),
            name: name.into(),
            kind: RepoKind::Git,
            location,
            default_branch: None,
            sync: None,
            run: RepoRun::Auto,
            is_primary: false,
            created_at: OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn repo_names_are_directory_safe_slugs() {
        for ok in [
            "benchfs",
            "benchfs-paper",
            "agent_platform",
            "a",
            "v1.2",
            "x0",
        ] {
            assert!(valid_repo_name(ok), "{ok}");
        }
        for bad in [
            "",
            "..",
            ".",
            ".hidden",
            "-lead",
            "trail-",
            "Upper",
            "with space",
            "a/b",
            &"x".repeat(65),
        ] {
            assert!(!valid_repo_name(bad), "{bad}");
        }
    }

    #[test]
    fn the_default_name_comes_from_the_directory_name() {
        assert_eq!(
            default_repo_name(&WorkspaceSpec::local("/home/u/workspace/benchfs")),
            "benchfs"
        );
        assert_eq!(
            default_repo_name(&WorkspaceSpec::Remote {
                cluster: "pegasus".into(),
                path: PathBuf::from("/work/NBB/rmaeda/BenchFS Paper"),
            }),
            "benchfs-paper"
        );
        // 何も残らなければ `repo`。
        assert_eq!(slugify_repo_name("///"), "repo");
        assert_eq!(slugify_repo_name(""), "repo");
    }

    /// ADR-0043 D2: 名前でリポジトリを選ぶ。知らない名前は 422、リモートと他の混在も 422。
    #[test]
    fn task_repos_are_resolved_by_name_and_remote_cannot_be_mixed() {
        let mut code = repo("benchfs", WorkspaceSpec::local("/srv/benchfs"));
        code.is_primary = true;
        let paper = repo("benchfs-paper", WorkspaceSpec::local("/srv/benchfs-paper"));
        let mut cluster = repo(
            "remote",
            WorkspaceSpec::Remote {
                cluster: "pegasus".into(),
                path: PathBuf::from("/work/x"),
            },
        );
        cluster.project_id = code.project_id;
        let available = vec![code.clone(), paper.clone(), cluster.clone()];

        let picked = resolve_task_repos(&available, &["benchfs".into(), "benchfs-paper".into()])
            .expect("resolve");
        assert_eq!(picked, vec![RepoRef::of(&code), RepoRef::of(&paper)]);
        // 同じ名前を 2 回書いても 1 件。
        assert_eq!(
            resolve_task_repos(&available, &["benchfs".into(), "benchfs".into()]).expect("resolve"),
            vec![RepoRef::of(&code)]
        );
        assert_eq!(
            resolve_task_repos(&available, &["nope".into()]),
            Err(RepoError::UnknownName("nope".into()))
        );
        // リモート 1 つだけなら通る（ADR-0018 / 0019 の従来の経路）。
        assert_eq!(
            resolve_task_repos(&available, &["remote".into()]).expect("resolve"),
            vec![RepoRef::of(&cluster)]
        );
        assert_eq!(
            resolve_task_repos(&available, &["remote".into(), "benchfs".into()]),
            Err(RepoError::MixedLocalAndRemote)
        );
        assert_eq!(
            resolve_task_repos(&available, &[]).expect("resolve"),
            Vec::new()
        );
    }

    #[test]
    fn upsert_rejects_duplicates_relative_paths_and_reserved_combinations() {
        let a = repo("benchfs", WorkspaceSpec::local("/srv/benchfs"));
        let mut b = repo("benchfs", WorkspaceSpec::local("/srv/other"));
        assert_eq!(
            validate_upsert(std::slice::from_ref(&a), &b),
            Err(RepoError::DuplicateName("benchfs".into()))
        );
        b.name = "other".into();
        assert_eq!(validate_upsert(std::slice::from_ref(&a), &b), Ok(()));
        // 自分自身との衝突は起きない（更新）。
        let mut same = a.clone();
        same.location = WorkspaceSpec::local("/srv/moved");
        assert_eq!(validate_upsert(std::slice::from_ref(&a), &same), Ok(()));

        let relative = repo("x", WorkspaceSpec::local("relative/path"));
        assert!(matches!(
            validate_upsert(&[], &relative),
            Err(RepoError::RelativePath(_))
        ));

        let mut local_sync = repo("x", WorkspaceSpec::local("/srv/x"));
        local_sync.sync = Some(RepoSync::Rsync);
        assert_eq!(
            validate_upsert(&[], &local_sync),
            Err(RepoError::SyncOnLocal)
        );

        let mut remote_b = repo(
            "x",
            WorkspaceSpec::Remote {
                cluster: "pegasus".into(),
                path: PathBuf::from("/work/x"),
            },
        );
        remote_b.sync = Some(RepoSync::None);
        assert_eq!(
            validate_upsert(&[], &remote_b),
            Err(RepoError::RemoteBUnsupported)
        );
        remote_b.sync = Some(RepoSync::Worktree);
        assert_eq!(validate_upsert(&[], &remote_b), Ok(()));

        let mut dir_branch = repo("x", WorkspaceSpec::local("/srv/data"));
        dir_branch.kind = RepoKind::Dir;
        dir_branch.default_branch = Some("main".into());
        assert_eq!(
            validate_upsert(&[], &dir_branch),
            Err(RepoError::BranchOnDir)
        );
    }
}
