import { useEffect, useState } from "react";
import { data, Form, isRouteErrorResponse, Link, useFetcher, useNavigate, useSearchParams } from "react-router";
import { CodeViewer } from "~/components/CodeViewer";
import { RetryFlash, TaskCommentFlash, TaskEditFlash, TaskReopenFlash, TransitionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { ImageViewer } from "~/components/ImageViewer";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Sha256Badge } from "~/components/Sha256Badge";
/* ADR-0043 D5 の差分・PR はこのコンポーネントを差し替えるだけ（A2 が本物を入れる） */
import TaskChanges from "~/components/task-changes";
/* ADR-0043 D6 のファイル閲覧はこのコンポーネントを差し替えるだけ（A1 が本物を入れる） */
import TaskFiles from "~/components/task-files";
import { Badge, GenreLabel, KindBadge, RoleLabel, StatusBadge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import {
  checkboxClass,
  chipLabelClass,
  hintClass,
  inputClass,
  labelClass,
  selectClass,
  tableClass,
  tdClass,
  textareaClass,
  thClass,
  theadClass,
  trHoverClass,
} from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, DataList, EmptyState, Mono } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { artifactStatusMessage, isJson, pickViewer } from "~/lib/artifact-view";
import { isValidLabel, MAX_LABELS, PRIORITY_LABELS } from "~/lib/board";
import {
  commentAuthorLabel,
  milestoneStatusLabel,
  parseTaskTab,
  priorityFullLabel,
  TASK_CATEGORIES,
  TASK_TABS,
  type TaskTab,
  TIERS,
  taskCategoryLabel,
  taskTabLabel,
  tierLabel,
  timelineKindLabel,
} from "~/lib/labels";
import { milestoneTitle } from "~/lib/project-index";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { cn } from "~/lib/utils";
import { TaskdBanner } from "~/root";
import type {
  RetryOutcome,
  TaskCommentOutcome,
  TaskEditOutcome,
  TaskReopenOutcome,
  TransitionOutcome,
} from "~/taskd/action-types";
import { retryData, transitionData } from "~/taskd/actions.server";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { runRetryAction, runTaskAction } from "~/taskd/route-actions.server";
import { buildTaskEdit, commentOnTask, editTask, reopenTask } from "~/taskd/tasks-admin.server";
import type {
  Action,
  ArtifactList,
  ArtifactView,
  CommentList,
  Event,
  EventsPage,
  MilestoneView,
  OrgList,
  OrgNode,
  ProjectDetail,
  TaskComment,
  TaskDetail,
  TaskRef,
  Timeline,
  TimelineItem,
} from "~/taskd/types";
import type { Route } from "./+types/tasks.$id";

/**
 * `docs/taskd-api-v1.md` §3.6 の `types` フィルタの選択肢。`Event` の `type` タグと同じ。
 */
const EVENT_TYPES: Event["type"][] = [
  "created",
  "transitioned",
  "worker_started",
  "worker_progress",
  "artifact_produced",
  "worker_finished",
  "review_verdict",
  "approval_requested",
  "approval_decided",
  "answered",
  "provider_throttled",
  // ADR-0044 D1（Phase 53）: 人がタスクを編集したときの記録。
  "edited",
];

const ACTION_LABELS: Record<Action, string> = {
  approve: "承認",
  reject: "却下",
  answer: "回答",
  cancel: "取り消し",
  retry: "やり直す",
  // ADR-0044 D1 / D2（Phase 53）。「編集」は概要タブ、「再開」はタイムラインタブに置く。
  edit: "編集",
  reopen: "再開",
};

/** run の outcome → 色（docs/adr/0011 D4 と同じ考え方。文字列は outcome 名をそのまま出す）。 */
const OUTCOME_TONE: Record<string, Tone> = {
  done: "success",
  question: "info",
  error: "danger",
  requeue: "warning",
  lease_expired: "warning",
  // ADR-0044 D2/D8: 人のコメントで止めた run は**失敗ではない**ので danger にしない。
  interrupted: "info",
};

/** タイムラインの 1 件の色（ADR-0044 D5）。 */
const TIMELINE_TONE: Record<string, Tone> = {
  event: "neutral",
  comment: "info",
  approval: "warning",
  report: "teal",
  delegation: "primary",
  release: "success",
  integration: "neutral",
};

export interface TaskDetailData {
  detail: TaskDetail;
  events: EventsPage;
  artifacts: ArtifactList;
  /** ADR-0044 D5: `GET /tasks/{id}/timeline`（時刻の昇順で 1 本）。 */
  timeline: Timeline;
  /** ADR-0044 D2: `GET /tasks/{id}/comments`（古い順）。タブの件数と、コメント欄の見出しに使う。 */
  comments: CommentList;
  /** ADR-0044 D1: 編集フォームの「担当」プルダウンの選択肢（`GET /org`。落ちても画面は出す）。 */
  org: OrgNode[];
  /** ADR-0044 D1: 編集フォームの「途中目標」プルダウン（そのタスクの案件のものだけ）。 */
  milestones: MilestoneView[];
  /** どの案件・どの途中目標・誰の仕事か（監査 M2「裏方から戻れる」）。分からなければ null。 */
  place: {
    projectId: string | null;
    projectTitle: string | null;
    milestoneTitle: string | null;
    assigneeId: string | null;
    assigneeName: string | null;
  };
}

/**
 * `/tasks/:id`（タスク詳細、docs/DESIGN.md §4.3。ADR-0044 D5 でタブになった）の loader 本体。
 * `GET /tasks/{id}`・`GET /tasks/{id}/events`・`GET /tasks/{id}/artifacts`・`GET /tasks/{id}/timeline`・
 * `GET /tasks/{id}/comments` を並列に呼び、応答をそのまま返す（派生値は taskd 側で計算済み。GUI は再計算しない）。
 * taskd 停止中・タスクが無い（404 `task_not_found`）等は呼び出し側（`loader`）が `Response` に変換して投げる
 * （docs/adr/0004-g1-decisions.md D6。本番ビルドは素の Error を ErrorBoundary に渡す前に汎用 500 へ
 * サニタイズするため、`Response` として投げないと taskd 停止中でもバナーではなく 500 になってしまう）。
 * 生ログ本体は `/tasks/:id/runs/:runId`（別ルート）、DAG は `/graph`（docs/adr/0006-g3-decisions.md D4）。
 *
 * `GET /org` と案件の詳細は**編集フォームの選択肢**（ADR-0044 D1: 担当・途中目標のプルダウン）にも使うので、
 * 担当や案件が未設定でも常に引く（落ちたら選択肢が空になるだけ。画面は出す）。
 */
export async function loadTaskDetail(client: TaskdClient, taskId: string, request: Request): Promise<TaskDetailData> {
  const url = new URL(request.url);
  // フォームは `types` チェックボックスごとに 1 つずつ付ける（`?types=a&types=b`）。
  // taskd 側はカンマ区切りの単一パラメータを期待する（docs/taskd-api-v1.md §3.6）ので、ここで結合する。
  const types = url.searchParams.getAll("types");
  const [detail, events, artifacts, timeline, comments] = await Promise.all([
    client.get<TaskDetail>(`/tasks/${taskId}`, { signal: request.signal }),
    client.get<EventsPage>(`/tasks/${taskId}/events`, {
      query: { types: types.length > 0 ? types.join(",") : undefined },
      signal: request.signal,
    }),
    client.get<ArtifactList>(`/tasks/${taskId}/artifacts`, { signal: request.signal }),
    client.get<Timeline>(`/tasks/${taskId}/timeline`, { signal: request.signal }),
    client.get<CommentList>(`/tasks/${taskId}/comments`, { signal: request.signal }),
  ]);
  // 案件・途中目標・担当の名前（監査 M2）と、編集フォームの選択肢（ADR-0044 D1）。
  const assigneeId = detail.task.assignee ?? null;
  const projectId = detail.task.project_id ?? null;
  const [project, org] = await Promise.all([
    projectId
      ? client
          .get<ProjectDetail>(`/projects/${encodeURIComponent(projectId)}`, { signal: request.signal })
          .catch(() => null)
      : Promise.resolve(null),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => null),
  ]);
  return {
    detail,
    events,
    artifacts,
    timeline,
    comments,
    org: org?.items ?? [],
    milestones: project?.milestones ?? [],
    place: {
      projectId,
      projectTitle: project?.project.title ?? null,
      milestoneTitle: milestoneTitle(project?.milestones ?? [], detail.task.milestone_id),
      assigneeId,
      assigneeName: assigneeId ? (org?.items.find((n) => n.id === assigneeId)?.name ?? assigneeId) : null,
    },
  };
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "タスク詳細 - Celeris" }];
}

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ params, request }: Route.LoaderArgs): Promise<TaskDetailData> {
  try {
    return await loadTaskDetail(getTaskdClient(), params.id, request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export async function action({ request, params }: Route.ActionArgs) {
  const form = await request.formData();
  const client = getTaskdClient();
  const intent = form.get("intent");
  // Phase 31: `retry` は `approve`/`reject`/`answer`/`cancel`（`TransitionInput`）とは語彙も応答の形も別
  // （新しいタスクを作る。`RetryOutcome`）なので、共通の `readTransitionForm` に渡す前に分岐する。
  // Phase 53（ADR-0044 D1/D2）: `edit` / `comment` / `reopen` も同じ理由でここで分ける。
  if (intent === "retry") {
    const outcome = await runRetryAction(client, params.id, form, request.signal);
    return retryData(outcome);
  }
  if (intent === "edit") {
    const outcome = await editTask(client, params.id, buildTaskEdit(form), request.signal);
    return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
  }
  if (intent === "comment") {
    const outcome = await commentOnTask(client, params.id, form, request.signal);
    return data(outcome, { status: outcome.ok ? 201 : outcome.error.status });
  }
  if (intent === "reopen") {
    const outcome = await reopenTask(client, params.id, form, request.signal);
    return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
  }
  const outcome = await runTaskAction(client, params.id, form, request.signal);
  return transitionData(outcome);
}

export default function TaskDetailPage({ loaderData }: Route.ComponentProps) {
  const { detail, events, artifacts, timeline, comments, org, milestones, place } = loaderData;
  const { task } = detail;
  const [searchParams] = useSearchParams();
  const tab = parseTaskTab(searchParams.get("tab"));
  // 操作の結果は fetcher に載せる（監査 H1。SSE の再検証で `actionData` が消えるのを避ける）。
  const fetcher = useFetcher<TransitionOutcome>();
  const submitting = fetcher.state !== "idle";
  // Phase 31: 「やり直す」は別のタスクを新しく作る（`TransitionOutcome` とは形が違う）ので別の fetcher。
  // 成功したら新しいタスクへ遷移する（fetcher はナビゲーションを行わないので `useNavigate` で明示的に行う）。
  const retryFetcher = useFetcher<RetryOutcome>();
  const retrying = retryFetcher.state !== "idle";
  const navigate = useNavigate();
  useEffect(() => {
    if (retryFetcher.data?.ok) {
      navigate(`/tasks/${retryFetcher.data.result.task_id}`);
    }
  }, [retryFetcher.data, navigate]);

  return (
    <div className="space-y-8">
      {/* ヒーロー: タイトル・status/kind/role・ID・クラスタ / 親・操作(DAG) */}
      <section aria-labelledby="header-heading" data-testid="header-section">
        <div className="rounded-2xl border border-border bg-surface p-6 shadow-sm">
          <div className="flex flex-wrap items-start justify-between gap-5">
            <div className="min-w-0 flex-1 space-y-2.5">
              <div className="flex flex-wrap items-center gap-2">
                <StatusBadge status={task.status} data-testid="task-status" />
                <KindBadge kind={task.kind} data-testid="task-kind" />
                <RoleLabel role={detail.role ?? "-"} data-testid="task-role" />
                {/* 分野（ADR-0027 D1）。role と同じ理由で色分けはせずテキストのラベルだけ。分野なしは "-"。 */}
                <GenreLabel genre={detail.genre ?? "-"} data-testid="task-genre" />
                {/* ADR-0044 D3（Phase 53）: 優先度・種類・ラベルは一目で分かるところに置く。 */}
                <Badge tone="primary" data-testid="task-priority">
                  {priorityFullLabel(detail.priority_label)}
                </Badge>
                <Badge tone="neutral" data-testid="task-category">
                  {taskCategoryLabel(task.category ?? "other")}
                </Badge>
                {(task.labels ?? []).map((label) => (
                  <Badge key={label} tone="teal" data-testid="task-label">
                    {label}
                  </Badge>
                ))}
              </div>
              <h1 id="header-heading" className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <span data-testid="task-id" className="font-mono text-xs text-fg-subtle">
                  {task.id}
                </span>
                <HelpLink anchor="screens" label="画面ごとの説明" />
              </h1>
              <p className="break-words text-2xl font-bold tracking-tight text-fg" data-testid="task-title">
                {task.title}
              </p>
              {/* 裏方から戻れる導線（監査 M2）: 案件・担当・途中目標。 */}
              <div
                className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-fg-muted"
                data-testid="task-place"
              >
                {place.projectId && (
                  <p>
                    案件:{" "}
                    <Link
                      to={`/projects/${place.projectId}`}
                      data-testid="task-project-link"
                      className="font-medium text-primary hover:underline"
                    >
                      {place.projectTitle ?? place.projectId}
                    </Link>
                  </p>
                )}
                {place.assigneeId && (
                  <p>
                    担当:{" "}
                    <Link
                      to={
                        place.assigneeId === "secretary"
                          ? "/org/secretary"
                          : `/org/${encodeURIComponent(place.assigneeId)}`
                      }
                      data-testid="task-assignee-link"
                      className="font-medium text-primary hover:underline"
                    >
                      {place.assigneeName ?? place.assigneeId}
                    </Link>
                  </p>
                )}
                {place.milestoneTitle && <p data-testid="task-milestone">途中目標: {place.milestoneTitle}</p>}
              </div>
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-fg-muted">
                {detail.cluster && (
                  <p data-testid="task-cluster">
                    cluster:{" "}
                    <Link to="/clusters" className="font-medium text-primary hover:underline">
                      {detail.cluster}
                    </Link>
                    <span className="ml-2 text-xs text-fg-subtle" data-testid="task-workspace-note">
                      {/* ADR-0039 D3（Phase G13k）: 編集は手元の作業ディレクトリで、検証はリモートで。 */}
                      {detail.workspace_dir
                        ? `手元の写し: ${detail.workspace_dir}（クラスタ側の元のパスは表示されません）`
                        : "workspace_dir はクラスタ側ではなく手元の写しです（クラスタ側の元のパスは表示されません）。"}
                    </span>
                  </p>
                )}
                {task.parent_id && (
                  <p data-testid="task-parent">
                    親:{" "}
                    <Link to={`/tasks/${task.parent_id}`} className="font-medium text-primary hover:underline">
                      {task.parent_id}
                    </Link>
                  </p>
                )}
              </div>
            </div>
            <Link
              to={`/graph?root=${task.id}`}
              data-testid="task-graph-link"
              className={buttonClass({ variant: "secondary", size: "sm" })}
            >
              <Icon name="gitBranch" />
              DAG で見る
            </Link>
          </div>
        </div>
      </section>

      {/* ADR-0044 D5: 概要 / タイムライン / 変更 / ファイル / 成果物。`?tab=` が状態なのでリンクできる。 */}
      <TaskTabs
        current={tab}
        searchParams={searchParams}
        counts={{ timeline: timeline.items.length, artifacts: artifacts.items.length }}
      />

      {tab === "overview" && (
        <OverviewTab
          detail={detail}
          artifactCount={artifacts.items.length}
          org={org}
          milestones={milestones}
          fetcher={fetcher}
          submitting={submitting}
          retryFetcher={retryFetcher}
          retrying={retrying}
        />
      )}

      {tab === "timeline" && (
        <TimelineTab taskId={task.id} detail={detail} timeline={timeline} comments={comments} events={events} />
      )}

      {/* ADR-0043 A1/A2 が入るまでの差し込み口（この 1 行を差し替えるだけで本物になる）。 */}
      {tab === "changes" && (
        <section aria-labelledby="changes-heading" data-testid="changes-section" className="space-y-4">
          <h2 id="changes-heading" className="text-[0.95rem] font-semibold text-fg">
            変更
          </h2>
          <TaskChanges taskId={task.id} />
        </section>
      )}

      {tab === "files" && (
        <section aria-labelledby="files-heading" data-testid="files-section" className="space-y-4">
          <h2 id="files-heading" className="text-[0.95rem] font-semibold text-fg">
            ファイル
          </h2>
          <TaskFiles taskId={task.id} />
        </section>
      )}

      {tab === "artifacts" && (
        <section aria-labelledby="artifacts-heading" data-testid="artifacts-section">
          <Card>
            <CardHeader
              icon="folder"
              title={
                <h2 id="artifacts-heading" className="text-[0.95rem] font-semibold text-fg">
                  成果物
                </h2>
              }
            />
            <CardBody>
              {artifacts.items.length === 0 ? (
                <EmptyState icon="folder" title="ありません。" />
              ) : (
                <ul className="space-y-3">
                  {artifacts.items.map((artifact) => (
                    <ArtifactRow key={artifact.idx} taskId={task.id} artifact={artifact} />
                  ))}
                </ul>
              )}
            </CardBody>
          </Card>
        </section>
      )}
    </div>
  );
}

/** タブの見出し（ADR-0044 D5）。`?tab=` だけを差し替え、他の検索パラメータ（`types` 等）は残す。 */
function TaskTabs({
  current,
  searchParams,
  counts,
}: {
  current: TaskTab;
  searchParams: URLSearchParams;
  counts: { timeline: number; artifacts: number };
}) {
  return (
    <nav aria-label="タスクの内訳" data-testid="task-tabs">
      <ul className="flex flex-wrap gap-1 border-b border-border">
        {TASK_TABS.map((t) => {
          const params = new URLSearchParams(searchParams);
          params.set("tab", t);
          const active = t === current;
          const count = t === "timeline" ? counts.timeline : t === "artifacts" ? counts.artifacts : null;
          return (
            <li key={t}>
              <Link
                to={`?${params.toString()}`}
                replace
                data-testid={`task-tab-${t}`}
                data-active={active ? "true" : undefined}
                aria-current={active ? "page" : undefined}
                className={cn(
                  "-mb-px inline-flex items-center gap-1.5 rounded-t-lg border-b-2 px-3.5 py-2 text-sm font-medium no-underline transition-colors",
                  active
                    ? "border-primary text-primary"
                    : "border-transparent text-fg-muted hover:border-border-strong hover:text-fg",
                )}
              >
                {taskTabLabel(t)}
                {count !== null && count > 0 && <span className="text-xs tabular-nums text-fg-subtle">{count}</span>}
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

/** 概要タブ（ADR-0044 D5）: 従来の詳細一式に、人が直接直せる編集フォーム（D1）を足したもの。 */
function OverviewTab({
  detail,
  artifactCount,
  org,
  milestones,
  fetcher,
  submitting,
  retryFetcher,
  retrying,
}: {
  detail: TaskDetail;
  artifactCount: number;
  org: OrgNode[];
  milestones: MilestoneView[];
  fetcher: ReturnType<typeof useFetcher<TransitionOutcome>>;
  submitting: boolean;
  retryFetcher: ReturnType<typeof useFetcher<RetryOutcome>>;
  retrying: boolean;
}) {
  const { task } = detail;
  return (
    <>
      <section aria-labelledby="info-heading" data-testid="info-section">
        <Card>
          <CardHeader
            icon="file"
            tone="neutral"
            title={
              <h2 id="info-heading" className="text-[0.95rem] font-semibold text-fg">
                基本情報
              </h2>
            }
          />
          <CardBody className="space-y-4">
            <DataItem label="目的" wide>
              <p className="whitespace-pre-wrap text-sm" data-testid="task-objective">
                {task.objective}
              </p>
            </DataItem>
            <DataList>
              <DataItem label="priority">{`${detail.priority_label}（${task.priority}）`}</DataItem>
              <DataItem label="worker_hint">
                {`tier=${task.worker_hint.tier}${task.worker_hint.adapter ? `, adapter=${task.worker_hint.adapter}` : ""}`}
              </DataItem>
              <DataItem label="attempts / max_retries">
                <span className="tabular-nums">{`${task.attempts} / ${task.budget.max_retries}`}</span>
              </DataItem>
              <DataItem label="budget">
                {`max_turns=${task.budget.max_turns}, max_wall_secs=${task.budget.max_wall_secs}, max_retries=${task.budget.max_retries}`}
              </DataItem>
              <DataItem label="成果物">
                <span className="tabular-nums">{artifactCount}</span>
              </DataItem>
              <DataItem label="workspace_dir" wide>
                <span className="break-all font-mono text-xs">{detail.workspace_dir ?? "(remote)"}</span>
              </DataItem>
            </DataList>
          </CardBody>
        </Card>
      </section>

      {/* ADR-0044 D1（Phase 53）: 人がタスクを細かく直せる。終端のタスクは taskd が 409 を返すので出さない。 */}
      {detail.actions.includes("edit") && (
        <TaskEditSection key={task.updated_at} detail={detail} org={org} milestones={milestones} />
      )}

      <section data-testid="relations-section">
        <Card>
          <CardHeader
            icon="gitBranch"
            tone="teal"
            title={<h2 className="text-[0.95rem] font-semibold text-fg">子 / 依存</h2>}
          />
          <CardBody className="space-y-5">
            <TaskRefList label="dependencies" testId="dependencies" refs={detail.dependencies} />
            <TaskRefList label="dependents" testId="dependents" refs={detail.dependents} />
            <TaskRefList label="children" testId="children" refs={detail.children} />
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="timers-heading" data-testid="timers-section">
        <Card>
          <CardHeader
            icon="clock"
            tone="info"
            title={
              <h2 id="timers-heading" className="text-[0.95rem] font-semibold text-fg">
                タイマー
              </h2>
            }
          />
          <CardBody>
            <DataList>
              <DataItem label="lease_expires_at">{detail.timers.lease_expires_at ?? "-"}</DataItem>
              <DataItem label="backoff_until">{detail.timers.backoff_until ?? "-"}</DataItem>
              <DataItem label="consecutive_requeues / max_requeues">
                <span className="tabular-nums">
                  {`${detail.timers.consecutive_requeues} / ${detail.timers.max_requeues}`}
                </span>
              </DataItem>
              <DataItem label="consecutive_reviewer_requeues">
                <span className="tabular-nums">{String(detail.timers.consecutive_reviewer_requeues)}</span>
              </DataItem>
              <DataItem label="now">{detail.timers.now}</DataItem>
            </DataList>
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="criteria-heading" data-testid="criteria-section">
        <Card>
          <CardHeader
            icon="checkCircle"
            tone="success"
            title={
              <h2 id="criteria-heading" className="text-[0.95rem] font-semibold text-fg">
                受け入れ条件と判定
              </h2>
            }
          />
          <CardBody>
            {detail.criteria.length === 0 ? (
              <EmptyState icon="checkCircle" title="ありません。" />
            ) : (
              <ul className="space-y-3">
                {detail.criteria.map((criterion) => (
                  <li
                    key={criterion.idx}
                    data-testid="criterion-item"
                    className="rounded-lg border border-border bg-surface-2/40 p-3 text-sm"
                  >
                    <p className="flex flex-wrap items-center gap-2">
                      <Mono>#{criterion.idx}</Mono>
                      <KindBadge kind={criterion.check.type} />
                      <span className="text-fg">{criterion.text}</span>
                    </p>
                    {criterion.latest_verdict && (
                      <p
                        className="mt-1.5 flex flex-wrap items-center gap-1.5 text-fg-muted"
                        data-testid="criterion-verdict"
                      >
                        <span>直近判定:</span>
                        <Badge tone={criterion.latest_verdict.pass ? "success" : "danger"} dot>
                          {criterion.latest_verdict.pass ? "pass" : "fail"}
                        </Badge>
                        <span>— {criterion.latest_verdict.reason}</span>
                      </p>
                    )}
                    {criterion.check.type === "human" && criterion.approval && (
                      <p className="mt-1.5 text-fg-muted" data-testid="criterion-approval">
                        Approval:{" "}
                        <Link
                          to={`/tasks/${criterion.approval.approval.id}`}
                          className="font-medium text-primary hover:underline"
                        >
                          {criterion.approval.approval.id}
                        </Link>
                        （{criterion.approval.approval.status}）
                      </p>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="runs-heading" data-testid="runs-section">
        <Card>
          <CardHeader
            icon="terminal"
            title={
              <h2 id="runs-heading" className="text-[0.95rem] font-semibold text-fg">
                run 一覧
              </h2>
            }
          />
          <CardBody className={detail.runs.length === 0 ? undefined : "p-0"}>
            {detail.runs.length === 0 ? (
              <EmptyState icon="terminal" title="ありません。" />
            ) : (
              <div className="overflow-x-auto">
                <table className={tableClass}>
                  <thead className={theadClass}>
                    <tr>
                      <th className={thClass}>run_id</th>
                      <th className={thClass}>role</th>
                      <th className={thClass}>adapter</th>
                      <th className={thClass}>provider</th>
                      <th className={thClass}>account</th>
                      <th className={thClass}>model</th>
                      <th className={thClass}>started_at</th>
                      <th className={thClass}>finished_at</th>
                      <th className={thClass}>outcome</th>
                      <th className={thClass}>usage</th>
                      <th className={thClass}>progress</th>
                      <th className={thClass}>artifacts</th>
                      <th className={thClass}>verdicts</th>
                      <th className={thClass}>files</th>
                      <th className={thClass}>ログ</th>
                    </tr>
                  </thead>
                  <tbody>
                    {detail.runs.map((run) => (
                      <tr key={run.run_id} data-testid="run-row" className={trHoverClass}>
                        <td className={cn(tdClass, "font-mono text-xs")}>{run.run_id}</td>
                        <td className={tdClass}>
                          <RoleLabel role={run.role} />
                        </td>
                        <td className={tdClass}>{run.adapter}</td>
                        <td className={tdClass}>{run.provider ?? "-"}</td>
                        {/* プールの run だけ、どのアカウントで動いたかが入る（ADR-0024 D4 / ADR-0025） */}
                        <td className={cn(tdClass, "whitespace-nowrap")} data-testid="run-account">
                          {run.account ?? "-"}
                        </td>
                        <td className={tdClass}>{run.model}</td>
                        <td className={cn(tdClass, "whitespace-nowrap text-xs text-fg-subtle")}>{run.started_at}</td>
                        <td className={cn(tdClass, "whitespace-nowrap text-xs text-fg-subtle")}>
                          {run.finished_at ?? "-"}
                        </td>
                        <td className={tdClass}>
                          {run.outcome ? (
                            <Badge tone={OUTCOME_TONE[run.outcome] ?? "neutral"}>
                              {run.outcome}
                              {run.outcome_text ? ` (${run.outcome_text})` : ""}
                            </Badge>
                          ) : (
                            <span className="text-fg-subtle">-</span>
                          )}
                        </td>
                        <td className={cn(tdClass, "tabular-nums")}>
                          {run.usage
                            ? `in=${run.usage.input_tokens ?? "-"} out=${run.usage.output_tokens ?? "-"}`
                            : "-"}
                        </td>
                        <td className={cn(tdClass, "tabular-nums")}>{run.progress}</td>
                        <td className={cn(tdClass, "tabular-nums")}>{run.artifacts}</td>
                        <td className={cn(tdClass, "tabular-nums")}>{run.verdicts}</td>
                        <td className={tdClass} data-testid="run-files">
                          {run.files
                            ? ["stdout", "stderr", "result"]
                                .filter((k) => run.files?.[k as keyof typeof run.files])
                                .join(", ") || "-"
                            : "-"}
                        </td>
                        <td className={tdClass}>
                          <Link
                            to={`/tasks/${task.id}/runs/${run.run_id}`}
                            data-testid="run-log-link"
                            className={buttonClass({ variant: "ghost", size: "xs" })}
                          >
                            <Icon name="terminal" />
                            ログ
                          </Link>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="delegated-heading" data-testid="delegated-section">
        <Card>
          <CardHeader
            icon="users"
            tone="teal"
            title={
              <h2 id="delegated-heading" className="text-[0.95rem] font-semibold text-fg">
                委譲
              </h2>
            }
          />
          <CardBody>
            {detail.delegated.length === 0 ? (
              <EmptyState icon="users" title="ありません。" />
            ) : (
              <ul className="space-y-3">
                {detail.delegated.map((group) => (
                  <li
                    key={group.run_id}
                    data-testid="delegated-group"
                    data-run-id={group.run_id}
                    className="rounded-lg border border-border p-3 text-sm"
                  >
                    <p className="text-xs text-fg-subtle">
                      run{" "}
                      <Link
                        to={`/tasks/${task.id}/runs/${group.run_id}`}
                        className="font-medium text-primary hover:underline"
                      >
                        {group.run_id}
                      </Link>{" "}
                      · {group.ts}
                    </p>
                    <ul className="mt-2 space-y-1.5">
                      {group.tasks.map((child) => (
                        <li key={child.id} className="flex flex-wrap items-center gap-1.5">
                          <Link
                            to={`/tasks/${child.id}`}
                            data-testid="delegated-child-link"
                            className="font-medium text-primary hover:underline"
                          >
                            {child.title}
                          </Link>
                          <span className="text-fg-subtle">（{child.status}）</span>
                        </li>
                      ))}
                    </ul>
                  </li>
                ))}
              </ul>
            )}
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="prior-review-heading" data-testid="prior-review-section">
        <Card>
          <CardHeader
            icon="rotate"
            title={
              <h2 id="prior-review-heading" className="text-[0.95rem] font-semibold text-fg">
                prior_review
              </h2>
            }
          />
          <CardBody>
            {detail.prior_review.length === 0 ? (
              <EmptyState icon="rotate" title="ありません。" />
            ) : (
              <ul className="space-y-1.5">
                {detail.prior_review.map((note) => (
                  <li
                    key={`${note.criterion}-${note.pass}-${note.reason}`}
                    data-testid="prior-review-item"
                    className="flex flex-wrap items-center gap-1.5 text-sm"
                  >
                    <Mono>#{note.criterion}</Mono>
                    <Badge tone={note.pass ? "success" : "danger"} dot>
                      {note.pass ? "pass" : "fail"}
                    </Badge>
                    <span className="text-fg-muted">— {note.reason}</span>
                  </li>
                ))}
              </ul>
            )}
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="answers-heading" data-testid="answers-section">
        <Card>
          <CardHeader
            icon="message"
            tone="info"
            title={
              <h2 id="answers-heading" className="text-[0.95rem] font-semibold text-fg">
                answers
              </h2>
            }
          />
          <CardBody className="space-y-3">
            {detail.answers.length === 0 ? (
              <EmptyState icon="message" title="ありません。" />
            ) : (
              <ul className="space-y-1.5">
                {detail.answers.map((note) => (
                  <li
                    key={`${note.question}-${note.answer}`}
                    data-testid="answer-item"
                    className="rounded-lg border border-border p-2.5 text-sm text-fg"
                  >
                    Q: {note.question} / A: {note.answer}
                  </li>
                ))}
              </ul>
            )}
            {detail.latest_question && (
              <p className="text-sm text-fg-muted" data-testid="latest-question">
                最新の質問: {detail.latest_question}
              </p>
            )}
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="actions-heading" data-testid="actions-section">
        <Card>
          <CardHeader
            icon="zap"
            tone="warning"
            title={
              <h2 id="actions-heading" className="text-[0.95rem] font-semibold text-fg">
                操作
              </h2>
            }
          />
          <CardBody className="space-y-4">
            <TransitionFlash outcome={fetcher.data} />
            {detail.actions.length === 0 ? (
              <EmptyState icon="ban" title="できる操作はありません。" />
            ) : (
              <div className="flex flex-wrap gap-4">
                {detail.actions.includes("approve") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full max-w-xs flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto"
                  >
                    <input type="hidden" name="intent" value="approve" />
                    <input type="hidden" name="expected_status" value={task.status} />
                    <textarea
                      name="note"
                      data-testid="action-note-approve"
                      rows={2}
                      placeholder="メモ（任意）"
                      className={textareaClass}
                    />
                    <Button
                      type="submit"
                      variant="success"
                      size="sm"
                      disabled={submitting}
                      data-testid="action-approve"
                    >
                      <Icon name="check" />
                      {ACTION_LABELS.approve}
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("reject") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full max-w-xs flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto"
                  >
                    <input type="hidden" name="intent" value="reject" />
                    <input type="hidden" name="expected_status" value={task.status} />
                    <textarea
                      name="note"
                      data-testid="action-note-reject"
                      rows={2}
                      placeholder="メモ（任意）"
                      className={textareaClass}
                    />
                    <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="action-reject">
                      <Icon name="x" />
                      {ACTION_LABELS.reject}
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("answer") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full max-w-sm flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto"
                  >
                    {detail.latest_question && (
                      <p className="text-sm text-fg" data-testid="action-question">
                        {detail.latest_question}
                      </p>
                    )}
                    <input type="hidden" name="intent" value="answer" />
                    <input type="hidden" name="expected_status" value={task.status} />
                    <textarea name="answer" data-testid="action-answer" rows={3} className={textareaClass} />
                    <Button
                      type="submit"
                      variant="primary"
                      size="sm"
                      disabled={submitting}
                      data-testid="action-answer-submit"
                    >
                      <Icon name="send" />
                      回答する
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("cancel") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full max-w-xs flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto"
                  >
                    <input type="hidden" name="intent" value="cancel" />
                    <input type="hidden" name="expected_status" value={task.status} />
                    <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="action-cancel">
                      <Icon name="ban" />
                      {ACTION_LABELS.cancel}
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("retry") && (
                  <retryFetcher.Form
                    method="post"
                    className="flex w-full max-w-xs flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto"
                  >
                    <input type="hidden" name="intent" value="retry" />
                    <label className="flex items-center gap-2 text-sm text-fg">
                      <input type="checkbox" name="accept" value="true" className={checkboxClass} />
                      受け入れ済み（ready）で始める
                    </label>
                    <Button type="submit" variant="primary" size="sm" disabled={retrying} data-testid="action-retry">
                      <Icon name="rotate" />
                      {ACTION_LABELS.retry}
                    </Button>
                  </retryFetcher.Form>
                )}
              </div>
            )}
            <RetryFlash outcome={retryFetcher.data} />
          </CardBody>
        </Card>
      </section>

      {detail.worker_run_hint && (
        <section aria-labelledby="worker-run-hint-heading" data-testid="worker-run-hint-section">
          <Card>
            <CardHeader
              icon="cpu"
              title={
                <h2 id="worker-run-hint-heading" className="text-[0.95rem] font-semibold text-fg">
                  worker_run_hint
                </h2>
              }
            />
            <CardBody>
              <code
                className="block break-all rounded-lg bg-surface-2 p-3 font-mono text-xs text-fg"
                data-testid="worker-run-hint"
              >
                {detail.worker_run_hint}
              </code>
            </CardBody>
          </Card>
        </section>
      )}
    </>
  );
}

/** 空白・カンマ区切りの id 列を配列にする（`tasks.new.tsx` の `depends_on_extra` と同じ扱い）。 */
function splitIds(text: string): string[] {
  return Array.from(
    new Set(
      text
        .split(/[\s,]+/)
        .map((s) => s.trim())
        .filter((s) => s !== ""),
    ),
  );
}

/**
 * タスクの編集フォーム（ADR-0044 D1、Phase 53）。**GUI 側では検証しない**のが原則だが、ラベルの形
 * （小文字・`[a-z0-9-]`・最大 8 個）だけはチップを足すときの入力補助として弾く（正は taskd の 422）。
 * `running` / `reviewing` でも編集できる（走っている run は止まらず、次の run から効く）。
 */
function TaskEditSection({
  detail,
  org,
  milestones,
}: {
  detail: TaskDetail;
  org: OrgNode[];
  milestones: MilestoneView[];
}) {
  const { task } = detail;
  const fetcher = useFetcher<TaskEditOutcome>({ key: `task-edit-${task.id}` });
  const busy = fetcher.state !== "idle";
  const [labels, setLabels] = useState<string[]>(task.labels ?? []);
  const [labelDraft, setLabelDraft] = useState("");
  const [labelError, setLabelError] = useState<string | null>(null);
  const [dependsOnText, setDependsOnText] = useState(() => (task.depends_on ?? []).join(" "));
  const dependsOnIds = splitIds(dependsOnText);

  function addLabel() {
    const value = labelDraft.trim().toLowerCase();
    if (value === "") return;
    if (!isValidLabel(value)) {
      setLabelError("ラベルは小文字の英数字とハイフン（a-z 0-9 -）だけです。");
      return;
    }
    if (labels.includes(value)) {
      setLabelDraft("");
      return;
    }
    if (labels.length >= MAX_LABELS) {
      setLabelError(`ラベルは ${MAX_LABELS} 個までです。`);
      return;
    }
    setLabels([...labels, value]);
    setLabelDraft("");
    setLabelError(null);
  }

  return (
    <section aria-labelledby="edit-heading" data-testid="edit-section">
      <Card>
        <CardHeader
          icon="settings"
          tone="primary"
          title={
            <h2 id="edit-heading" className="text-[0.95rem] font-semibold text-fg">
              編集
            </h2>
          }
          description="書き換えた項目だけが変わります。作業中でも変えられますが、走っている run は止まりません（次の run から効きます）。"
        />
        <CardBody className="space-y-4">
          <TaskEditFlash
            outcome={fetcher.data}
            runningNote={task.status === "running" || task.status === "reviewing"}
          />
          <fetcher.Form method="post" data-testid="task-edit-form" className="space-y-4">
            <input type="hidden" name="intent" value="edit" />
            <input type="hidden" name="expected_status" value={task.status} />

            <div>
              <label htmlFor="edit-title" className={labelClass}>
                題名
              </label>
              <input
                id="edit-title"
                name="title"
                type="text"
                defaultValue={task.title}
                data-testid="edit-title"
                className={cn(inputClass, "mt-1.5")}
              />
            </div>

            <div>
              <label htmlFor="edit-objective" className={labelClass}>
                目的
              </label>
              <textarea
                id="edit-objective"
                name="objective"
                rows={4}
                defaultValue={task.objective}
                data-testid="edit-objective"
                className={cn(textareaClass, "mt-1.5")}
              />
            </div>

            <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
              <div>
                <label htmlFor="edit-tier" className={labelClass}>
                  担当エージェントのレベル
                </label>
                <select
                  id="edit-tier"
                  name="tier"
                  defaultValue={task.worker_hint.tier}
                  data-testid="edit-tier"
                  className={cn(selectClass, "mt-1.5")}
                >
                  {TIERS.map((t) => (
                    <option key={t} value={t}>
                      {tierLabel(t)}
                    </option>
                  ))}
                </select>
              </div>

              <div>
                <label htmlFor="edit-priority" className={labelClass}>
                  優先度
                </label>
                <select
                  id="edit-priority"
                  name="priority"
                  defaultValue={detail.priority_label}
                  data-testid="edit-priority"
                  className={cn(selectClass, "mt-1.5")}
                >
                  {PRIORITY_LABELS.map((p) => (
                    <option key={p} value={p}>
                      {priorityFullLabel(p)}
                    </option>
                  ))}
                </select>
              </div>

              <div>
                <label htmlFor="edit-category" className={labelClass}>
                  種類
                </label>
                <select
                  id="edit-category"
                  name="category"
                  defaultValue={task.category ?? "other"}
                  data-testid="edit-category"
                  className={cn(selectClass, "mt-1.5")}
                >
                  {TASK_CATEGORIES.map((c) => (
                    <option key={c} value={c}>
                      {taskCategoryLabel(c)}
                    </option>
                  ))}
                </select>
              </div>

              <div>
                <label htmlFor="edit-assignee" className={labelClass}>
                  担当
                </label>
                <select
                  id="edit-assignee"
                  name="assignee"
                  defaultValue={task.assignee ?? ""}
                  data-testid="edit-assignee"
                  className={cn(selectClass, "mt-1.5")}
                >
                  <option value="">（決めない）</option>
                  {org.map((node) => (
                    <option key={node.id} value={node.id}>
                      {node.name}
                    </option>
                  ))}
                </select>
                {org.length === 0 && <p className={cn(hintClass, "mt-1")}>組織を読めませんでした。</p>}
              </div>

              <div>
                <label htmlFor="edit-milestone" className={labelClass}>
                  途中目標
                </label>
                <select
                  id="edit-milestone"
                  name="milestone_id"
                  defaultValue={task.milestone_id ?? ""}
                  data-testid="edit-milestone"
                  className={cn(selectClass, "mt-1.5")}
                >
                  <option value="">（なし）</option>
                  {milestones
                    .slice()
                    .sort((a, b) => a.seq - b.seq)
                    .map((m) => (
                      <option key={m.id} value={m.id}>
                        #{m.seq} {m.title}（{milestoneStatusLabel(m.status)}）
                      </option>
                    ))}
                </select>
                {milestones.length === 0 && <p className={cn(hintClass, "mt-1")}>この案件には途中目標がありません。</p>}
              </div>
            </div>

            {/* ラベル（ADR-0044 D3）。送るのは hidden の `labels`（空の番兵で「差し替える意思」を示す）。 */}
            <fieldset data-testid="edit-labels">
              <legend className={labelClass}>ラベル</legend>
              <input type="hidden" name="labels" value="" />
              {labels.map((label) => (
                <input key={label} type="hidden" name="labels" value={label} />
              ))}
              <div className="mt-1.5 flex flex-wrap items-center gap-2">
                {labels.map((label) => (
                  <span key={label} className={cn(chipLabelClass, "cursor-default")} data-testid="edit-label-chip">
                    {label}
                    <button
                      type="button"
                      aria-label={`ラベル ${label} を外す`}
                      onClick={() => setLabels(labels.filter((l) => l !== label))}
                      className="text-fg-subtle hover:text-danger"
                    >
                      <Icon name="x" className="size-3.5" />
                    </button>
                  </span>
                ))}
                <input
                  type="text"
                  value={labelDraft}
                  aria-label="ラベルを足す"
                  placeholder="例: pluvio"
                  data-testid="edit-label-input"
                  onChange={(e) => {
                    setLabelDraft(e.target.value);
                    if (labelError) setLabelError(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      addLabel();
                    }
                  }}
                  className={cn(inputClass, "w-40")}
                />
                <Button type="button" variant="ghost" size="xs" onClick={addLabel} data-testid="edit-label-add">
                  <Icon name="plus" />
                  足す
                </Button>
              </div>
              {labelError && (
                <p role="alert" className="mt-1 text-xs text-danger" data-testid="edit-label-error">
                  {labelError}
                </p>
              )}
              <p className={cn(hintClass, "mt-1")}>小文字の英数字とハイフンだけ、最大 {MAX_LABELS} 個。</p>
            </fieldset>

            <div>
              <label htmlFor="edit-depends-on" className={labelClass}>
                依存（depends_on）
              </label>
              <input type="hidden" name="depends_on" value="" />
              {dependsOnIds.map((id) => (
                <input key={id} type="hidden" name="depends_on" value={id} />
              ))}
              <input
                id="edit-depends-on"
                type="text"
                value={dependsOnText}
                onChange={(e) => setDependsOnText(e.target.value)}
                data-testid="edit-depends-on"
                className={cn(inputClass, "mt-1.5 font-mono text-xs")}
              />
              <p className={cn(hintClass, "mt-1")}>
                タスク id を空白かカンマで区切って書きます（差し替え。空にすると依存が無くなります）。
              </p>
            </div>

            <Button type="submit" variant="primary" size="sm" disabled={busy} data-testid="task-edit-submit">
              <Icon name="check" />
              保存
            </Button>
          </fetcher.Form>
        </CardBody>
      </Card>
    </section>
  );
}

/**
 * タイムラインタブ（ADR-0044 D5）。`GET /tasks/{id}/timeline` を**古い順に**並べ、一番下にコメント欄を置く
 * （D2: 人のコメントは担当をすぐ起こす）。終端のタスクで `actions` に `reopen` があれば「再開」も出す。
 * 生のイベント（`GET /tasks/{id}/events` の `types` 絞り込み）は従来どおり下に畳んで残す。
 */
function TimelineTab({
  taskId,
  detail,
  timeline,
  comments,
  events,
}: {
  taskId: string;
  detail: TaskDetail;
  timeline: Timeline;
  comments: CommentList;
  events: EventsPage;
}) {
  const [searchParams] = useSearchParams();
  const selectedTypes = new Set(searchParams.getAll("types"));
  const commentFetcher = useFetcher<TaskCommentOutcome>({ key: `task-comment-${taskId}` });
  const commenting = commentFetcher.state !== "idle";
  const reopenFetcher = useFetcher<TaskReopenOutcome>({ key: `task-reopen-${taskId}` });
  const reopening = reopenFetcher.state !== "idle";

  return (
    <section aria-labelledby="timeline-heading" data-testid="timeline-section" className="space-y-4">
      <Card>
        <CardHeader
          icon="activity"
          tone="info"
          title={
            <h2 id="timeline-heading" className="text-[0.95rem] font-semibold text-fg">
              タイムライン
            </h2>
          }
          description="このタスクに起きたことを時刻の順に 1 本にまとめたものです（できごと・コメント・認可・報告・委譲・リリース）。"
        />
        <CardBody className="space-y-4">
          {timeline.items.length === 0 ? (
            <EmptyState icon="activity" title="まだ何も起きていません。" />
          ) : (
            <ol className="space-y-2" data-testid="timeline-list">
              {timeline.items.map((item) => (
                <TimelineRow key={timelineItemKey(item)} taskId={taskId} item={item} />
              ))}
            </ol>
          )}

          {/* ADR-0044 D2: コメント欄はタイムラインの下。人のコメントは担当をすぐ起こす。 */}
          <div className="rounded-xl border border-border bg-surface-2/40 p-3" data-testid="comment-box">
            <TaskCommentFlash outcome={commentFetcher.data} />
            <commentFetcher.Form method="post" className="space-y-2">
              <input type="hidden" name="intent" value="comment" />
              <label htmlFor="task-comment-body" className={labelClass}>
                コメント（{comments.items.length} 件）
              </label>
              <textarea
                id="task-comment-body"
                name="body"
                rows={3}
                placeholder="例: 先に関連研究を 3 本だけ読んでから進めてください。"
                data-testid="comment-input"
                className={cn(textareaClass, "w-full")}
              />
              <p className={hintClass}>
                作業中のタスクにコメントすると、走っている run を止めて待機中に戻します（続きは同じ worktree
                から始まります）。質問待ちのときは回答として渡されます。
              </p>
              <Button type="submit" variant="primary" size="sm" disabled={commenting} data-testid="comment-submit">
                <Icon name="send" />
                コメントする
              </Button>
            </commentFetcher.Form>
          </div>

          {/* ADR-0044 D2: 終端（done / failed）のタスクは同じ worktree のまま再開できる（cancelled は不可）。 */}
          {detail.actions.includes("reopen") && (
            <div className="rounded-xl border border-border p-3" data-testid="reopen-box">
              <TaskReopenFlash outcome={reopenFetcher.data} />
              <reopenFetcher.Form method="post" className="flex flex-wrap items-center gap-3">
                <input type="hidden" name="intent" value="reopen" />
                <input type="hidden" name="expected_status" value={detail.task.status} />
                <p className="text-sm text-fg-muted">
                  このタスクは終わっています。同じ作業ディレクトリのまま続きをやらせられます。
                </p>
                <Button type="submit" variant="primary" size="sm" disabled={reopening} data-testid="action-reopen">
                  <Icon name="rotate" />
                  {ACTION_LABELS.reopen}
                </Button>
              </reopenFetcher.Form>
            </div>
          )}
        </CardBody>
      </Card>

      {/* 生のイベント（従来の `GET /tasks/{id}/events` の絞り込み）。裏方の確認用に畳んで残す。 */}
      <Card>
        <CardBody>
          <details data-testid="raw-events">
            <summary className="cursor-pointer select-none text-sm font-medium text-fg">
              生のイベント（絞り込み）
            </summary>
            <div className="mt-3 space-y-4">
              <Form method="get" className="flex flex-wrap items-center gap-2" data-testid="timeline-filter-form">
                <input type="hidden" name="tab" value="timeline" />
                {EVENT_TYPES.map((type) => (
                  <label key={type} className={chipLabelClass}>
                    <input
                      type="checkbox"
                      name="types"
                      value={type}
                      defaultChecked={selectedTypes.has(type)}
                      className={checkboxClass}
                    />
                    {type}
                  </label>
                ))}
                <button type="submit" className={buttonClass({ variant: "secondary", size: "sm" })}>
                  絞り込み
                </button>
              </Form>
              {events.items.length === 0 ? (
                <EmptyState icon="activity" title="ありません。" />
              ) : (
                <ul className="space-y-1.5">
                  {events.items.map((row) => (
                    <li
                      key={row.id}
                      data-testid="event-item"
                      data-event-type={row.event.type}
                      className="rounded-lg border border-border px-3 py-2 text-sm"
                    >
                      {row.event.type === "worker_progress" ? (
                        <details>
                          <summary className="flex cursor-pointer flex-wrap items-center gap-2">
                            <Mono>#{row.seq}</Mono>
                            <span className="text-xs text-fg-subtle">{row.ts}</span>
                            <Badge tone="neutral">{row.event.type}</Badge>
                          </summary>
                          <p className="mt-1.5 text-fg-muted">{row.event.msg}</p>
                        </details>
                      ) : (
                        <p className="flex flex-wrap items-center gap-2">
                          <Mono>#{row.seq}</Mono>
                          <span className="text-xs text-fg-subtle">{row.ts}</span>
                          <Badge tone="neutral">{row.event.type}</Badge>
                        </p>
                      )}
                    </li>
                  ))}
                </ul>
              )}
              {events.has_more && <p className="text-xs text-fg-subtle">続きがあります（has_more）。</p>}
            </div>
          </details>
        </CardBody>
      </Card>
    </section>
  );
}

/**
 * タイムラインの 1 件の安定した key（`kind` ごとに taskd が持つ一意の値を使う。添字は使わない:
 * SSE の再検証で先頭に項目が増えると並びがずれるため）。
 */
function timelineItemKey(item: TimelineItem): string {
  switch (item.kind) {
    case "event":
      return `event-${item.seq}`;
    case "comment":
      return `comment-${item.comment.id}`;
    case "approval":
      return `approval-${item.approval.id}`;
    case "report":
      return `report-${item.report.id}`;
    case "delegation":
      return `delegation-${item.run_id}`;
    case "release":
      return `release-${item.sha12}`;
    default:
      return `${item.kind}-${item.at}-${item.action}`;
  }
}

/** タイムラインの 1 件（ADR-0044 D5）。`kind` ごとに出し分ける（知らない `kind` は無視する）。 */
function TimelineRow({ taskId, item }: { taskId: string; item: TimelineItem }) {
  return (
    <li
      data-testid="timeline-item"
      data-timeline-kind={item.kind}
      className="rounded-lg border border-border px-3 py-2 text-sm"
    >
      <p className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-fg-subtle">{item.at}</span>
        <Badge tone={TIMELINE_TONE[item.kind] ?? "neutral"}>{timelineKindLabel(item.kind)}</Badge>
        {item.kind === "event" && <Mono>{item.event.type}</Mono>}
      </p>
      <TimelineBody taskId={taskId} item={item} />
    </li>
  );
}

function TimelineBody({ taskId, item }: { taskId: string; item: TimelineItem }) {
  switch (item.kind) {
    case "event":
      return <TimelineEventBody event={item.event} />;
    case "comment":
      return <CommentBody comment={item.comment} />;
    case "approval":
      return (
        <div className="mt-1.5 text-fg-muted">
          <p className="whitespace-pre-wrap">{item.approval.question}</p>
          {item.approval.decision && (
            <p data-testid="timeline-approval-decision">
              決定: {item.approval.decision}
              {item.approval.answer ? `（${item.approval.answer}）` : ""}
            </p>
          )}
        </div>
      );
    case "report":
      return (
        <p className="mt-1.5 text-fg-muted">
          <Link to="/reports" className="font-medium text-primary hover:underline">
            {item.report.headline}
          </Link>
          <span className="ml-2 text-xs text-fg-subtle">{item.report.kind}</span>
        </p>
      );
    case "delegation":
      return (
        <div className="mt-1.5 text-fg-muted">
          <p className="text-xs text-fg-subtle">
            run{" "}
            <Link to={`/tasks/${taskId}/runs/${item.run_id}`} className="font-medium text-primary hover:underline">
              {item.run_id}
            </Link>
          </p>
          <ul className="mt-1 space-y-0.5">
            {item.tasks.map((child) => (
              <li key={child.id}>
                <Link to={`/tasks/${child.id}`} className="font-medium text-primary hover:underline">
                  {child.title}
                </Link>
                <span className="text-fg-subtle">（{child.status}）</span>
              </li>
            ))}
          </ul>
        </div>
      );
    case "release":
      return (
        <p className="mt-1.5 text-fg-muted" data-testid="timeline-release">
          このタスクの変更はリリース{" "}
          <Link to="/releases" className="font-mono font-medium text-primary hover:underline">
            {item.sha12}
          </Link>{" "}
          に入りました（コミット {item.commits.length} 件）。
        </p>
      );
    // ADR-0043 D5 / A2 が入るまでは作られない。来ても画面が壊れないように素のまま出す。
    case "integration":
      return (
        <p className="mt-1.5 text-fg-muted">
          {item.action}: {item.detail}
        </p>
      );
    default:
      return null;
  }
}

/** イベント 1 件の中身。人が読む意味のあるものだけ文にする（それ以外は `type` のバッジだけ）。 */
function TimelineEventBody({ event }: { event: Event }) {
  switch (event.type) {
    case "transitioned":
      return (
        <p className="mt-1.5 text-fg-muted">
          {event.from} → {event.to}（{event.reason}）
        </p>
      );
    case "worker_started":
      return (
        <p className="mt-1.5 text-fg-muted">
          run {event.run_id} 開始（{event.adapter} / {event.model}）
        </p>
      );
    case "worker_finished":
      return (
        <p className="mt-1.5 text-fg-muted">
          run {event.run_id} 終了: {event.outcome}
        </p>
      );
    case "worker_progress":
      return <p className="mt-1.5 text-fg-muted">{event.msg}</p>;
    case "artifact_produced":
      return <p className="mt-1.5 font-mono text-xs text-fg-muted">{event.artifact.name}</p>;
    case "review_verdict":
      return (
        <p className="mt-1.5 text-fg-muted">
          #{event.criterion_idx} {event.pass ? "pass" : "fail"} — {event.reason}
        </p>
      );
    case "answered":
      return <p className="mt-1.5 whitespace-pre-wrap text-fg-muted">{event.answer}</p>;
    // ADR-0044 D1: 人が編集したときの記録（何を変えたか）。
    case "edited":
      return (
        <p className="mt-1.5 text-fg-muted" data-testid="timeline-edited">
          {event.by} が変えた項目: {event.fields.join("・")}
        </p>
      );
    default:
      return null;
  }
}

/** コメント 1 件（ADR-0044 D2）。`dangerouslySetInnerHTML` は使わず、素のテキストとして出す。 */
function CommentBody({ comment }: { comment: TaskComment }) {
  return (
    <div className="mt-1.5" data-testid="comment-item" data-author-kind={comment.author_kind}>
      <p className="text-xs font-medium text-fg-subtle">
        {commentAuthorLabel(comment.author_kind)}
        {comment.author ? `（${comment.author}）` : ""}
      </p>
      <p className="mt-0.5 whitespace-pre-wrap text-fg">{comment.body}</p>
    </div>
  );
}

function TaskRefList({ label, testId, refs }: { label: string; testId: string; refs: TaskRef[] }) {
  return (
    <div data-testid={testId}>
      <p className="text-xs font-semibold uppercase tracking-wide text-fg-subtle">{label}</p>
      {refs.length === 0 ? (
        <p className="mt-1 text-sm text-fg-subtle">ありません。</p>
      ) : (
        <ul className="mt-1.5 divide-y divide-border overflow-hidden rounded-lg border border-border">
          {refs.map((ref) => (
            <li key={ref.id} className="px-3 py-2 text-sm">
              <Link to={`/tasks/${ref.id}`} className="font-medium text-primary hover:underline">
                {ref.title}
              </Link>
              <span className="text-fg-subtle">（{ref.status}）</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * 成果物 1 件の行（docs/adr/0006-g3-decisions.md D3/D4）。本体は「開く」を押したときだけ
 * `/files/tasks/:id/artifacts/:idx` を fetch し、taskd が返した実際の `Content-Type` でビューアを選ぶ
 * （拡張子からの推測はしない。taskd の値をそのまま使う）。403（`forbidden`）は一覧の `ArtifactView.forbidden`
 * だけで判定し、本体を取りに行かない。
 */
function ArtifactRow({ taskId, artifact }: { taskId: string; artifact: ArtifactView }) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState<{ contentType: string; content?: string } | null>(null);
  const href = `/files/tasks/${taskId}/artifacts/${artifact.idx}`;
  const canOpen = artifact.exists && !artifact.forbidden;
  const statusMessage = artifactStatusMessage(artifact);

  useEffect(() => {
    if (!open || body || !canOpen) return;
    let cancelled = false;
    (async () => {
      const res = await fetch(href);
      const contentType = res.headers.get("content-type") ?? "application/octet-stream";
      if (pickViewer(contentType, artifact.artifact.name) === "image") {
        if (!cancelled) setBody({ contentType });
        return;
      }
      const content = await res.text();
      if (!cancelled) setBody({ contentType, content });
    })();
    return () => {
      cancelled = true;
    };
  }, [open, body, canOpen, href, artifact.artifact.name]);

  return (
    <li data-testid="artifact-item" className="rounded-lg border border-border p-3 text-sm">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate font-mono text-xs text-fg" data-testid="artifact-name" title={artifact.artifact.name}>
            {artifact.artifact.name}
          </p>
          <p className="mt-0.5 text-xs text-fg-subtle">
            {artifact.artifact.kind} · run {artifact.run_id}
          </p>
        </div>
        {canOpen && (
          <div className="flex shrink-0 gap-2">
            <button
              type="button"
              onClick={() => setOpen((v) => !v)}
              data-testid="artifact-toggle"
              className={buttonClass({ variant: "secondary", size: "xs" })}
            >
              <Icon name={open ? "chevronDown" : "chevronRight"} />
              {open ? "閉じる" : "開く"}
            </button>
            <a
              href={`${href}?download=1`}
              download
              data-testid="artifact-download"
              className={buttonClass({ variant: "ghost", size: "xs" })}
            >
              保存
            </a>
          </div>
        )}
      </div>
      {statusMessage && (
        <p
          data-testid={artifact.forbidden ? "artifact-forbidden" : "artifact-missing"}
          className="mt-2 rounded-md border border-danger-border bg-danger-soft px-2.5 py-1.5 text-danger-soft-fg"
        >
          {statusMessage}
        </p>
      )}
      <Sha256Badge
        recorded={artifact.artifact.sha256}
        current={artifact.sha256_current}
        matches={artifact.sha256_matches}
      />
      {open && body && (
        <div className="mt-3">
          {pickViewer(body.contentType, artifact.artifact.name) === "image" ? (
            <ImageViewer src={href} alt={artifact.artifact.name} />
          ) : pickViewer(body.contentType, artifact.artifact.name) === "markdown" ? (
            <MarkdownViewer content={body.content ?? ""} />
          ) : (
            <CodeViewer content={body.content ?? ""} json={isJson(body.contentType)} />
          )}
        </div>
      )}
    </li>
  );
}

/**
 * loader が `taskdErrorResponse` で投げた `Response` を `isRouteErrorResponse` で判別する
 * （docs/adr/0004-g1-decisions.md D6）。taskd 停止中はこのルート自身が root と同じバナーを出し
 * （200 にはならないが 500 でもない。§6.5「500 にしない」）、404 は「タスクが見つかりません」にする。
 */
export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as TaskdRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="p-4">
          <TaskdBanner taskdApiUrl={data.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-2xl space-y-3 p-6">
        <h1 className="text-xl font-semibold text-fg">
          {data.status === 404 ? "タスクが見つかりません" : `エラー ${data.status}`}
        </h1>
        <Alert tone="danger">{data.detail}</Alert>
      </main>
    );
  }

  return (
    <main className="mx-auto max-w-2xl space-y-3 p-6">
      <h1 className="text-xl font-semibold text-fg">エラー</h1>
      <Alert tone="danger">予期しないエラーが起きました。</Alert>
    </main>
  );
}
