import { lazy, Suspense, useEffect, useState } from "react";
import {
  data,
  Form,
  isRouteErrorResponse,
  Link,
  useFetcher,
  useNavigate,
  useNavigation,
  useSearchParams,
} from "react-router";
import type {
  ActionError,
  DocsOpOutcome,
  RetryOutcome,
  TaskCommentOutcome,
  TaskEditOutcome,
  TaskReopenOutcome,
  TaskRereviewOutcome,
  TransitionOutcome,
} from "~/celeris/action-types";
import { retryData, transitionData } from "~/celeris/actions.server";
import type { CelerisClient } from "~/celeris/client.server";
import { getCelerisClient } from "~/celeris/client.server";
import { promoteArtifact, readArtifactPromoteBody } from "~/celeris/docs-admin.server";
import { type CelerisRouteErrorData, celerisErrorResponse, toActionError } from "~/celeris/errors";
import { runRetryAction, runTaskAction } from "~/celeris/route-actions.server";
import { loadTaskChanges, readTaskChangesQuery, type TaskChangesData } from "~/celeris/task-changes";
import { loadTaskFiles, readTaskFilesQuery, type TaskFilesData } from "~/celeris/task-files";
import { buildTaskEdit, commentOnTask, editTask, reopenTask, rereviewTask } from "~/celeris/tasks-admin.server";
import type {
  Action,
  ApprovalItem,
  ArtifactList,
  ArtifactView,
  CommentList,
  ConfigView,
  Event,
  EventsPage,
  Inbox,
  MilestoneView,
  OrgList,
  OrgNode,
  ProjectDetail,
  TaskComment,
  TaskDetail,
  TaskRef,
  Timeline,
  TimelineItem,
} from "~/celeris/types";
import { CodeViewer } from "~/components/CodeViewer";
/* ADR-0048 D2・フェーズ 74: worker_progress の折り畳みの中身は Console と同じ行を再利用する。 */
import { ReplyStepRow } from "~/components/ConsoleBlockItem";
import {
  ErrorFlash,
  RetryFlash,
  TaskCommentFlash,
  TaskEditFlash,
  TaskReopenFlash,
  TaskRereviewFlash,
  TransitionFlash,
} from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { HumanReviewPanel } from "~/components/HumanReviewPanel";
import { ImageViewer } from "~/components/ImageViewer";
import { LocalTime } from "~/components/LocalTime";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Sha256Badge } from "~/components/Sha256Badge";
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
  touchLinkClass,
  trHoverClass,
} from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, DataList, EmptyState, Mono } from "~/components/ui/misc";
import { Skeleton } from "~/components/ui/skeleton";
import type { Tone } from "~/components/ui/tone";
import { artifactStatusMessage, isJson, pickViewer } from "~/lib/artifact-view";
import { isValidLabel, MAX_LABELS, PRIORITY_LABELS } from "~/lib/board";
import { defaultPromotePath, docsHref, isMarkdownName } from "~/lib/docs";
import { shortId, splitOutcome } from "~/lib/format";
import { isKnowledgeFallback } from "~/lib/knowledge";
import {
  ASSIGNED_WHY_LABEL,
  assignedScoreLabel,
  commentAuthorLabel,
  docsErrorHint,
  harnessOptions,
  milestoneStatusLabel,
  PROMOTE_OVERWRITE_LABEL,
  PROMOTE_TO_DOC_LABEL,
  PROMOTE_TO_DOC_SUBMIT_LABEL,
  parseTaskTab,
  priorityFullLabel,
  TASK_CATEGORIES,
  TASK_MODES,
  TASK_TABS,
  type TaskTab,
  TIERS,
  taskCategoryLabel,
  taskModeLabel,
  taskTabLabel,
  tierLabel,
  timelineKindLabel,
} from "~/lib/labels";
import { isLiveStatusScreen } from "~/lib/live-status";
import { milestoneTitle } from "~/lib/project-index";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import {
  groupTimelineWorkerProgress,
  type TimelineWorkerProgressItem,
  timelineProgressGroupSummary,
  workerProgressStep,
} from "~/lib/task-timeline";
import { cn } from "~/lib/utils";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/tasks.$id";

// Phase 77（ADR-0055 性能予算）: 「変更」「ファイル」タブの本体（`~/components/task-changes.tsx`・
// `~/components/task-files.tsx`）は、5 つあるタブのうち一度に 1 つしか出ない（`?tab=` で切り替え）のに
// これまで両方とも静的 import していたので、どのタブを開いても他の 4 タブぶんの JS まで初回に届いていた。
// `React.lazy` にして、実際に選んだタブのチャンクだけを取りに行くようにする（ADR-0043 D5/D6 の中身・
// `~/routes/tasks.$id.changes.tsx`・`~/routes/tasks.$id.files.tsx` という兄弟ルートからの静的 import は
// そのまま残すので、そちらの動作・バンドルは変えない）。
const TaskChanges = lazy(() => import("~/components/task-changes").then((m) => ({ default: m.TaskChanges })));
const TaskFiles = lazy(() => import("~/components/task-files").then((m) => ({ default: m.TaskFiles })));

/**
 * タブの中身の読み込み中プレースホルダ（Phase 77、ADR-0055 D3「体感速度」）。チャンク待ち（`Suspense`）と
 * タブ切り替えのナビゲーション待ち（`useNavigation`）の両方で使う。高さは実際のタブの中身（見出し 1 行 +
 * カード数枚）に近い概算。
 */
function TaskTabSkeleton() {
  return (
    <div className="space-y-4" aria-hidden="true" data-testid="task-tab-skeleton">
      <Skeleton className="h-5 w-24" />
      <div className="space-y-3 rounded-xl border border-border bg-surface p-4">
        <Skeleton className="h-4 w-1/3" />
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-4 w-2/3" />
      </div>
    </div>
  );
}

/**
 * `docs/celeris-api-v1.md` §3.6 の `types` フィルタの選択肢。`Event` の `type` タグと同じ。
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
  // ADR-0070 D2（Phase 116）。failed の失敗バナーに置く。
  rereview: "再レビュー",
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
  doc: "teal",
  // ADR-0047 D4/D5（Phase 62）。
  knowledge: "primary",
};

export interface TaskDetailData {
  detail: TaskDetail;
  events: EventsPage;
  artifacts: ArtifactList;
  /** ADR-0044 D5: `GET /tasks/{id}/timeline`（時刻の昇順で 1 本）。 */
  timeline: Timeline;
  /** ADR-0044 D2: `GET /tasks/{id}/comments`（古い順）。タブの件数と、コメント欄の見出しに使う。 */
  comments: CommentList;
  /**
   * ADR-0055 D2 ラウンド 6（フェーズ 74）: タイムラインの相対時刻表示（`relativeTimeLabel`。
   * `~/routes/approvals.tsx` の `fetchedAt` と同じ作り）の基準時刻。loader が読み込んだ時刻。
   */
  fetchedAt: string;
  /** ADR-0044 D1: 編集フォームの「担当」プルダウンの選択肢（`GET /org`。落ちても画面は出す）。 */
  org: OrgNode[];
  /** ADR-0044 D1: 編集フォームの「途中目標」プルダウン（そのタスクの案件のものだけ）。 */
  milestones: MilestoneView[];
  /**
   * ADR-0046 D3（Phase 59）: 編集フォームの「ハーネス」プルダウンの選択肢（`GET /config` の
   * `genres[].id` = ハーネスのレジストリの射影）。空なら自由記述の欄にする（celeris 側で検証しない
   * 最小構成。`~/routes/org.tsx` の「分野」欄と同じ考え方）。落ちても画面は出す。
   */
  genres: string[];
  /**
   * ADR-0046 D5（Phase 59 / G21）: matching が担当を決めた理由（「なぜこの担当か」）。
   * `GET /tasks/{id}/events` に載っている最新の `assigned` イベント。無ければ null
   * （明示の `assignee` で作られた・組織を使っていない構成 等）。
   */
  assignedEvent: { node: string; score: number; reason: string } | null;
  /**
   * ADR-0043 D6 + ADR-0044 D5（Phase 52 + 53 のマージ）: 「ファイル」タブの中身。
   * **`?tab=files` のときだけ**引く（他のタブで毎回 `GET /tasks/{id}/tree` を叩かないため）。
   * 作業ツリーが無いタスク（404 `file_not_found`）やリポジトリを使わないタスクでは `error` に
   * celeris の文言が入り、タブはその文言だけを出す（ページ全体は落とさない）。
   */
  files: { data: TaskFilesData; error: null } | { data: null; error: ActionError } | null;
  /**
   * ADR-0043 D5 + ADR-0044 D5（Phase 53 + 54 のマージ）: 「変更」タブの中身。
   * **`?tab=changes` のときだけ**引く（`GET /tasks/{id}/changes` はリポジトリごとに git を数回起こすので、
   * 他のタブを見ているあいだは走らせない。celeris 側の U54-1）。ブランチも作業ツリーも無いタスクや
   * リポジトリを使わないタスクでは `error` に celeris の文言が入り、タブはその文言だけを出す。
   */
  changes: { data: TaskChangesData; error: null } | { data: null; error: ActionError } | null;
  /**
   * 人のレビュー待ち（`reviewing` のタスク、または `kind = approval` のタスク）の判断材料。
   * `GET /inbox` の未決の承認のうち、このタスクが承認タスク自身か、その親のもの。落ちても画面は出す（空）。
   */
  humanReview: ApprovalItem[];
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
 * `GET /tasks/{id}/comments` を並列に呼び、応答をそのまま返す（派生値は celeris 側で計算済み。GUI は再計算しない）。
 * celeris 停止中・タスクが無い（404 `task_not_found`）等は呼び出し側（`loader`）が `Response` に変換して投げる
 * （docs/adr/0004-g1-decisions.md D6。本番ビルドは素の Error を ErrorBoundary に渡す前に汎用 500 へ
 * サニタイズするため、`Response` として投げないと celeris 停止中でもバナーではなく 500 になってしまう）。
 * 生ログ本体は `/tasks/:id/runs/:runId`（別ルート）、DAG は `/graph`（docs/adr/0006-g3-decisions.md D4）。
 *
 * `GET /org` と案件の詳細は**編集フォームの選択肢**（ADR-0044 D1: 担当・途中目標のプルダウン）にも使うので、
 * 担当や案件が未設定でも常に引く（落ちたら選択肢が空になるだけ。画面は出す）。
 */
export async function loadTaskDetail(client: CelerisClient, taskId: string, request: Request): Promise<TaskDetailData> {
  const url = new URL(request.url);
  // フォームは `types` チェックボックスごとに 1 つずつ付ける（`?types=a&types=b`）。
  // celeris 側はカンマ区切りの単一パラメータを期待する（docs/celeris-api-v1.md §3.6）ので、ここで結合する。
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
  // 案件・途中目標・担当の名前（監査 M2）と、編集フォームの選択肢（ADR-0044 D1、ADR-0046 D3 の「ハーネス」）。
  const assigneeId = detail.task.assignee ?? null;
  const projectId = detail.task.project_id ?? null;
  const [project, org, config] = await Promise.all([
    projectId
      ? client
          .get<ProjectDetail>(`/projects/${encodeURIComponent(projectId)}`, { signal: request.signal })
          .catch(() => null)
      : Promise.resolve(null),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => null),
    client.get<ConfigView>("/config", { signal: request.signal }).catch(() => null),
  ]);
  // ADR-0046 D5（Phase 59 / G21）: 「なぜこの担当か」。`assigned` は matching が決めたときに 1 件だけ
  // 付く（明示の assignee で作られたタスクには無い）。複数走っていれば直近を出す。
  const assignedEvent = events.items
    .map((row) => row.event)
    .filter((e): e is Extract<Event, { type: "assigned" }> => e.type === "assigned")
    .at(-1);
  // ADR-0043 D6: 「ファイル」タブを見ているときだけ作業ツリーを引く。403 / 404 は画面に出す
  // （兄弟のルート `/tasks/:id/files` はページ自体を落とすが、タブでは他のタブが見えていてほしい）。
  let files: TaskDetailData["files"] = null;
  if (parseTaskTab(url.searchParams.get("tab")) === "files") {
    try {
      files = { data: await loadTaskFiles(client, taskId, readTaskFilesQuery(request), request.signal), error: null };
    } catch (e) {
      files = { data: null, error: toActionError(e) };
    }
  }
  // ADR-0043 D5:「変更」タブを見ているときだけ差分を引く（`GET /tasks/{id}/changes` は git を起こす）。
  // 404 / 403 はタブの中に出す（兄弟のルート `/tasks/:id/changes` はページ自体を落とす）。
  let changes: TaskDetailData["changes"] = null;
  if (parseTaskTab(url.searchParams.get("tab")) === "changes") {
    try {
      changes = {
        data: await loadTaskChanges(client, taskId, readTaskChangesQuery(request), request.signal),
        error: null,
      };
    } catch (e) {
      changes = { data: null, error: toActionError(e) };
    }
  }
  // 人のレビュー待ちの判断材料（`GET /inbox` の承認）。reviewing の親か承認タスク自身のときだけ引く。
  let humanReview: ApprovalItem[] = [];
  if (detail.task.status === "reviewing" || detail.task.kind === "approval") {
    const inbox = await client.get<Inbox>("/inbox", { signal: request.signal }).catch(() => null);
    humanReview = (inbox?.approvals ?? []).filter((a) => a.approval.id === taskId || a.parent?.id === taskId);
  }
  return {
    detail,
    events,
    artifacts,
    timeline,
    comments,
    fetchedAt: new Date().toISOString(),
    org: org?.items ?? [],
    milestones: project?.milestones ?? [],
    genres: (config?.genres ?? []).map((g) => g.id),
    assignedEvent: assignedEvent
      ? { node: assignedEvent.node, score: assignedEvent.score, reason: assignedEvent.reason }
      : null,
    files,
    changes,
    humanReview,
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
    return await loadTaskDetail(getCelerisClient(), params.id, request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export async function action({ request, params }: Route.ActionArgs) {
  const form = await request.formData();
  const client = getCelerisClient();
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
  // ADR-0070 D2（Phase 116）: 再レビューは `retry`/`reopen` と同じ理由でここで分ける
  // （`TransitionInput` とは語彙が違う。`task_ops::comment::rereview` を呼ぶ）。
  if (intent === "rereview") {
    const outcome = await rereviewTask(client, params.id, form, request.signal);
    return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
  }
  // ADR-0044 D7（Phase 57 / G20）: 成果物を案件の文書に昇格する（**管理系**。宛先は人が決める）。
  if (intent === "promote") {
    const outcome = await promoteArtifact(client, params.id, readArtifactPromoteBody(form), request.signal);
    return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
  }
  const outcome = await runTaskAction(client, params.id, form, request.signal);
  return transitionData(outcome);
}

export default function TaskDetailPage({ loaderData }: Route.ComponentProps) {
  const {
    detail,
    events,
    artifacts,
    timeline,
    comments,
    fetchedAt,
    org,
    milestones,
    genres,
    assignedEvent,
    files,
    changes,
    humanReview,
    place,
  } = loaderData;
  const { task } = detail;
  const [searchParams] = useSearchParams();
  const tab = parseTaskTab(searchParams.get("tab"));
  // Phase 77（ADR-0055 D3「体感速度」）: タブを切り替えると `?tab=` が変わって loader が再実行される
  // （タブごとに `changes`/`files`/`timeline` の中身が違うため）。その間は前のタブの中身がそのまま
  // 残るだけで何も動いて見えないので、この画面への遷移が pending の間はタブの中身をスケルトンに差し替える
  // （高さは `TaskTabSkeleton` の概算。レイアウトのガタつきを避けるため中身の種類では変えない）。
  const navigation = useNavigation();
  const tabNavigationPending = navigation.state === "loading" && navigation.location?.pathname === `/tasks/${task.id}`;
  // 操作の結果は fetcher に載せる（監査 H1。SSE の再検証で `actionData` が消えるのを避ける）。
  const fetcher = useFetcher<TransitionOutcome>();
  const submitting = fetcher.state !== "idle";
  // Phase 31: 「やり直す」は別のタスクを新しく作る（`TransitionOutcome` とは形が違う）ので別の fetcher。
  // 成功したら新しいタスクへ遷移する（fetcher はナビゲーションを行わないので `useNavigate` で明示的に行う）。
  const retryFetcher = useFetcher<RetryOutcome>();
  const retrying = retryFetcher.state !== "idle";
  // ADR-0070 D2（Phase 116）: 失敗バナーの「再レビュー」「取り下げ」。retry は上の retryFetcher を共有する。
  const rereviewFetcher = useFetcher<TaskRereviewOutcome>({ key: `task-rereview-${task.id}` });
  const rereviewing = rereviewFetcher.state !== "idle";
  const dismissFetcher = useFetcher<TaskCommentOutcome>({ key: `task-dismiss-${task.id}` });
  const dismissing = dismissFetcher.state !== "idle";
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
                {/* U-G32-2 の解消（Phase 84）: 詳細は開いたまま SSE の再検証で更新され続ける画面なので、
                    状態バッジをライブリージョンにする（`~/lib/live-status.ts`）。 */}
                <StatusBadge
                  status={task.status}
                  role={isLiveStatusScreen("task-detail") ? "status" : undefined}
                  data-testid="task-status"
                />
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
                      className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
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
                      className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
                    >
                      {place.assigneeName ?? place.assigneeId}
                    </Link>
                    {/* ADR-0046 D5（Phase 59 / G21）: 「なぜこの担当か」。matching が決めたタスクにだけ出る
                        （`Event::Assigned`。明示の assignee で作られたタスクには無い）。 */}
                    {assignedEvent && assignedEvent.node === place.assigneeId && (
                      <span
                        className="ml-1.5 text-sm text-fg-subtle lg:text-xs"
                        data-testid="task-assigned-why"
                        title={`${ASSIGNED_WHY_LABEL}: ${assignedEvent.reason}（${assignedScoreLabel(assignedEvent.score)}）`}
                      >
                        （{ASSIGNED_WHY_LABEL}: {assignedEvent.reason}）
                      </span>
                    )}
                  </p>
                )}
                {place.milestoneTitle && <p data-testid="task-milestone">途中目標: {place.milestoneTitle}</p>}
              </div>
              <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-fg-muted">
                {detail.cluster && (
                  <p data-testid="task-cluster">
                    cluster:{" "}
                    <Link to="/clusters" className={cn(touchLinkClass, "font-medium text-primary hover:underline")}>
                      {detail.cluster}
                    </Link>
                    {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
                    <span className="ml-2 text-sm text-fg-subtle lg:text-xs" data-testid="task-workspace-note">
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
                    <Link
                      to={`/tasks/${task.parent_id}`}
                      className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
                    >
                      {task.parent_id}
                    </Link>
                  </p>
                )}
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              {/* 作業ツリー（ADR-0043 D6、Phase 52 / G16）。ADR-0044 D5 のタブの殻ができたので
                  「ファイル」タブに載せ替えた。兄弟のルート `/tasks/:id/files` も従来どおり動く
                  （同じ `~/components/task-files.tsx` を全画面で出す）。 */}
              <Link
                to={`/tasks/${task.id}?tab=files`}
                data-testid="task-files-link"
                className={buttonClass({ variant: "secondary", size: "sm" })}
              >
                <Icon name="folder" />
                ファイル
              </Link>
              {/* 変更の取り込み（ADR-0043 D5、Phase 54 / G18）。ADR-0044 D5 のタブの殻ができたので
                  「変更」タブに載せ替えた。兄弟のルート `/tasks/:id/changes` も従来どおり動く
                  （同じ `~/components/task-changes.tsx` を全画面で出す）。 */}
              <Link
                to={`/tasks/${task.id}?tab=changes`}
                data-testid="task-changes-link"
                className={buttonClass({ variant: "secondary", size: "sm" })}
              >
                <Icon name="gitBranch" />
                変更
              </Link>
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
        </div>
      </section>

      {/* ADR-0070 D1/D2（Phase 116）: failed のタスクは、原因の分類と「やり直す」「再レビュー」
          「取り下げ」をどのタブからでも見える位置に出す（受け入れ条件 D6(e)）。 */}
      {detail.failure && (
        <FailureBanner
          taskId={task.id}
          failure={detail.failure}
          actions={detail.actions}
          retryFetcher={retryFetcher}
          retrying={retrying}
          rereviewFetcher={rereviewFetcher}
          rereviewing={rereviewing}
          dismissFetcher={dismissFetcher}
          dismissing={dismissing}
        />
      )}

      {/* ADR-0044 D5: 概要 / タイムライン / 変更 / ファイル / 成果物。`?tab=` が状態なのでリンクできる。 */}
      <TaskTabs
        current={tab}
        searchParams={searchParams}
        counts={{ timeline: timeline.items.length, artifacts: artifacts.items.length }}
      />

      {tabNavigationPending ? (
        <TaskTabSkeleton />
      ) : (
        <>
          {tab === "overview" && (
            <OverviewTab
              detail={detail}
              artifactCount={artifacts.items.length}
              org={org}
              milestones={milestones}
              genres={genres}
              fetcher={fetcher}
              submitting={submitting}
              retryFetcher={retryFetcher}
              retrying={retrying}
              fetchedAt={fetchedAt}
              humanReview={humanReview}
            />
          )}

          {tab === "timeline" && (
            <TimelineTab
              taskId={task.id}
              detail={detail}
              timeline={timeline}
              comments={comments}
              events={events}
              fetchedAt={fetchedAt}
            />
          )}

          {tab === "changes" && (
            <section aria-labelledby="changes-heading" data-testid="changes-section" className="space-y-4">
              <h2 id="changes-heading" className="text-[0.95rem] font-semibold text-fg">
                変更
              </h2>
              {/* ADR-0043 D5（Phase 54 / G18）の本物。中身は `~/components/task-changes.tsx`。
                  取り込みの `fetcher` と差分のリンクは兄弟のルート `/tasks/:id/changes` に出る
                  （「ファイル」タブと同じ作り）。Phase 77: `React.lazy`（このファイル冒頭）なので `Suspense` で包む。 */}
              {changes?.data ? (
                <Suspense fallback={<TaskTabSkeleton />}>
                  <TaskChanges
                    taskId={task.id}
                    changes={changes.data.changes}
                    diff={changes.data.diff}
                    diffError={changes.data.diffError}
                    diffRepo={changes.data.diffRepo}
                    diffPath={changes.data.diffPath}
                  />
                </Suspense>
              ) : changes?.error ? (
                <EmptyState icon="gitBranch" title="取り込める変更がありません" data-testid="task-changes-unavailable">
                  {changes.error.detail}
                </EmptyState>
              ) : null}
            </section>
          )}

          {tab === "files" && (
            <section aria-labelledby="files-heading" data-testid="files-section" className="space-y-4">
              <h2 id="files-heading" className="text-[0.95rem] font-semibold text-fg">
                ファイル
              </h2>
              {/* ADR-0043 D6（Phase 52 / G16）の本物。中身は `~/components/task-files.tsx`。
                  Phase 77: `React.lazy`（このファイル冒頭）なので `Suspense` で包む。 */}
              {files?.data ? (
                <Suspense fallback={<TaskTabSkeleton />}>
                  <TaskFiles
                    taskId={task.id}
                    tree={files.data.tree}
                    file={files.data.file}
                    fileError={files.data.fileError}
                    filePath={files.data.filePath}
                  />
                </Suspense>
              ) : files?.error ? (
                <EmptyState icon="folder" title="作業ツリーがありません" data-testid="task-files-unavailable">
                  {files.error.detail}
                </EmptyState>
              ) : null}
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
                        <ArtifactRow
                          key={artifact.idx}
                          taskId={task.id}
                          artifact={artifact}
                          projectId={task.project_id ?? null}
                          taskTitle={task.title}
                          category={task.category ?? null}
                        />
                      ))}
                    </ul>
                  )}
                </CardBody>
              </Card>
            </section>
          )}
        </>
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
    // フェーズ 71（ADR-0055 D2 ラウンド 3）: タブは横スクロールするピル行（`overflow-x-auto`、折り返さない）。
    // 5 つのタブ名が並んでも 393px に収まらないことがあるため、切れた分はスクロールで見せる（D1-1/D1-6 の
    // 「overflow-x-auto の箱」と同じ扱い。`lg:` は元の折り返し行のまま）。
    // Phase 95（目視点検の所見、重さ「高」）: 5 つ目の「成果物」が右端で文字の途中で切れているのに、
    // 横にまだ続きがあると気付ける手がかりが無かった。右端に `pointer-events-none` のフェードを重ね、
    // タップ領域・DOM 構造・スクロール自体は変えずに「まだ右にある」ことだけを示す（`lg:hidden`。
    // デスクトップは折り返すのでフェード不要）。
    // フェードの右端をスクロール箱の実際の右端（`-mx-4` で画面端まで伸びた見た目上の境界）に合わせるため、
    // `relative` と `-mx-4`（bleed）は `nav` 側に置く（`ul` は `px-4` だけ残す）。`ul` にだけ `relative` を
    // 付けると、bleed していない `nav` の内側 16px 分だけフェードがずれて中途半端な位置に出てしまう。
    <nav aria-label="タスクの内訳" data-testid="task-tabs" className="relative -mx-4 lg:mx-0">
      <ul className="flex gap-1 overflow-x-auto border-b border-border px-4 lg:flex-wrap lg:overflow-visible lg:px-0">
        {TASK_TABS.map((t) => {
          const params = new URLSearchParams(searchParams);
          params.set("tab", t);
          const active = t === current;
          const count = t === "timeline" ? counts.timeline : t === "artifacts" ? counts.artifacts : null;
          return (
            <li key={t} className="shrink-0">
              <Link
                to={`?${params.toString()}`}
                replace
                data-testid={`task-tab-${t}`}
                data-active={active ? "true" : undefined}
                aria-current={active ? "page" : undefined}
                className={cn(
                  // ADR-0055 D1-2: タップ領域 44×44 以上。
                  "-mb-px inline-flex min-h-11 items-center gap-1.5 rounded-t-lg border-b-2 px-3.5 py-2 text-sm font-medium no-underline transition-colors",
                  active
                    ? "border-primary text-primary"
                    : "border-transparent text-fg-muted hover:border-border-strong hover:text-fg",
                )}
              >
                {taskTabLabel(t)}
                {count !== null && count > 0 && (
                  // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
                  <span className="text-sm tabular-nums text-fg-subtle lg:text-xs">{count}</span>
                )}
              </Link>
            </li>
          );
        })}
      </ul>
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-y-0 right-0 w-8 bg-gradient-to-l from-bg to-transparent lg:hidden"
      />
    </nav>
  );
}

const FAILURE_CLASS_LABEL: Record<"infra" | "work", string> = {
  infra: "インフラ",
  work: "作業内容",
};

/**
 * ADR-0070 D1/D2（Phase 116）: failed のタスク詳細に出す赤いバナー。「失敗: <分類> — <理由>」に、
 * 配送済みなら一言添え、「やり直す」（常に。`retry`）「再レビュー」（`actions` に `rereview` が
 * あるときだけ）「取り下げ」（状態は変えず、対応不要と記録するコメントを残すだけ。ADR-0070 D2）を出す。
 * `docs/gui/help`（`/help`）の「失敗したタスクの直し方」と同じ言葉づかい。
 */
function FailureBanner({
  taskId,
  failure,
  actions,
  retryFetcher,
  retrying,
  rereviewFetcher,
  rereviewing,
  dismissFetcher,
  dismissing,
}: {
  taskId: string;
  failure: NonNullable<TaskDetail["failure"]>;
  actions: Action[];
  retryFetcher: ReturnType<typeof useFetcher<RetryOutcome>>;
  retrying: boolean;
  rereviewFetcher: ReturnType<typeof useFetcher<TaskRereviewOutcome>>;
  rereviewing: boolean;
  dismissFetcher: ReturnType<typeof useFetcher<TaskCommentOutcome>>;
  dismissing: boolean;
}) {
  return (
    <section aria-labelledby="failure-banner-heading" data-testid="failure-banner">
      <Alert tone="danger" icon="alert" className="rounded-2xl border-2 p-5 shadow-sm">
        <p id="failure-banner-heading" className="text-base font-semibold" data-testid="failure-banner-summary">
          失敗: {FAILURE_CLASS_LABEL[failure.class]} — {failure.reason}
        </p>
        {failure.delivered_release && (
          <p data-testid="failure-banner-delivered">
            成果は配送済み（release {failure.delivered_release}）だがレビューで不合格。
          </p>
        )}
        <div className="mt-3 flex flex-wrap items-center gap-2">
          {actions.includes("retry") && (
            <retryFetcher.Form method="post" action={`/tasks/${taskId}`}>
              <input type="hidden" name="intent" value="retry" />
              <Button type="submit" variant="primary" size="sm" disabled={retrying} data-testid="failure-banner-retry">
                <Icon name="rotate" />
                やり直す
              </Button>
            </retryFetcher.Form>
          )}
          {actions.includes("rereview") && (
            <rereviewFetcher.Form method="post" action={`/tasks/${taskId}`}>
              <input type="hidden" name="intent" value="rereview" />
              <Button
                type="submit"
                variant="secondary"
                size="sm"
                disabled={rereviewing}
                data-testid="failure-banner-rereview"
              >
                <Icon name="check" />
                再レビュー
              </Button>
            </rereviewFetcher.Form>
          )}
          <dismissFetcher.Form method="post" action={`/tasks/${taskId}`}>
            <input type="hidden" name="intent" value="comment" />
            <input type="hidden" name="body" value="取り下げ: 対応不要と判断しました（状態は failed のまま）。" />
            <Button type="submit" variant="ghost" size="sm" disabled={dismissing} data-testid="failure-banner-dismiss">
              <Icon name="x" />
              取り下げ
            </Button>
          </dismissFetcher.Form>
        </div>
        <RetryFlash outcome={retryFetcher.data} />
        <TaskRereviewFlash outcome={rereviewFetcher.data} />
        {dismissFetcher.data && !dismissFetcher.data.ok && <ErrorFlash error={dismissFetcher.data.error} />}
        {dismissFetcher.data?.ok && (
          <p className="mt-1 text-sm text-fg-muted" data-testid="failure-banner-dismissed">
            取り下げのコメントを記録しました。
          </p>
        )}
      </Alert>
    </section>
  );
}

/** 概要タブ（ADR-0044 D5）: 従来の詳細一式に、人が直接直せる編集フォーム（D1）を足したもの。 */
function OverviewTab({
  detail,
  artifactCount,
  org,
  milestones,
  genres,
  fetcher,
  submitting,
  retryFetcher,
  retrying,
  fetchedAt,
  humanReview,
}: {
  humanReview: ApprovalItem[];
  detail: TaskDetail;
  artifactCount: number;
  org: OrgNode[];
  milestones: MilestoneView[];
  genres: string[];
  fetcher: ReturnType<typeof useFetcher<TransitionOutcome>>;
  submitting: boolean;
  retryFetcher: ReturnType<typeof useFetcher<RetryOutcome>>;
  retrying: boolean;
  fetchedAt: string;
}) {
  const { task } = detail;
  return (
    <>
      {humanReview.length > 0 && (
        <HumanReviewPanel
          items={humanReview}
          criteria={detail.criteria}
          priorReview={detail.prior_review}
          reviewTaskId={task.id}
        />
      )}

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
            {/* フェーズ 71（ADR-0055 D2 ラウンド 3）: メタデータは 2 列を既定にする（`~/components/ui/misc.tsx`
                の既定 `DataList` は `sm:`（640px）未満は 1 列なので、393px の実機では常に 1 列になってしまう）。
                374px 未満のごく狭い端末だけ 1 列に折り返す。 */}
            <DataList className="grid-cols-1 min-[374px]:grid-cols-2 sm:grid-cols-2 lg:grid-cols-4">
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

      {/* ADR-0044 D1（Phase 53）: 人がタスクを細かく直せる。終端のタスクは celeris が 409 を返すので出さない。 */}
      {detail.actions.includes("edit") && (
        <TaskEditSection key={task.updated_at} detail={detail} org={org} milestones={milestones} genres={genres} />
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
            <DataList className="grid-cols-1 min-[374px]:grid-cols-2 sm:grid-cols-2 lg:grid-cols-4">
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
              <EmptyState icon="checkCircle" title="ありません。" compact />
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
                          className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
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
              <EmptyState icon="terminal" title="ありません。" compact />
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
                        <td className={cn(tdClass, "font-mono text-xs break-all")} title={run.run_id}>
                          {shortId(run.run_id)}
                        </td>
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
                        {/* フェーズ 74（ADR-0055 D2 ラウンド 6）: 生の ISO は表の幅も取るので相対表示に揃える。 */}
                        <td className={cn(tdClass, "whitespace-nowrap text-xs text-fg-subtle")}>
                          <LocalTime iso={run.started_at} fetchedAtIso={fetchedAt} />
                        </td>
                        <td className={cn(tdClass, "whitespace-nowrap text-xs text-fg-subtle")}>
                          {run.finished_at ? <LocalTime iso={run.finished_at} fetchedAtIso={fetchedAt} /> : "-"}
                        </td>
                        <td className={tdClass}>
                          {run.outcome ? (
                            <Badge tone={OUTCOME_TONE[run.outcome] ?? "neutral"} title={run.outcome_text ?? undefined}>
                              {run.outcome}
                            </Badge>
                          ) : (
                            <span className="text-fg-subtle">-</span>
                          )}
                          {/* 長い理由・要約はステータス欄に混ぜず、折り畳みの中に分ける。 */}
                          {run.outcome_text && (
                            <details data-testid="run-outcome-detail" className="mt-1 max-w-xs text-xs text-fg-muted">
                              <summary className="cursor-pointer select-none">詳細</summary>
                              <p className="mt-1 max-h-48 overflow-y-auto whitespace-pre-wrap break-words">
                                {run.outcome_text}
                              </p>
                            </details>
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
              <EmptyState icon="users" title="ありません。" compact />
            ) : (
              <ul className="space-y-3">
                {detail.delegated.map((group) => (
                  <li
                    key={group.run_id}
                    data-testid="delegated-group"
                    data-run-id={group.run_id}
                    className="rounded-lg border border-border p-3 text-sm"
                  >
                    {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
                    <p className="text-sm text-fg-subtle lg:text-xs">
                      run{" "}
                      <Link
                        to={`/tasks/${task.id}/runs/${group.run_id}`}
                        className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
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
                            className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
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
              <EmptyState icon="rotate" title="ありません。" compact />
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
              <EmptyState icon="message" title="ありません。" compact />
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
            {/* フェーズ 71（ADR-0055 D2 ラウンド 3）: 操作は画面下の全幅ボタン（モバイルは縦積み、
                `sm:` からは元どおり横並び）。 */}
            {detail.actions.length === 0 ? (
              <EmptyState icon="ban" title="できる操作はありません。" compact />
            ) : (
              <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap sm:gap-4">
                {detail.actions.includes("approve") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto sm:max-w-xs"
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
                      className="w-full sm:w-auto"
                    >
                      <Icon name="check" />
                      {ACTION_LABELS.approve}
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("reject") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto sm:max-w-xs"
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
                    <Button
                      type="submit"
                      variant="danger"
                      size="sm"
                      disabled={submitting}
                      data-testid="action-reject"
                      className="w-full sm:w-auto"
                    >
                      <Icon name="x" />
                      {ACTION_LABELS.reject}
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("answer") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto sm:max-w-sm"
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
                      className="w-full sm:w-auto"
                    >
                      <Icon name="send" />
                      回答する
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("cancel") && (
                  <fetcher.Form
                    method="post"
                    className="flex w-full flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto sm:max-w-xs"
                  >
                    <input type="hidden" name="intent" value="cancel" />
                    <input type="hidden" name="expected_status" value={task.status} />
                    <Button
                      type="submit"
                      variant="danger"
                      size="sm"
                      disabled={submitting}
                      data-testid="action-cancel"
                      className="w-full sm:w-auto"
                    >
                      <Icon name="ban" />
                      {ACTION_LABELS.cancel}
                    </Button>
                  </fetcher.Form>
                )}
                {detail.actions.includes("retry") && (
                  <retryFetcher.Form
                    method="post"
                    className="flex w-full flex-col gap-2 rounded-lg border border-border p-3 sm:w-auto sm:max-w-xs"
                  >
                    <input type="hidden" name="intent" value="retry" />
                    {/* ADR-0070 D2 追記（Phase 116）: 既定は ready。draft のまま始めたいときだけ
                        チェックする（既定を逆にした。チェック無し = ready）。
                        `min-h-11`: タップ領域 44 以上（mobile-audit で検出。他の同種チェックボックス
                        〈inbox.tsx / projects.$id.tsx〉と同じ）。 */}
                    <label className="flex min-h-11 items-center gap-2 text-sm text-fg">
                      <input type="checkbox" name="draft" value="true" className={checkboxClass} />
                      下書き（draft）のまま始める（既定は受け入れ済み = ready）
                    </label>
                    <Button
                      type="submit"
                      variant="primary"
                      size="sm"
                      disabled={retrying}
                      data-testid="action-retry"
                      className="w-full sm:w-auto"
                    >
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
 * （小文字・`[a-z0-9-]`・最大 8 個）だけはチップを足すときの入力補助として弾く（正は celeris の 422）。
 * `running` / `reviewing` でも編集できる（走っている run は止まらず、次の run から効く）。
 */
function TaskEditSection({
  detail,
  org,
  milestones,
  genres,
}: {
  detail: TaskDetail;
  org: OrgNode[];
  milestones: MilestoneView[];
  /** ADR-0046 D3（Phase 59）: ハーネス（`genre` 列）の選択肢。空なら自由記述にする。 */
  genres: string[];
}) {
  const { task } = detail;
  const harnesses = harnessOptions(genres);
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

              {/* ADR-0046 D3（Phase 59）: ハーネス（`Task.genre` 列がそのまま harness id）。空の選択肢を
                  選べば `null`（外す）を送る（`NULLABLE_FIELDS`。§3.74）。 */}
              <div>
                <label htmlFor="edit-harness" className={labelClass}>
                  ハーネス
                </label>
                {harnesses.length > 0 ? (
                  <select
                    id="edit-harness"
                    name="harness"
                    defaultValue={task.genre ?? ""}
                    data-testid="edit-harness"
                    className={cn(selectClass, "mt-1.5")}
                  >
                    <option value="">（決めない）</option>
                    {harnesses.map((h) => (
                      <option key={h} value={h}>
                        {h}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    id="edit-harness"
                    name="harness"
                    type="text"
                    defaultValue={task.genre ?? ""}
                    data-testid="edit-harness"
                    className={cn(inputClass, "mt-1.5")}
                  />
                )}
                <p className={hintClass}>担当が無ければ、これと能力タグの重なりで決まります（ADR-0046 D5）。</p>
              </div>

              {/* ADR-0046 D2（Phase 59）: 必要な能力タグ（開いた語彙。空白/カンマ区切り）。 */}
              <div>
                <label htmlFor="edit-skills" className={labelClass}>
                  能力タグ
                </label>
                <input
                  id="edit-skills"
                  name="skills"
                  type="text"
                  defaultValue={(task.skills ?? []).join(", ")}
                  data-testid="edit-skills"
                  className={cn(inputClass, "mt-1.5")}
                />
                <p className={hintClass}>空白かカンマ区切り（例: rust, benchmark）。小文字の `[a-z0-9._-]` だけ。</p>
              </div>

              {/* ADR-0046 D4（Phase 59）: 進め方。前置きの規則とレビューの厳しさが変わる。 */}
              <div>
                <label htmlFor="edit-mode" className={labelClass}>
                  進め方
                </label>
                <select
                  id="edit-mode"
                  name="mode"
                  defaultValue={task.mode ?? "production"}
                  data-testid="edit-mode"
                  className={cn(selectClass, "mt-1.5")}
                >
                  {TASK_MODES.map((m) => (
                    <option key={m} value={m}>
                      {taskModeLabel(m)}
                    </option>
                  ))}
                </select>
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
                <p role="alert" className="mt-1 text-sm text-danger lg:text-xs" data-testid="edit-label-error">
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
  fetchedAt,
}: {
  taskId: string;
  detail: TaskDetail;
  timeline: Timeline;
  comments: CommentList;
  events: EventsPage;
  fetchedAt: string;
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
              {groupTimelineWorkerProgress(timeline.items).map((display) =>
                display.kind === "progress_group" ? (
                  <TimelineProgressGroupRow
                    key={`progress-${display.items[0].seq}`}
                    items={display.items}
                    fetchedAt={fetchedAt}
                  />
                ) : (
                  <TimelineRow
                    key={timelineItemKey(display.item)}
                    taskId={taskId}
                    item={display.item}
                    fetchedAt={fetchedAt}
                  />
                ),
              )}
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
                          <summary className="flex min-h-11 cursor-pointer flex-wrap items-center gap-2">
                            <Mono>#{row.seq}</Mono>
                            <span className="text-sm text-fg-subtle lg:text-xs">{row.ts}</span>
                            <Badge tone="neutral">{row.event.type}</Badge>
                          </summary>
                          <p className="mt-1.5 text-fg-muted">{row.event.msg}</p>
                        </details>
                      ) : (
                        <p className="flex flex-wrap items-center gap-2">
                          <Mono>#{row.seq}</Mono>
                          <span className="text-sm text-fg-subtle lg:text-xs">{row.ts}</span>
                          <Badge tone="neutral">{row.event.type}</Badge>
                        </p>
                      )}
                    </li>
                  ))}
                </ul>
              )}
              {events.has_more && <p className="text-sm text-fg-subtle lg:text-xs">続きがあります（has_more）。</p>}
            </div>
          </details>
        </CardBody>
      </Card>
    </section>
  );
}

/**
 * タイムラインの 1 件の安定した key（`kind` ごとに celeris が持つ一意の値を使う。添字は使わない:
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
    case "doc":
      return `doc-${item.path}`;
    case "integration":
      return `integration-${item.at}-${item.action}`;
    case "knowledge":
      return `knowledge-${item.run_task_id}`;
  }
}

/** タイムラインの 1 件（ADR-0044 D5）。`kind` ごとに出し分ける（知らない `kind` は無視する）。 */
function TimelineRow({ taskId, item, fetchedAt }: { taskId: string; item: TimelineItem; fetchedAt: string }) {
  return (
    <li
      data-testid="timeline-item"
      data-timeline-kind={item.kind}
      className="rounded-lg border border-border px-3 py-2 text-sm"
    >
      <p className="flex flex-wrap items-center gap-2">
        {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
            フェーズ 74（ADR-0055 D2 ラウンド 6）: 生の ISO のままだと 393px には長すぎるので、他画面
            （`/approvals` 等）と同じ `LocalTime`（`~/lib/reports.ts::relativeTimeLabel`）に揃え、
            絶対時刻は `title`/`dateTime` に残す。 */}
        <LocalTime iso={item.at} fetchedAtIso={fetchedAt} className="text-sm text-fg-subtle lg:text-xs" />
        <Badge tone={TIMELINE_TONE[item.kind] ?? "neutral"}>{timelineKindLabel(item.kind)}</Badge>
        {item.kind === "event" && <Mono>{item.event.type}</Mono>}
      </p>
      <TimelineBody taskId={taskId} item={item} />
    </li>
  );
}

/**
 * `worker_progress` イベントが連続した区間 1 つ（ADR-0048 D2、フェーズ 74）。既定は折り畳み
 * （`aria-expanded` を持つ `button`。44px のタップ領域）で、見出しは「件数 ・ 最後の kind ・ 最後の時刻」。
 * 開くと Console の `ReplyStepRow` と同じ行（`~/lib/task-timeline.ts::workerProgressStep` で
 * `ConsoleReplyStep` の形に写す）が並び、両画面の見た目が揃う。
 */
function TimelineProgressGroupRow({ items, fetchedAt }: { items: TimelineWorkerProgressItem[]; fetchedAt: string }) {
  const [open, setOpen] = useState(false);
  const last = items[items.length - 1];
  return (
    <li
      data-testid="timeline-progress-group"
      data-progress-count={items.length}
      className="rounded-lg border border-border px-3 py-2 text-sm"
    >
      {/* 通常の TimelineRow の見出し（時刻・kind バッジ・event type）とそろえる（ADR-0055 D1-3: 状態は
          1 語のバッジ + 色。ここでは `kind` = "event" のバッジ、`worker_progress` は他の event 行と
          同じ `Mono` 表示にする）。 */}
      <p className="flex flex-wrap items-center gap-2">
        <LocalTime iso={last.at} fetchedAtIso={fetchedAt} className="text-sm text-fg-subtle lg:text-xs" />
        <Badge tone={TIMELINE_TONE.event ?? "neutral"}>{timelineKindLabel("event")}</Badge>
        <Mono>worker_progress</Mono>
      </p>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        data-testid="timeline-progress-group-toggle"
        className="mt-1.5 flex min-h-11 w-full items-center gap-2 text-left"
      >
        <Icon name={open ? "chevronDown" : "chevronRight"} className="size-3.5 shrink-0 text-fg-subtle" />
        {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
        <span className="min-w-0 flex-1 text-sm text-fg-subtle lg:text-xs">{timelineProgressGroupSummary(items)}</span>
      </button>
      {open && (
        <div className="mt-2 space-y-1 border-t border-border pt-2" data-testid="timeline-progress-group-detail">
          {items.map((item) => (
            <ReplyStepRow key={item.seq} step={workerProgressStep(item.event)} />
          ))}
        </div>
      )}
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
          <Link to="/reports" className={cn(touchLinkClass, "font-medium text-primary hover:underline")}>
            {item.report.headline}
          </Link>
          {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
          <span className="ml-2 text-sm text-fg-subtle lg:text-xs">{item.report.kind}</span>
        </p>
      );
    case "delegation":
      return (
        <div className="mt-1.5 text-fg-muted">
          {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
          <p className="text-sm text-fg-subtle lg:text-xs">
            run{" "}
            <Link
              to={`/tasks/${taskId}/runs/${item.run_id}`}
              className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
            >
              {item.run_id}
            </Link>
          </p>
          <ul className="mt-1 space-y-0.5">
            {item.tasks.map((child) => (
              <li key={child.id}>
                <Link
                  to={`/tasks/${child.id}`}
                  className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
                >
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
          <Link to="/releases" className={cn(touchLinkClass, "font-mono font-medium text-primary hover:underline")}>
            {item.sha12}
          </Link>{" "}
          に入りました（コミット {item.commits.length} 件）。
        </p>
      );
    // ADR-0043 D5 / A2（取り込み）。`action` は merge / pr / discard、`detail` は 1 行の説明。
    case "integration":
      return (
        <p className="mt-1.5 text-fg-muted">
          {item.action}: {item.detail}
        </p>
      );
    // ADR-0044 D7（Phase 57 / G20）: 逆リンク。front matter の `tasks:` にこのタスクを持つページ。
    case "doc":
      return (
        <p className="mt-1.5 text-fg-muted" data-testid="timeline-doc">
          <Link
            to={docsHref(item.project_id, { path: item.path })}
            className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
          >
            {item.title}
          </Link>{" "}
          <Mono className="text-xs">{item.path}</Mono>
        </p>
      );
    // ADR-0047 D4/D5（Phase 62）: このタスクの終端から起きた知識整理 run。
    case "knowledge": {
      const total = (item.ingested ?? 0) + (item.inbox ?? 0) + (item.discarded ?? 0);
      return (
        <p className="mt-1.5 text-fg-muted" data-testid="timeline-knowledge">
          {item.state === "scheduled" ? (
            "知識整理 run を起こしました（まだ適用されていません）。"
          ) : item.state === "failed" ? (
            "知識整理 run が失敗しました（候補はありません）。"
          ) : (
            <>
              知識 {total} 件: 取り込み {item.ingested ?? 0} / 候補 {item.inbox ?? 0} / 破棄 {item.discarded ?? 0}
              {(item.inbox ?? 0) > 0 && (
                <>
                  {" "}
                  <Link
                    to="/knowledge/inbox"
                    className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
                  >
                    知識の候補を見る
                  </Link>
                </>
              )}
            </>
          )}
          {/* ADR-0052 D2（Phase 64）: Qwen に届かず tier cheap の汎用ハーネスで抽出した run。 */}
          {isKnowledgeFallback(item.via) && (
            <span className="ml-1 text-fg-subtle" data-testid="timeline-knowledge-fallback">
              （cheap のハーネスで抽出）
            </span>
          )}
        </p>
      );
    }
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
    case "worker_finished": {
      // `done: <長い要約>` はステータス名だけ本文に出し、要約は折り畳みに分ける。
      const { status, text } = splitOutcome(event.outcome);
      return (
        <div className="mt-1.5 text-fg-muted">
          <p>
            run {event.run_id} 終了: {status}
          </p>
          {text && (
            <details data-testid="timeline-outcome-detail" className="mt-1 text-sm lg:text-xs">
              <summary className="cursor-pointer select-none">詳細</summary>
              <p className="mt-1 max-h-48 overflow-y-auto whitespace-pre-wrap break-words">{text}</p>
            </details>
          )}
        </div>
      );
    }
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
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="text-sm font-medium text-fg-subtle lg:text-xs">
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
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="text-sm font-semibold uppercase tracking-wide text-fg-subtle lg:text-xs">{label}</p>
      {refs.length === 0 ? (
        <p className="mt-1 text-sm text-fg-subtle">ありません。</p>
      ) : (
        <ul className="mt-1.5 divide-y divide-border overflow-hidden rounded-lg border border-border">
          {refs.map((ref) => (
            <li key={ref.id} className="px-3 py-2 text-sm">
              <Link to={`/tasks/${ref.id}`} className={cn(touchLinkClass, "font-medium text-primary hover:underline")}>
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
 * `/files/tasks/:id/artifacts/:idx` を fetch し、celeris が返した実際の `Content-Type` でビューアを選ぶ
 * （拡張子からの推測はしない。celeris の値をそのまま使う）。403（`forbidden`）は一覧の `ArtifactView.forbidden`
 * だけで判定し、本体を取りに行かない。
 */
function ArtifactRow({
  taskId,
  artifact,
  projectId,
  taskTitle,
  category,
}: {
  taskId: string;
  artifact: ArtifactView;
  /** ADR-0044 D7: 昇格の宛先は案件の文書なので、案件に属さないタスクでは出さない。 */
  projectId: string | null;
  taskTitle: string;
  category: string | null;
}) {
  const [open, setOpen] = useState(false);
  const [promoting, setPromoting] = useState(false);
  const [body, setBody] = useState<{ contentType: string; content?: string } | null>(null);
  const href = `/files/tasks/${taskId}/artifacts/${artifact.idx}`;
  const canOpen = artifact.exists && !artifact.forbidden;
  const statusMessage = artifactStatusMessage(artifact);
  // ADR-0044 D7: 昇格できるのは Markdown の成果物だけ（判定は名前だけ。中身は celeris が読む）。
  const canPromote = canOpen && projectId !== null && isMarkdownName(artifact.artifact.name);

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
          <p className="mt-0.5 text-sm text-fg-subtle lg:text-xs">
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
            {canPromote && (
              <button
                type="button"
                onClick={() => setPromoting((v) => !v)}
                data-testid="artifact-promote"
                className={buttonClass({ variant: "ghost", size: "xs" })}
              >
                <Icon name="book" />
                {PROMOTE_TO_DOC_LABEL}
              </button>
            )}
          </div>
        )}
      </div>
      {promoting && projectId && (
        <PromoteToDoc
          taskId={taskId}
          projectId={projectId}
          name={artifact.artifact.name}
          taskTitle={taskTitle}
          category={category}
          onClose={() => setPromoting(false)}
        />
      )}
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
 * 成果物を案件の文書に昇格する（ADR-0044 D7、docs/celeris-api-v1.md §3.97。**管理系**。Phase 57 / G20）。
 * 宛先のパスは人が決める（既定は `docs/<種類>/<題名の slug>.md`）。宛先が既にあれば celeris が
 * 409 `page_exists` を返すので、そのときだけ「上書きする」を選び直す（GUI では判定しない）。
 */
function PromoteToDoc({
  taskId,
  projectId,
  name,
  taskTitle,
  category,
  onClose,
}: {
  taskId: string;
  projectId: string;
  name: string;
  taskTitle: string;
  category: string | null;
  onClose: () => void;
}) {
  const fetcher = useFetcher<DocsOpOutcome>({ key: `promote-${taskId}-${name}` });
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : null;
  return (
    <div className="mt-3 rounded-lg border border-border bg-surface-2/50 p-3" data-testid="artifact-promote-form">
      <fetcher.Form method="post" action={`/tasks/${taskId}`} className="space-y-2">
        <input type="hidden" name="intent" value="promote" />
        <input type="hidden" name="name" value={name} />
        <label className={labelClass} htmlFor={`promote-path-${name}`}>
          文書の置き場（案件の文書の根からの相対パス。`.md`）
        </label>
        <input
          id={`promote-path-${name}`}
          name="path"
          className={inputClass}
          defaultValue={defaultPromotePath("docs", category, taskTitle, name)}
          data-testid="artifact-promote-path"
        />
        <label className={labelClass} htmlFor={`promote-title-${name}`}>
          題名（任意。省略すると中身の 1 行目）
        </label>
        <input id={`promote-title-${name}`} name="title" className={inputClass} defaultValue={taskTitle} />
        <label className={chipLabelClass}>
          <input type="checkbox" name="overwrite" value="1" className={checkboxClass} />
          {PROMOTE_OVERWRITE_LABEL}
        </label>
        <div className="flex items-center gap-2">
          <button
            type="submit"
            disabled={submitting}
            className={buttonClass({ variant: "primary", size: "xs" })}
            data-testid="artifact-promote-submit"
          >
            {PROMOTE_TO_DOC_SUBMIT_LABEL}
          </button>
          <button type="button" onClick={onClose} className={buttonClass({ variant: "ghost", size: "xs" })}>
            やめる
          </button>
        </div>
      </fetcher.Form>
      {error && (
        <div data-testid="artifact-promote-error">
          <ErrorFlash error={error} />
          {docsErrorHint(error.code) && <p className="text-sm text-fg-muted lg:text-xs">{docsErrorHint(error.code)}</p>}
        </div>
      )}
      {fetcher.data?.ok && fetcher.data.op === "docs_promote" && (
        <p className="mt-2 text-sm" data-testid="artifact-promote-done">
          <Link
            to={docsHref(projectId, { path: fetcher.data.result.path })}
            className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
          >
            {fetcher.data.result.path}
          </Link>{" "}
          に昇格しました。
        </p>
      )}
    </div>
  );
}

/**
 * loader が `celerisErrorResponse` で投げた `Response` を `isRouteErrorResponse` で判別する
 * （docs/adr/0004-g1-decisions.md D6）。celeris 停止中はこのルート自身が root と同じバナーを出し
 * （200 にはならないが 500 でもない。§6.5「500 にしない」）、404 は「タスクが見つかりません」にする。
 */
export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as CelerisRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={data.baseUrl ?? ""} problem={null} />
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
