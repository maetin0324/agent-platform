import { type FormEvent, useEffect, useMemo, useState } from "react";
import { data, isRouteErrorResponse, Link, useFetcher, useNavigate } from "react-router";
import { ArtifactsList } from "~/components/ArtifactsList";
import { ErrorFlash, FieldErrors, ProjectActionFlash, RetryFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { ProjectIntegrations } from "~/components/ProjectIntegrations";
import { ProjectRepos } from "~/components/ProjectRepos";
import { ReportsList } from "~/components/ReportsList";
import { Badge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { checkboxClass, hintClass, inputClass, labelClass, selectClass, textareaClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, EmptyState, PageHeader, SectionTitle } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { WorkspaceFields } from "~/components/WorkspaceFields";
import { WorkTree } from "~/components/WorkTree";
import {
  buildProjectArtifactRows,
  type ProjectArtifactRow,
  type TaskArtifactBundle,
  workspacePlace,
} from "~/lib/artifacts";
import { PRIORITY_LABELS } from "~/lib/board";
import {
  milestoneStatusLabel,
  priorityFullLabel,
  projectStatusLabel,
  TASK_CATEGORIES,
  TIERS,
  taskCategoryLabel,
  taskStatusLabel,
  tierLabel,
} from "~/lib/labels";
import { milestoneDecisionValid, milestoneIsStalled } from "~/lib/milestone-review";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { projectTasksToGraph, visibleWorkTasks } from "~/lib/work-tree";
import { readWorkspaceFromForm, workspaceKindOf, workspaceSummaryText } from "~/lib/workspace-form";
import { TaskdBanner } from "~/root";
import type { ProjectOpOutcome, RetryOutcome, TransitionOutcome } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { formString } from "~/taskd/forms";
import {
  createMilestone,
  decideMilestone,
  patchMilestoneStatus,
  patchProjectStatus,
  patchProjectWorkspace,
  startProjectPlan,
} from "~/taskd/projects-admin.server";
import {
  createRepo,
  deleteRepo,
  patchRepo,
  readRepoCreateBody,
  readRepoPatchBody,
  setPrimaryRepo,
} from "~/taskd/repos-admin.server";
import { createTask } from "~/taskd/route-actions.server";
import { buildProjectTaskSpec } from "~/taskd/tasks-admin.server";
import type {
  ArtifactList,
  Clusters,
  ClusterView,
  MilestoneDecideBody,
  MilestoneStatus,
  MilestoneView,
  OrgList,
  OrgNode,
  ProjectDetail,
  ProjectIntegrationItem,
  ProjectIntegrations as ProjectIntegrationsView,
  ProjectStatus,
  ProjectTaskView,
  ReportList,
  TaskDetail,
  TaskId,
} from "~/taskd/types";
import type { Route } from "./+types/projects.$id";

/**
 * `/projects/:id`（案件の詳細・途中目標・仕事の木・報告・成果物、SPEC §3.3・§3.5・§3.7、ADR-0033 D2/D3、
 * docs/gui/api.md §3.47〜3.51、Phase G13c）。
 * 「仕事の木」は `GET /projects/{id}` の `tasks`（`ProjectTaskView`、`parent_id` / `depends_on` は既存の DAG
 * と同じ辺の作り方）を `/graph` と同じ `layoutGraph`（`~/components/WorkTree.tsx`）で描く。
 * 各ノードには `assignee` の組織ノードの名前を出す（`GET /org` と突き合わせる。組織のノード名を出すだけで、
 * taskd 側の判断値は増やさない）。
 * 「報告」タブは `GET /reports?project=<id>`（**全レベル**。`level` を付けない。`/reports` の既定は秘書
 * レベルの未読だけだが、案件詳細ではこの案件のすべての段の報告を見せる。G13b-1 の依頼どおり）を
 * `/reports` と同じ `ReportsList` で出す。
 * 「成果物」節は `~/routes/artifacts.tsx`（横断一覧）と同じ組み立て（`~/lib/artifacts.ts::buildProjectArtifactRows`。
 * taskd への問い合わせ自体は各 loader に閉じる私的ヘルパー。下記コメント参照）で、この案件のタスクぶんだけを
 * `~/components/ArtifactsList.tsx` で出す（G13b-1 の「報告」タブと同じ作り）。
 */

export interface ProjectDetailData {
  detail: ProjectDetail;
  org: OrgList;
  reports: ReportList;
  artifactRows: ProjectArtifactRow[];
  fetchedAt: string;
  /** 作業場所の編集フォームの選択肢（`GET /clusters`。ADR-0039 D1、Phase G13k）。taskd に届かないときは空。 */
  clusters: ClusterView[];
  /**
   * 「PR と取り込み」節（`GET /projects/{id}/integrations`。ADR-0043 D5、Phase 54 / G18）。
   * タスク × リポジトリごとに最新の 1 件を taskd が新しい順で返す。落ちても案件の詳細自体は出す。
   */
  integrations: ProjectIntegrationItem[];
}

/**
 * 1 タスクぶんの成果物 + 置き場所を束ねる（N+1。`GET /tasks/{id}` と `GET /tasks/{id}/artifacts`）。
 * `~/routes/artifacts.tsx` に同じ形の私的ヘルパーがある（React Router のクライアントバンドル除去は
 * `loader`/`action` 等に限られるため、公開関数から `.server.ts` モジュールを参照しない。重複はこの小ささでは許容する）。
 */
async function loadTaskArtifactBundles(
  client: TaskdClient,
  taskIds: readonly TaskId[],
  signal: AbortSignal | undefined,
): Promise<Map<TaskId, TaskArtifactBundle>> {
  const entries = await Promise.all(
    taskIds.map(async (id) => {
      const [detail, list] = await Promise.all([
        client.get<TaskDetail>(`/tasks/${encodeURIComponent(id)}`, { signal }).catch(() => null as TaskDetail | null),
        client
          .get<ArtifactList>(`/tasks/${encodeURIComponent(id)}/artifacts`, { signal })
          .catch(() => ({ items: [] }) as ArtifactList),
      ]);
      const bundle: TaskArtifactBundle = {
        workspace: detail
          ? workspacePlace(detail.task.workspace, detail.workspace_dir)
          : { text: "-", vscodeHref: null, localCopyNote: null },
        artifacts: list.items,
      };
      return [id, bundle] as const;
    }),
  );
  return new Map(entries);
}

export async function loadProjectDetail(client: TaskdClient, id: string, request: Request): Promise<ProjectDetailData> {
  const [detail, org, reports, clusters, integrations] = await Promise.all([
    client.get<ProjectDetail>(`/projects/${encodeURIComponent(id)}`, { signal: request.signal }),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
    client
      .get<ReportList>("/reports", { query: { project: id }, signal: request.signal })
      .catch(() => ({ items: [] }) as ReportList),
    // 作業場所の編集フォームの選択肢（ADR-0039 D1、Phase G13k）。落ちても案件の詳細自体は出す。
    client.get<Clusters>("/clusters", { signal: request.signal }).catch(() => ({ items: [] }) as Clusters),
    // 取り込みの記録（ADR-0043 D5、Phase 54 / G18）。落ちても案件の詳細自体は出す。
    client
      .get<ProjectIntegrationsView>(`/projects/${encodeURIComponent(id)}/integrations`, { signal: request.signal })
      .catch(() => ({ items: [] }) as ProjectIntegrationsView),
  ]);
  const orgById = new Map(org.items.map((n) => [n.id, n]));
  const bundles = await loadTaskArtifactBundles(
    client,
    detail.tasks.map((t) => t.id),
    request.signal,
  );
  const artifactRows = buildProjectArtifactRows(detail.tasks, bundles, orgById);
  return {
    detail,
    org,
    reports,
    artifactRows,
    fetchedAt: new Date().toISOString(),
    clusters: clusters.items,
    integrations: integrations.items,
  };
}

export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ params, request }: Route.LoaderArgs): Promise<ProjectDetailData> {
  try {
    return await loadProjectDetail(getTaskdClient(), params.id, request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "案件詳細 - Celeris" }];
}

export async function action({ request, params }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getTaskdClient();

  let outcome: ProjectOpOutcome;
  switch (intent) {
    case "project_status":
      outcome = await patchProjectStatus(
        client,
        params.id,
        (formString(form, "status") ?? "proposed") as ProjectStatus,
        request.signal,
      );
      break;
    case "milestone_create":
      outcome = await createMilestone(client, params.id, form, request.signal);
      break;
    case "project_plan":
      outcome = await startProjectPlan(client, params.id, form, request.signal);
      break;
    case "milestone_status":
      outcome = await patchMilestoneStatus(
        client,
        formString(form, "milestone_id") ?? "",
        (formString(form, "status") ?? "proposed") as MilestoneStatus,
        request.signal,
      );
      break;
    case "milestone_decide":
      outcome = await decideMilestone(client, formString(form, "milestone_id") ?? "", form, request.signal);
      break;
    // 作業場所の保存・消去（ADR-0039 D1、Phase G13k）。保存は選んだ kind（local/remote）をそのまま送り、
    // 消去は明示的に `workspace: null` を送る（別ボタン。編集フォームで「まだ決めない」は選べない）。
    case "project_workspace_save":
      outcome = await patchProjectWorkspace(client, params.id, readWorkspaceFromForm(form), request.signal);
      break;
    case "project_workspace_clear":
      outcome = await patchProjectWorkspace(client, params.id, null, request.signal);
      break;
    // 案件のリポジトリ（ADR-0043 D1、docs/taskd-api-v1.md §3.69〜3.71。Phase 52 / G16）。
    // どれもフォームの値を対応する要求に写すだけで、GUI 側では検証しない（409 / 422 は taskd の文言）。
    case "repo_create":
      outcome = await createRepo(client, params.id, readRepoCreateBody(form), request.signal);
      break;
    case "repo_patch":
      outcome = await patchRepo(client, formString(form, "repo_id") ?? "", readRepoPatchBody(form), request.signal);
      break;
    case "repo_primary":
      outcome = await setPrimaryRepo(client, formString(form, "repo_id") ?? "", request.signal);
      break;
    case "repo_delete":
      outcome = await deleteRepo(client, formString(form, "repo_id") ?? "", request.signal);
      break;
    // ADR-0044 D1（Phase 53）: 案件・途中目標から人がタスクを足す。人が作ったタスクは `ready`
    // （`POST /tasks` の既定。`status` は送らない）。
    case "task_create": {
      const created = await createTask(client, buildProjectTaskSpec(form, params.id), request.signal);
      outcome = created.ok ? { ok: true, op: "task_create", task: created.task } : { ...created, op: "task_create" };
      break;
    }
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

const PROJECT_STATUSES: ProjectStatus[] = ["proposed", "active", "paused", "done"];
const MILESTONE_STATUSES: MilestoneStatus[] = ["proposed", "approved", "in_progress", "reached", "redesigned"];

const PROJECT_STATUS_TONE: Record<ProjectStatus, Tone> = {
  proposed: "info",
  active: "primary",
  paused: "warning",
  done: "success",
};

const MILESTONE_STATUS_TONE: Record<MilestoneStatus, Tone> = {
  proposed: "info",
  approved: "primary",
  in_progress: "warning",
  reached: "success",
  redesigned: "teal",
};

export default function ProjectDetailPage({ loaderData }: Route.ComponentProps) {
  const { detail, org, reports, artifactRows, fetchedAt, clusters, integrations } = loaderData;
  const { project, milestones, tasks } = detail;
  // ADR-0043 D1（Phase 52 / G16）: 並びは taskd が決めたもの（primary が先頭）をそのまま使う。
  const repos = detail.repos ?? [];
  const fetcher = useFetcher<ProjectOpOutcome>();
  const submitting = fetcher.state !== "idle";
  const workspaceError = fetcher.data && !fetcher.data.ok ? fetcher.data.error : undefined;

  const orgById = useMemo(() => new Map(org.items.map((n) => [n.id, n])), [org.items]);
  // 裏方のタスク（`support`: 対話・報告のまとめ・承認待ち・レビュー。Phase 29）は仕事の木から完全に外す
  // （SPEC「タスクは裏方」/ ADR-0033 D8）。件数表示・「担当に話す」一覧（下の `work-tree-assignees`）も
  // 同じ判断に揃えるため、ここで 1 度だけ絞る。
  const workTasks = useMemo(() => visibleWorkTasks(tasks), [tasks]);
  const graph = useMemo(() => projectTasksToGraph(tasks, orgById), [tasks, orgById]);

  return (
    <div className="space-y-8">
      <PageHeader
        icon="folder"
        title={
          <>
            {project.title}
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="案件は組織の上から入り、分解されて下へ流れます。その依存関係が「仕事の木」です。"
        actions={
          <Badge tone={PROJECT_STATUS_TONE[project.status]} data-testid="project-status">
            {projectStatusLabel(project.status)}
          </Badge>
        }
      />

      <ProjectActionFlash outcome={fetcher.data} />

      <section aria-labelledby="project-detail-heading" className="space-y-4">
        <SectionTitle icon="file" id="project-detail-heading">
          依頼
        </SectionTitle>
        <Card>
          <CardBody className="space-y-4">
            <DataItem label="依頼文" wide>
              <p className="whitespace-pre-wrap" data-testid="project-request-text">
                {project.request}
              </p>
            </DataItem>
            {project.secretary_summary && (
              <Alert tone="info" title="秘書の理解の確認・方針" data-testid="project-secretary-summary">
                <p className="whitespace-pre-wrap">{project.secretary_summary}</p>
              </Alert>
            )}
            <fetcher.Form method="post" className="flex flex-wrap items-end gap-3" data-testid="project-status-form">
              <input type="hidden" name="intent" value="project_status" />
              <div>
                <label htmlFor="project-status-select" className={labelClass}>
                  案件の状態
                </label>
                <select
                  id="project-status-select"
                  name="status"
                  defaultValue={project.status}
                  className={`${selectClass} mt-1.5`}
                >
                  {PROJECT_STATUSES.map((s) => (
                    <option key={s} value={s}>
                      {projectStatusLabel(s)}
                    </option>
                  ))}
                </select>
              </div>
              <Button
                type="submit"
                variant="secondary"
                size="sm"
                disabled={submitting}
                data-testid="project-status-submit"
              >
                <Icon name="check" />
                状態を変える
              </Button>
            </fetcher.Form>
          </CardBody>
        </Card>
      </section>

      {/* 案件の作業場所（ADR-0039 D1、Phase G13k）。コードを扱う仕事の子タスクが継ぐ場所
          （明示 > 案件 > 親。ADR-0039 D2）で、未設定だと空の作業ディレクトリで走ってしまう
          （実機の事故 2026-09-18）。 */}
      <section aria-labelledby="project-workspace-heading" data-testid="project-workspace" className="space-y-4">
        <SectionTitle icon="folder" id="project-workspace-heading">
          作業場所
        </SectionTitle>
        <Card>
          <CardBody className="space-y-4">
            {project.workspace ? (
              <DataItem label="現在" wide>
                <p className="break-all font-mono text-sm">{workspaceSummaryText(project.workspace)}</p>
              </DataItem>
            ) : (
              <Alert tone="warning" data-testid="project-workspace-unset">
                未設定 — コードを扱う仕事は空の作業ディレクトリで走ります
              </Alert>
            )}
            <fetcher.Form method="post" className="space-y-3" data-testid="project-workspace-form">
              <input type="hidden" name="intent" value="project_workspace_save" />
              <WorkspaceFields
                idPrefix="project-workspace"
                clusters={clusters}
                allowUndecided={false}
                defaultKind={project.workspace ? workspaceKindOf(project.workspace) : "local"}
                defaultPath={project.workspace?.path ?? ""}
                defaultCluster={project.workspace?.kind === "remote" ? project.workspace.cluster : undefined}
                error={workspaceError}
              />
              <div className="flex flex-wrap gap-2">
                <Button
                  type="submit"
                  variant="secondary"
                  size="sm"
                  disabled={submitting}
                  data-testid="project-workspace-save"
                >
                  <Icon name="check" />
                  保存
                </Button>
              </div>
            </fetcher.Form>
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="project_workspace_clear" />
              <Button
                type="submit"
                variant="ghost"
                size="sm"
                disabled={submitting}
                data-testid="project-workspace-clear"
              >
                <Icon name="x" />
                消去
              </Button>
            </fetcher.Form>
          </CardBody>
        </Card>
      </section>

      {/* ADR-0043 D1（Phase 52 / G16）: 案件は「リポジトリ」を複数持つ（論文とコード、git ではない
          データの置き場）。`is_primary` の 1 件が上の「作業場所」と同じものを指す。並び・primary の
          付け替え・削除できるかどうかは taskd が決めるので、ここは表示と中継だけ。 */}
      <section aria-labelledby="project-repos-heading" data-testid="project-repos-section" className="space-y-4">
        <SectionTitle icon="database" id="project-repos-heading" count={repos.length}>
          リポジトリ
        </SectionTitle>
        <ProjectRepos projectId={project.id} repos={repos} clusters={clusters} />
      </section>

      {/* PR と取り込み（ADR-0043 D5、Phase 54 / G18）。タスク × リポジトリごとに最新の 1 件を
          taskd が新しい順で返すので、並べ替えも集計もしない。操作はタスクの「変更」で行う。 */}
      <section
        aria-labelledby="project-integrations-heading"
        data-testid="project-integrations-section"
        className="space-y-4"
      >
        <SectionTitle icon="gitBranch" id="project-integrations-heading" count={integrations.length}>
          PR と取り込み
        </SectionTitle>
        <ProjectIntegrations items={integrations} />
      </section>

      <section aria-labelledby="milestones-heading" data-testid="milestones-section" className="space-y-4">
        <SectionTitle icon="target" id="milestones-heading" count={milestones.length}>
          途中目標
        </SectionTitle>
        {milestones.length === 0 ? (
          <EmptyState icon="target" title="途中目標がありません" />
        ) : (
          <ul className="space-y-2">
            {milestones
              .slice()
              .sort((a, b) => a.seq - b.seq)
              .map((m) => (
                <li key={m.id} data-testid="milestone-row" data-milestone-id={m.id}>
                  <Card>
                    <CardBody className="space-y-3">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-mono text-xs text-fg-subtle">#{m.seq}</span>
                        <span className="font-medium">{m.title}</span>
                        <Badge tone={MILESTONE_STATUS_TONE[m.status]} data-testid="milestone-status">
                          {milestoneStatusLabel(m.status)}
                        </Badge>
                      </div>
                      {m.description && <p className="text-sm text-fg-muted">{m.description}</p>}

                      {/* ADR-0038 D3（Phase 41 / G13j）: 秘書のレビューの返事が付いたら、まとめ・提案・
                          ok / 議論 / ng のカードを出す。`reached` / `redesigned` は判定済みなので
                          バッジだけ（`review` が付いたままでもボタンは出さない。誤って ok/議論/ng を
                          もう一度押せてしまわないように）。 */}
                      {m.status === "reached" || m.status === "redesigned" ? null : m.review ? (
                        <MilestoneReviewPanel milestone={m} projectId={project.id} />
                      ) : (
                        milestoneIsStalled(tasks, m.id) && (
                          <Alert tone="info" data-testid="milestone-review-pending">
                            秘書が結果をまとめています…
                          </Alert>
                        )
                      )}

                      {/* ADR-0044 D1（Phase 53）: この途中目標に属するタスクを人が直接足せる。 */}
                      <AddTaskForm
                        projectId={project.id}
                        milestoneId={m.id}
                        org={org.items}
                        testId={`milestone-add-task-${m.id}`}
                      />

                      {/* 既存の直接変更は裏方の詳細に畳む（誤って押さないように。SPEC §7 のアジャイル判定は
                          本来「ok / 議論 / ng」の対話で行う。ADR-0038 D3 の依頼）。 */}
                      <details className="text-xs text-fg-subtle" data-testid="milestone-status-details">
                        <summary className="cursor-pointer select-none">状態を直接変える（裏方）</summary>
                        <fetcher.Form method="post" className="mt-2 flex flex-wrap items-end gap-2">
                          <input type="hidden" name="intent" value="milestone_status" />
                          <input type="hidden" name="milestone_id" value={m.id} />
                          <select
                            name="status"
                            defaultValue={m.status}
                            aria-label={`途中目標 ${m.title} の状態`}
                            className={`${selectClass} h-8 text-xs`}
                          >
                            {MILESTONE_STATUSES.map((s) => (
                              <option key={s} value={s}>
                                {milestoneStatusLabel(s)}
                              </option>
                            ))}
                          </select>
                          <Button
                            type="submit"
                            variant="ghost"
                            size="xs"
                            disabled={submitting}
                            data-testid="milestone-status-submit"
                          >
                            <Icon name="check" />
                            Go / 再設計
                          </Button>
                        </fetcher.Form>
                      </details>
                    </CardBody>
                  </Card>
                </li>
              ))}
          </ul>
        )}

        <Card>
          <CardHeader
            icon="plus"
            title="途中目標を足す"
            description="途中目標は予め大まかに決めておいて、達成のたびに Go を出すか、再設計します。"
          />
          <CardBody>
            <fetcher.Form method="post" data-testid="milestone-new-form" className="space-y-3">
              <input type="hidden" name="intent" value="milestone_create" />
              <div>
                <label htmlFor="milestone-title" className={labelClass}>
                  題名
                </label>
                <input id="milestone-title" name="title" type="text" className={`${inputClass} mt-1.5 w-full`} />
              </div>
              <div>
                <label htmlFor="milestone-description" className={labelClass}>
                  説明
                </label>
                <textarea
                  id="milestone-description"
                  name="description"
                  rows={2}
                  className={`${textareaClass} mt-1.5 w-full`}
                />
              </div>
              <Button
                type="submit"
                variant="primary"
                size="sm"
                disabled={submitting}
                data-testid="milestone-new-submit"
              >
                <Icon name="plus" />
                追加
              </Button>
            </fetcher.Form>
          </CardBody>
        </Card>
      </section>

      {/* 「この方針で進める」（監査 H3、docs/taskd-api-v1.md §3.61）。押すと秘書に分解の仕事が 1 件立ち、
          仕事の木が増えていく。案件が「提案中」でも押せる（taskd が「進行中」にする）。 */}
      <section aria-labelledby="project-plan-heading" className="space-y-4">
        <SectionTitle icon="sparkles" id="project-plan-heading">
          この方針で進める
        </SectionTitle>
        <Card>
          <CardHeader
            icon="sparkles"
            title="分解を秘書に頼む"
            description="秘書が、依頼文・途中目標・ここまでのやり取りとあなたの一言をまとめて、仕事に分解します。返事は待ちません（仕事の木が増えていきます）。"
          />
          <CardBody>
            <fetcher.Form method="post" data-testid="project-plan-form" className="space-y-3">
              <input type="hidden" name="intent" value="project_plan" />
              <div>
                <label htmlFor="project-plan-milestone" className={labelClass}>
                  どの途中目標まで進めるか
                </label>
                <select
                  id="project-plan-milestone"
                  name="milestone_id"
                  data-testid="project-plan-milestone"
                  defaultValue=""
                  className={`${selectClass} mt-1.5 w-full max-w-md`}
                >
                  <option value="">指定しない（今ある途中目標を文脈として渡す）</option>
                  {milestones
                    .filter((m) => m.status === "approved" || m.status === "in_progress")
                    .sort((a, b) => a.seq - b.seq)
                    .map((m) => (
                      <option key={m.id} value={m.id}>
                        #{m.seq} {m.title}
                      </option>
                    ))}
                </select>
                <p className={hintClass}>
                  選べるのは承認済み・進行中の途中目標だけです（提案のままのものは出ません）。
                </p>
              </div>
              <div>
                <label htmlFor="project-plan-note" className={labelClass}>
                  ひとこと（任意）
                </label>
                <textarea
                  id="project-plan-note"
                  name="note"
                  rows={2}
                  data-testid="project-plan-note"
                  placeholder="例: 急がなくてよい。まず関連研究から。"
                  className={`${textareaClass} mt-1.5 w-full`}
                />
              </div>
              <Button type="submit" variant="primary" size="sm" disabled={submitting} data-testid="project-plan-submit">
                <Icon name="sparkles" />
                この方針で進める
              </Button>
            </fetcher.Form>
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="work-tree-heading" className="space-y-4">
        <SectionTitle icon="gitBranch" id="work-tree-heading" count={workTasks.length}>
          仕事の木
        </SectionTitle>
        {/* ADR-0044 D1（Phase 53）: 秘書に分解を頼むだけでなく、人が直接タスクを足せる。
            ボード（`/board?project=…`）でこの案件のタスクを並べて見られる。 */}
        <div className="flex flex-wrap items-center gap-3">
          <Link
            to={`/board?project=${encodeURIComponent(project.id)}`}
            data-testid="project-board-link"
            className={buttonClass({ variant: "secondary", size: "sm" })}
          >
            <Icon name="layers" />
            ボードで見る
          </Link>
        </div>
        <AddTaskForm projectId={project.id} org={org.items} testId="project-add-task" />
        <p className="text-xs text-fg-subtle">
          パッと見て、おかしな方針を立てていないかを確かめるための図です。四角を押すと裏方のタスクへ移ります。
          対話の返事や報告のまとめといった裏方の作業は出しません。
        </p>
        {workTasks.length === 0 ? (
          <EmptyState icon="gitBranch" title="この案件のタスクはまだありません" />
        ) : (
          <>
            <WorkTree graph={graph} />
            {/* SPEC §3.4「おかしなことをしていたら、誰に言うかを決めてその担当に直接言う」。
                木のノード（タスク）の担当へ、この案件を選んだ状態で話しかける導線（Phase G13b-2）。
                Phase 31: draft には「Go」（accept）、failed/cancelled には「やり直す」（retry）も
                ここから直接できる（担当がいないタスクも拾えるよう、絞り込みは assignee 限定をやめた）。 */}
            <ul className="space-y-1" data-testid="work-tree-assignees">
              {workTasks
                .filter((t) => t.assignee || t.status === "draft" || t.status === "failed" || t.status === "cancelled")
                .map((t) => (
                  <WorkTreeTaskRow
                    key={t.id}
                    task={t}
                    projectId={project.id}
                    orgName={orgById.get(t.assignee ?? "")?.name}
                  />
                ))}
            </ul>
          </>
        )}
      </section>

      <section aria-labelledby="project-reports-heading" className="space-y-4">
        <SectionTitle icon="send" id="project-reports-heading" count={reports.items.length}>
          報告
        </SectionTitle>
        <p className="text-xs text-fg-subtle">
          この案件について、各段から上がってきた報告です。全体の未読（秘書まで上がったもの）は
          <Link to="/reports" className="mx-1 underline underline-offset-2">
            報告
          </Link>
          の画面で流し見できます。
        </p>
        {reports.items.length === 0 ? (
          <EmptyState icon="send" title="この案件の報告はまだありません" />
        ) : (
          <ReportsList items={reports.items} projects={[project]} org={org.items} fetchedAt={fetchedAt} />
        )}
      </section>

      <section aria-labelledby="project-artifacts-heading" data-testid="artifacts-section" className="space-y-4">
        <SectionTitle icon="file" id="project-artifacts-heading" count={artifactRows.length}>
          成果物
        </SectionTitle>
        <p className="text-xs text-fg-subtle">
          調査結果の文書とリンク集はここで読めます。コードは置き場所（普段のパス）へのリンクで示します。
        </p>
        {artifactRows.length === 0 ? (
          <EmptyState icon="file" title="この案件の成果物はまだありません" />
        ) : (
          <ArtifactsList rows={artifactRows} fetchedAt={fetchedAt} />
        )}
      </section>
    </div>
  );
}

/**
 * 「タスクを追加」（ADR-0044 D1、Phase 53）。案件のヘッダと途中目標のカードの両方から同じ形で開く
 * （`milestoneId` を渡すとその途中目標に属するタスクになる）。
 *
 * **人が作ったタスクは待機中（ready）で始まる**（`POST /tasks` の既定。人は Go を出す側なので
 * draft を挟まない）。検証は taskd（題名・目的・受け入れ条件が空なら 422）に任せ、その文言をそのまま出す。
 */
function AddTaskForm({
  projectId,
  milestoneId,
  org,
  testId,
}: {
  projectId: string;
  milestoneId?: string;
  org: OrgNode[];
  testId: string;
}) {
  const fetcher = useFetcher<ProjectOpOutcome>({ key: `task-create-${milestoneId ?? projectId}` });
  const busy = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : undefined;

  return (
    <details data-testid={testId} className="rounded-lg border border-border bg-surface-2/40 p-3">
      <summary className="cursor-pointer select-none text-sm font-medium text-fg">タスクを追加</summary>
      <div className="mt-3 space-y-3">
        <ProjectActionFlash outcome={fetcher.data} />
        <fetcher.Form method="post" className="space-y-3" data-testid={`${testId}-form`}>
          <input type="hidden" name="intent" value="task_create" />
          {milestoneId && <input type="hidden" name="milestone_id" value={milestoneId} />}
          <div>
            <label htmlFor={`${testId}-title`} className={labelClass}>
              題名
            </label>
            <input
              id={`${testId}-title`}
              name="title"
              type="text"
              data-testid={`${testId}-title`}
              className={`${inputClass} mt-1.5 w-full`}
            />
            <FieldErrors error={error} field="title" />
          </div>
          <div>
            <label htmlFor={`${testId}-objective`} className={labelClass}>
              目的
            </label>
            <textarea
              id={`${testId}-objective`}
              name="objective"
              rows={3}
              data-testid={`${testId}-objective`}
              className={`${textareaClass} mt-1.5 w-full`}
            />
            <FieldErrors error={error} field="objective" />
          </div>
          <div>
            <label htmlFor={`${testId}-acceptance`} className={labelClass}>
              終わったと言える条件
            </label>
            <input
              id={`${testId}-acceptance`}
              name="acceptance"
              type="text"
              placeholder="例: 候補が 3 件以上まとまっている"
              data-testid={`${testId}-acceptance`}
              className={`${inputClass} mt-1.5 w-full`}
            />
            <FieldErrors error={error} field="acceptance" />
          </div>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <div>
              <label htmlFor={`${testId}-assignee`} className={labelClass}>
                担当
              </label>
              <select
                id={`${testId}-assignee`}
                name="assignee"
                defaultValue=""
                data-testid={`${testId}-assignee`}
                className={`${selectClass} mt-1.5`}
              >
                <option value="">（taskd に任せる）</option>
                {org.map((node) => (
                  <option key={node.id} value={node.id}>
                    {node.name}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label htmlFor={`${testId}-tier`} className={labelClass}>
                レベル
              </label>
              <select
                id={`${testId}-tier`}
                name="tier"
                defaultValue=""
                data-testid={`${testId}-tier`}
                className={`${selectClass} mt-1.5`}
              >
                <option value="">（既定）</option>
                {TIERS.map((t) => (
                  <option key={t} value={t}>
                    {tierLabel(t)}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label htmlFor={`${testId}-priority`} className={labelClass}>
                優先度
              </label>
              <select
                id={`${testId}-priority`}
                name="priority"
                defaultValue="P2"
                data-testid={`${testId}-priority`}
                className={`${selectClass} mt-1.5`}
              >
                {PRIORITY_LABELS.map((p) => (
                  <option key={p} value={p}>
                    {priorityFullLabel(p)}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label htmlFor={`${testId}-category`} className={labelClass}>
                種類
              </label>
              <select
                id={`${testId}-category`}
                name="category"
                defaultValue="other"
                data-testid={`${testId}-category`}
                className={`${selectClass} mt-1.5`}
              >
                {TASK_CATEGORIES.map((c) => (
                  <option key={c} value={c}>
                    {taskCategoryLabel(c)}
                  </option>
                ))}
              </select>
            </div>
          </div>
          <Button type="submit" variant="primary" size="sm" disabled={busy} data-testid={`${testId}-submit`}>
            <Icon name="plus" />
            追加（待機中で始まります）
          </Button>
        </fetcher.Form>
      </div>
    </details>
  );
}

/**
 * 途中目標のレビューカード（ADR-0038 D3、Phase 41 / G13j）。秘書のまとめ（Markdown）と提案された次の
 * 途中目標を出し、「ok」「議論」「ng」の 3 ボタン＋自由記述欄を持つ。この画面全体の `fetcher`
 * （project 単位の intent）とは別に、途中目標ごとの専用 `fetcher`（`WorkTreeTaskRow` と同じ考え方）を持つ。
 * `discuss` が通ったら秘書の対話画面（`/org/secretary?project=<id>`）へ遷移して返事を待つ
 * （`~/lib/conversation.ts` の「考え中」の仕組みにそのまま乗る。`waiting=1` は新しい案件を作った直後と同じ扱い）。
 */
function MilestoneReviewPanel({ milestone, projectId }: { milestone: MilestoneView; projectId: string }) {
  const fetcher = useFetcher<ProjectOpOutcome>({ key: `milestone-decide-${milestone.id}` });
  const busy = fetcher.state !== "idle";
  const [note, setNote] = useState("");
  const [invalid, setInvalid] = useState(false);
  const navigate = useNavigate();
  const review = milestone.review;

  useEffect(() => {
    if (fetcher.data?.ok && fetcher.data.op === "milestone_decide" && fetcher.data.decided.decision === "discuss") {
      navigate(`/org/secretary?project=${encodeURIComponent(projectId)}&waiting=1`);
    }
  }, [fetcher.data, navigate, projectId]);

  if (!review) return null;

  function handleSubmit(e: FormEvent<HTMLFormElement>) {
    const submitter = (e.nativeEvent as SubmitEvent).submitter as HTMLButtonElement | null;
    const decision = (submitter?.value ?? "ok") as MilestoneDecideBody["decision"];
    if (!milestoneDecisionValid(decision, note)) {
      e.preventDefault();
      setInvalid(true);
      return;
    }
    setInvalid(false);
  }

  return (
    <div className="space-y-3" data-testid="milestone-review">
      <Alert tone="info" title="秘書のまとめ">
        <div data-testid="milestone-review-text">
          <MarkdownViewer content={review.text} />
        </div>
      </Alert>
      {milestone.proposal && (
        <div className="rounded-lg border border-border bg-surface-2/50 p-3 text-sm" data-testid="milestone-proposal">
          <p className="font-medium">次の途中目標の提案: {milestone.proposal.title}</p>
          {milestone.proposal.description && (
            <p className="mt-1 whitespace-pre-wrap text-fg-muted">{milestone.proposal.description}</p>
          )}
        </div>
      )}
      <fetcher.Form method="post" onSubmit={handleSubmit} className="space-y-2">
        <input type="hidden" name="intent" value="milestone_decide" />
        <input type="hidden" name="milestone_id" value={milestone.id} />
        <div>
          <label htmlFor={`milestone-decide-note-${milestone.id}`} className={labelClass}>
            一言（議論・ng は必須）
          </label>
          <textarea
            id={`milestone-decide-note-${milestone.id}`}
            name="note"
            rows={2}
            value={note}
            onChange={(e) => {
              setNote(e.target.value);
              if (invalid) setInvalid(false);
            }}
            aria-invalid={invalid ? true : undefined}
            data-testid="milestone-decide-note"
            className={`${textareaClass} mt-1.5 w-full`}
          />
          {invalid && (
            <p role="alert" className="mt-1 text-xs text-danger" data-testid="milestone-decide-note-required">
              議論・ng には一言が要ります。
            </p>
          )}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button
            type="submit"
            name="decision"
            value="ok"
            variant="success"
            size="sm"
            disabled={busy}
            data-testid="milestone-decide-ok"
          >
            <Icon name="check" />
            ok
          </Button>
          <Button
            type="submit"
            name="decision"
            value="discuss"
            variant="secondary"
            size="sm"
            disabled={busy}
            data-testid="milestone-decide-discuss"
          >
            <Icon name="message" />
            議論
          </Button>
          <Button
            type="submit"
            name="decision"
            value="ng"
            variant="danger"
            size="sm"
            disabled={busy}
            data-testid="milestone-decide-ng"
          >
            <Icon name="x" />
            ng
          </Button>
        </div>
      </fetcher.Form>
      <ProjectActionFlash outcome={fetcher.data} />
    </div>
  );
}

/**
 * 仕事の木の 1 行（Phase 31。実機の事故、2026-09-18）。`draft` には「Go」（`/tasks/:id/approve` の
 * `Trigger::Accept`）、`failed`/`cancelled` には「やり直す」（`/tasks/:id/retry`）を直接置く。
 * どちらも `/tasks/:id` の action へ直接 POST する fetcher（この画面の action は project 単位の
 * intent しか扱わないため）。成功後の遷移は「やり直す」だけ（新しいタスクが増えるので、そちらを見せる）。
 */
function WorkTreeTaskRow({
  task,
  projectId,
  orgName,
}: {
  task: ProjectTaskView;
  projectId: string;
  orgName: string | undefined;
}) {
  const goFetcher = useFetcher<TransitionOutcome>({ key: `work-tree-go-${task.id}` });
  const going = goFetcher.state !== "idle";
  const retryFetcher = useFetcher<RetryOutcome>({ key: `work-tree-retry-${task.id}` });
  const retrying = retryFetcher.state !== "idle";
  const navigate = useNavigate();
  useEffect(() => {
    if (retryFetcher.data?.ok) {
      navigate(`/tasks/${retryFetcher.data.result.task_id}`);
    }
  }, [retryFetcher.data, navigate]);

  return (
    <li
      className="flex flex-wrap items-center gap-2 text-sm"
      data-testid="work-tree-task-row"
      data-task-status={task.status}
    >
      <Badge tone="neutral">{taskStatusLabel(task.status)}</Badge>
      <Link to={`/tasks/${task.id}`} className="underline underline-offset-2">
        {task.title}
      </Link>
      {task.assignee && (
        <>
          <span className="text-xs text-fg-subtle">担当: {orgName ?? task.assignee}</span>
          <Link
            to={`/org/${encodeURIComponent(task.assignee)}?project=${encodeURIComponent(projectId)}`}
            data-testid="work-tree-talk"
            data-assignee={task.assignee}
            className={buttonClass({ variant: "ghost", size: "xs" })}
          >
            <Icon name="message" />
            担当に話す
          </Link>
        </>
      )}
      {task.status === "draft" && (
        <goFetcher.Form method="post" action={`/tasks/${task.id}`}>
          <input type="hidden" name="intent" value="approve" />
          <input type="hidden" name="expected_status" value="draft" />
          <Button type="submit" variant="success" size="xs" disabled={going} data-testid="work-tree-go">
            <Icon name="check" />
            Go
          </Button>
        </goFetcher.Form>
      )}
      {(task.status === "failed" || task.status === "cancelled") && (
        <retryFetcher.Form method="post" action={`/tasks/${task.id}`} className="flex items-center gap-2">
          <input type="hidden" name="intent" value="retry" />
          <label className="flex items-center gap-1 text-xs text-fg-subtle">
            <input type="checkbox" name="accept" value="true" className={checkboxClass} />
            ready で始める
          </label>
          <Button type="submit" variant="primary" size="xs" disabled={retrying} data-testid="work-tree-retry">
            <Icon name="rotate" />
            やり直す
          </Button>
        </retryFetcher.Form>
      )}
      {goFetcher.data && !goFetcher.data.ok && <ErrorFlash error={goFetcher.data.error} />}
      <RetryFlash outcome={retryFetcher.data} />
    </li>
  );
}

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
      <main className="p-4">
        <h1 className="text-xl font-semibold">
          {data.status === 404 ? "案件が見つかりません" : `エラー ${data.status}`}
        </h1>
        <p className="mt-2 text-sm text-fg-muted">{data.detail}</p>
      </main>
    );
  }
  return (
    <main className="p-4">
      <h1 className="text-xl font-semibold">エラー</h1>
      <p className="mt-2 text-sm text-fg-muted">予期しないエラーが起きました。</p>
    </main>
  );
}
