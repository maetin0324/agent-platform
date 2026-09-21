//! `celerisctl projects ls` / `celerisctl projects show <id>`（ADR-0054 D2。Phase 68）。
//!
//! CoS の対話 run に許す**読み取りだけの道具**の一部（`docs/adr/0054-stateful-sessions-and-streaming-chat.md`
//! D2: 「celerisctl knowledge search|get、タスク・案件の一覧と詳細の read API」）。タスクの一覧・詳細
//! （`celerisctl ls` / `celerisctl show`）は既にあったが、**案件**の一覧・詳細は無かったので足した。
//! `ls`/`show`（`query.rs`）と同じ流儀（DB を開いて読むだけ、書き込みは一切しない、プレーンテキスト出力）。

use std::process::ExitCode;

use clap::{Args, Subcommand};
use task_core::{MilestoneStatus, ProjectId, TaskStore};

use crate::error::CliError;
use crate::outln;

#[derive(Subcommand, Debug)]
pub enum ProjectsCommand {
    /// 案件の一覧（id・状態・題名）。
    Ls(ProjectsLsArgs),
    /// 1 件の案件の詳細（依頼文・状態・途中目標）。
    Show(ProjectsShowArgs),
}

#[derive(Args, Debug)]
pub struct ProjectsLsArgs {
    /// アーカイブ済みの案件も出す（既定は隠す。`GET /projects` と同じ既定）。
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct ProjectsShowArgs {
    pub id: String,
}

pub fn run(store: &dyn TaskStore, command: ProjectsCommand) -> Result<ExitCode, CliError> {
    match command {
        ProjectsCommand::Ls(args) => run_ls(store, args),
        ProjectsCommand::Show(args) => run_show(store, args),
    }
}

fn run_ls(store: &dyn TaskStore, args: ProjectsLsArgs) -> Result<ExitCode, CliError> {
    let mut projects = store.project_list()?;
    if !args.all {
        projects.retain(|p| p.archived_at.is_none());
    }
    projects.sort_by_key(|p| p.created_at);
    for project in &projects {
        outln!(
            "{} {:?} {}",
            project.id,
            project.status,
            project.title
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn run_show(store: &dyn TaskStore, args: ProjectsShowArgs) -> Result<ExitCode, CliError> {
    let id: ProjectId = args
        .id
        .parse()
        .map_err(|_| CliError::Message(format!("`{}` is not a project id (ULID)", args.id)))?;
    let project = store
        .project_get(id)?
        .ok_or_else(|| CliError::Message(format!("project not found: {}", args.id)))?;

    outln!("id: {}", project.id);
    outln!("title: {}", project.title);
    outln!("status: {:?}", project.status);
    outln!("request: {}", project.request);
    outln!(
        "secretary_summary: {}",
        project.secretary_summary.as_deref().unwrap_or("(none)")
    );
    match project.archived_at {
        Some(at) => outln!("archived_at: {at}"),
        None => outln!("archived_at: (none)"),
    }

    outln!("milestones:");
    for milestone in store.milestone_list(id)? {
        let mark = match milestone.status {
            MilestoneStatus::Proposed => "proposed",
            MilestoneStatus::Approved => "approved",
            MilestoneStatus::InProgress => "in_progress",
            MilestoneStatus::Reached => "reached",
            MilestoneStatus::Redesigned => "redesigned",
            MilestoneStatus::Paused => "paused",
            MilestoneStatus::Cancelled => "cancelled",
        };
        outln!("  - [{mark}] {} — {}", milestone.id, milestone.title);
    }

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Project, ProjectStatus, SqliteStore};
    use time::OffsetDateTime;

    fn sample_project(status: ProjectStatus, archived: bool) -> Project {
        let now = OffsetDateTime::now_utc();
        Project {
            id: ProjectId::new(),
            title: "Pluvio".into(),
            request: "降水予測の研究".into(),
            status,
            secretary_summary: None,
            workspace: None,
            archived_at: if archived { Some(now) } else { None },
            paused_from: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// 読み取り専用: `ls` は既定でアーカイブ済みを隠し、`--all` で出す。
    #[test]
    fn run_ls_hides_archived_projects_unless_all() {
        let store = SqliteStore::open_in_memory().expect("open store");
        store
            .project_create(&sample_project(ProjectStatus::Active, false))
            .expect("create");
        store
            .project_create(&sample_project(ProjectStatus::Active, true))
            .expect("create");

        assert!(run(&store, ProjectsCommand::Ls(ProjectsLsArgs { all: false })).is_ok());
        assert!(run(&store, ProjectsCommand::Ls(ProjectsLsArgs { all: true })).is_ok());
        // アーカイブ有無で `project_list` の中身が違うことを確かめておく（表示は目視）。
        let archived_count = store
            .project_list()
            .expect("list")
            .iter()
            .filter(|p| p.archived_at.is_some())
            .count();
        assert_eq!(archived_count, 1);
    }

    /// 読み取り専用: `show` は milestone も一緒に出し、無い id は 404 相当のエラー。
    #[test]
    fn run_show_reads_the_project_and_its_milestones() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let project = sample_project(ProjectStatus::Active, false);
        store.project_create(&project).expect("create");

        let ok = run(
            &store,
            ProjectsCommand::Show(ProjectsShowArgs {
                id: project.id.to_string(),
            }),
        );
        assert!(ok.is_ok());

        let missing = run(
            &store,
            ProjectsCommand::Show(ProjectsShowArgs {
                id: task_core::ProjectId::new().to_string(),
            }),
        );
        assert!(missing.is_err());

        let bad_id = run(
            &store,
            ProjectsCommand::Show(ProjectsShowArgs {
                id: "not-a-ulid".into(),
            }),
        );
        assert!(bad_id.is_err());
    }
}
