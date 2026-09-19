//! ADR-0044 D5（Phase 53）: `GET /tasks/{id}/timeline`。
//!
//! そのタスクに起きたことを**時刻の昇順で 1 本**にまとめる: イベント（遷移・run・質問・回答・編集・
//! 割り込み）、コメント、認可、報告、委譲（子の作成）、リリース（そのタスクのブランチのコミットが
//! 入ったリリース）。
//!
//! 集めるのは決定的（ストアと、`ReleaseSource` が読むリリースのディレクトリだけ）。LLM は関与しない。
//! ADR-0043 D5 / A2 の「取り込み（merge / PR / discard）」は `TimelineItem::Integration` の口だけ空けてある。

use std::collections::HashSet;

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use task_core::{ApprovalStore, Event, ReportFilter, ReportStore, SqliteStore, Task, TaskId, TaskStore};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::handlers::{ApiResult, Params, json_response, no_query};
use crate::problem::{ApiProblem, store_problem};
use crate::query::parse_task_id;
use crate::state::ApiState;
use crate::types::{Timeline, TimelineItem};

/// 報告を引くときの件数の上限（`task_id` で SQL 側から絞るので、実際にはまず届かない）。
const REPORTS_LIMIT: usize = 1_000;

/// 1 本に混ぜるイベントの上限（ADR-0044 D5。これを超える古いイベントは `GET /tasks/{id}/events` で見る）。
const TIMELINE_EVENTS_LIMIT: usize = 2_000;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new().route("/api/v1/tasks/{id}/timeline", axum::routing::get(timeline))
}

/// `GET /tasks/{id}/timeline`（読み取り）。
pub(crate) async fn timeline(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let ctx = state.inner.view.clone();
    let releases = state.inner.releases.clone();
    let view = state
        .blocking(move |store| {
            let (task, mut items) = store_items(store, id)?;
            // リリースはファイルを読む（ストアには無い）。読めなければ何も足さない。
            if let Some(source) = releases {
                items.extend(release_items(&task, &ctx.workspace_root, source.as_ref()));
            }
            sort_items(&mut items);
            Ok(Timeline { task_id: id, items })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

/// `at` の昇順（同時刻は元の並びを保つ = 安定ソート）。
///
/// RFC 3339 は**文字列のままでは比べられない**: `…:00.500Z` は `…:00Z` より後の時刻なのに
/// `'.' < 'Z'` なので文字列では前に来る（`created_at` には小数秒が付く。Phase 53 の監査で発見）。
/// 解析できたものは `OffsetDateTime` で、できなかったもの（`built_at` が読めないリリース等）は
/// 「いちばん古い」として扱う。
fn sort_items(items: &mut [TimelineItem]) {
    items.sort_by_key(|item| {
        OffsetDateTime::parse(at_of(item), &Rfc3339)
            .map(SortKey::At)
            .unwrap_or(SortKey::Unknown)
    });
}

/// 解析できない `at` は先頭（いちばん古い）に置く。
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    Unknown,
    At(OffsetDateTime),
}

pub(crate) fn at_of(item: &TimelineItem) -> &str {
    match item {
        TimelineItem::Event { at, .. }
        | TimelineItem::Comment { at, .. }
        | TimelineItem::Approval { at, .. }
        | TimelineItem::Report { at, .. }
        | TimelineItem::Delegation { at, .. }
        | TimelineItem::Release { at, .. }
        | TimelineItem::Integration { at, .. } => at,
    }
}

/// ストアから引ける 5 種（イベント・コメント・認可・報告・委譲）。
fn store_items(store: &SqliteStore, id: TaskId) -> Result<(Task, Vec<TimelineItem>), ApiProblem> {
    let task = store
        .get(id)
        .map_err(store_problem)?
        .ok_or_else(|| ApiProblem::task_not_found(id))?;
    let mut items = Vec::new();

    let rows = store
        .event_rows_for(id, None, TIMELINE_EVENTS_LIMIT)
        .map_err(store_problem)?;
    for row in rows {
        // 委譲は「子ができた」1 件として別に出す（同じイベントを 2 回出さない）。
        if let Event::Delegated { run_id, task_ids } = &row.event {
            let mut tasks = Vec::new();
            for child in task_ids {
                if let Some(t) = store.get(*child).map_err(store_problem)? {
                    tasks.push(task_ops::view::task_ref(&t));
                }
            }
            items.push(TimelineItem::Delegation {
                at: row.ts.clone(),
                run_id: run_id.clone(),
                tasks,
            });
            continue;
        }
        items.push(TimelineItem::Event {
            at: row.ts,
            seq: row.seq,
            event: row.event,
        });
    }

    for comment in store.comments_for(id).map_err(store_problem)? {
        items.push(TimelineItem::Comment {
            at: crate::handlers::rfc3339(comment.created_at),
            comment,
        });
    }

    for approval in store
        .approval_list(None, task.project_id, None)
        .map_err(store_problem)?
        .into_iter()
        .filter(|a| a.task_id == Some(id))
    {
        // 決まっていればその時刻、まだなら聞いた時刻。
        let at = crate::handlers::rfc3339(approval.decided_at.unwrap_or(approval.created_at));
        items.push(TimelineItem::Approval { at, approval });
    }

    // `task_id` は SQL で絞る（Rust 側で絞ると `LIMIT` で新しい報告に押し出されて消える）。
    let filter = ReportFilter {
        task_id: Some(id),
        limit: REPORTS_LIMIT,
        ..ReportFilter::default()
    };
    for report in store.report_list(&filter).map_err(store_problem)? {
        items.push(TimelineItem::Report {
            at: crate::handlers::rfc3339(report.created_at),
            report,
        });
    }

    Ok((task, items))
}

/// ADR-0044 D5: そのタスクのブランチのコミットが入ったリリース。
///
/// ブランチ名は `worktree.json`（ADR-0041 D1 の目印。`celeris/<task_id>` / `taskd/<task_id>`）から取る。
/// 目印が無い・git が動かない・`changes.json` が無いリリースしかない、のどれでも**何も足さない**。
fn release_items(
    task: &Task,
    workspace_root: &std::path::Path,
    source: &dyn crate::releases::ReleaseSource,
) -> Vec<TimelineItem> {
    let task_dir = task_ops::workspace::local_dir(task, workspace_root);
    let Some(marker) = task_ops::workspace::read_marker(&task_dir) else {
        return Vec::new();
    };
    // base が分からなければ**何も足さない**。`rev-list <branch>` はそのブランチの歴史すべて
    // （main の分も）を返すので、他人のコミットをこのタスクの成果として並べてしまう
    // （Phase 53 の監査で発見）。
    let base = if marker.base.is_empty() {
        return Vec::new();
    } else {
        marker.base.as_str()
    };
    let commits: HashSet<String> = source
        .branch_commits(std::path::Path::new(&marker.repo), &marker.branch, Some(base))
        .into_iter()
        .collect();
    if commits.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in source.list().items {
        let Some(changes) = &item.changes else { continue };
        let mine: Vec<String> = changes
            .commits
            .iter()
            .filter(|c| commits.contains(&c.sha))
            .map(|c| c.sha.clone())
            .collect();
        if mine.is_empty() {
            continue;
        }
        out.push(TimelineItem::Release {
            // `built_at` が読めないリリース（`manifest.json` が壊れている）は `at` を空にする。
            // 空文字は RFC 3339 のどの時刻より小さいので先頭に来るが、並びは安定ソートなので
            // 同じ `at` どうしの順は `GET /releases` の順（`built_at` の新しい順）のまま。
            at: item.built_at.clone().unwrap_or_default(),
            sha12: item.sha12.clone(),
            commits: mine,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TimelineItem;

    /// 時刻の昇順に並び、同時刻は元の順を保つ（安定ソート）。
    #[test]
    fn items_are_sorted_by_time_and_ties_keep_their_order() {
        let mut items = vec![
            TimelineItem::Release {
                at: "2026-09-19T03:00:00Z".into(),
                sha12: "b".into(),
                commits: vec![],
            },
            TimelineItem::Integration {
                at: "2026-09-19T01:00:00Z".into(),
                action: "merge".into(),
                detail: "first".into(),
            },
            TimelineItem::Integration {
                at: "2026-09-19T01:00:00Z".into(),
                action: "merge".into(),
                detail: "second".into(),
            },
        ];
        sort_items(&mut items);
        assert_eq!(at_of(&items[0]), "2026-09-19T01:00:00Z");
        assert!(matches!(&items[0], TimelineItem::Integration { detail, .. } if detail == "first"));
        assert!(matches!(&items[1], TimelineItem::Integration { detail, .. } if detail == "second"));
        assert_eq!(at_of(&items[2]), "2026-09-19T03:00:00Z");
    }

    /// Phase 53 の監査: 小数秒のある RFC 3339 を**文字列で**比べると順が狂う
    /// （`'.' < 'Z'` なので `…00.5Z` が `…00Z` より前に来る）。時刻として比べる。
    #[test]
    fn fractional_seconds_do_not_reverse_the_order() {
        let at = |t: &str| TimelineItem::Integration {
            at: t.into(),
            action: "x".into(),
            detail: t.into(),
        };
        let mut items = vec![
            at("2026-09-19T01:00:01Z"),
            at("2026-09-19T01:00:00.500000000Z"),
            at("2026-09-19T01:00:00Z"),
            // 解析できない `at`（`built_at` が読めないリリース）はいちばん古い扱い。
            at(""),
        ];
        sort_items(&mut items);
        assert_eq!(
            items.iter().map(at_of).collect::<Vec<_>>(),
            vec![
                "",
                "2026-09-19T01:00:00Z",
                "2026-09-19T01:00:00.500000000Z",
                "2026-09-19T01:00:01Z"
            ]
        );
    }
}
