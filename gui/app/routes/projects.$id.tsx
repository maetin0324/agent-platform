import { useMemo } from "react";
import { data, isRouteErrorResponse, useFetcher } from "react-router";
import { ProjectActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { ReportsList } from "~/components/ReportsList";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { inputClass, labelClass, selectClass, textareaClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, EmptyState, PageHeader, SectionTitle } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { WorkTree } from "~/components/WorkTree";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { projectTasksToGraph } from "~/lib/work-tree";
import { TaskdBanner } from "~/root";
import type { ProjectOpOutcome } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { formString } from "~/taskd/forms";
import { createMilestone, patchMilestoneStatus, patchProjectStatus } from "~/taskd/projects-admin.server";
import type { MilestoneStatus, OrgList, ProjectDetail, ProjectStatus, ReportList } from "~/taskd/types";
import type { Route } from "./+types/projects.$id";

/**
 * `/projects/:id`（案件の詳細・途中目標・仕事の木・報告、SPEC §3.3・§3.5、ADR-0033 D2/D3、
 * docs/gui/api.md §3.47〜3.51）。
 * 「仕事の木」は `GET /projects/{id}` の `tasks`（`ProjectTaskView`、`parent_id` / `depends_on` は既存の DAG
 * と同じ辺の作り方）を `/graph` と同じ `layoutGraph`（`~/components/WorkTree.tsx`）で描く。
 * 各ノードには `assignee` の組織ノードの名前を出す（`GET /org` と突き合わせる。組織のノード名を出すだけで、
 * taskd 側の判断値は増やさない）。
 * 「報告」タブは `GET /reports?project=<id>`（**全レベル**。`level` を付けない。`/reports` の既定は秘書
 * レベルの未読だけだが、案件詳細ではこの案件のすべての段の報告を見せる。G13b-1 の依頼どおり）を
 * `/reports` と同じ `ReportsList` で出す。
 */

export interface ProjectDetailData {
  detail: ProjectDetail;
  org: OrgList;
  reports: ReportList;
  fetchedAt: string;
}

export async function loadProjectDetail(client: TaskdClient, id: string, request: Request): Promise<ProjectDetailData> {
  const [detail, org, reports] = await Promise.all([
    client.get<ProjectDetail>(`/projects/${encodeURIComponent(id)}`, { signal: request.signal }),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
    client
      .get<ReportList>("/reports", { query: { project: id }, signal: request.signal })
      .catch(() => ({ items: [] }) as ReportList),
  ]);
  return { detail, org, reports, fetchedAt: new Date().toISOString() };
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
  return [{ title: "案件詳細 - taskd-gui" }];
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
    case "milestone_status":
      outcome = await patchMilestoneStatus(
        client,
        formString(form, "milestone_id") ?? "",
        (formString(form, "status") ?? "proposed") as MilestoneStatus,
        request.signal,
      );
      break;
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
  const { detail, org, reports, fetchedAt } = loaderData;
  const { project, milestones, tasks } = detail;
  const fetcher = useFetcher<ProjectOpOutcome>();
  const submitting = fetcher.state !== "idle";

  const orgById = useMemo(() => new Map(org.items.map((n) => [n.id, n])), [org.items]);
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
        description="SPEC §3.3「案件は組織の上から入り、分解されて下へ流れる。分解された仕事の依存関係が仕事の木」。"
        actions={<Badge tone={PROJECT_STATUS_TONE[project.status]}>{project.status}</Badge>}
      />

      <ProjectActionFlash outcome={fetcher.data} />

      <section aria-labelledby="project-detail-heading" className="space-y-4">
        <SectionTitle icon="file" id="project-detail-heading">
          依頼
        </SectionTitle>
        <Card>
          <CardBody className="space-y-4">
            <DataItem label="request" wide>
              <p className="whitespace-pre-wrap" data-testid="project-request-text">
                {project.request}
              </p>
            </DataItem>
            {project.secretary_summary && (
              <Alert tone="info" title="秘書の理解確認・方針" data-testid="project-secretary-summary">
                <p className="whitespace-pre-wrap">{project.secretary_summary}</p>
              </Alert>
            )}
            <fetcher.Form method="post" className="flex flex-wrap items-end gap-3" data-testid="project-status-form">
              <input type="hidden" name="intent" value="project_status" />
              <div>
                <label htmlFor="project-status-select" className={labelClass}>
                  status
                </label>
                <select
                  id="project-status-select"
                  name="status"
                  defaultValue={project.status}
                  className={`${selectClass} mt-1.5`}
                >
                  {PROJECT_STATUSES.map((s) => (
                    <option key={s} value={s}>
                      {s}
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
                    <CardBody className="space-y-2">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-mono text-xs text-fg-subtle">#{m.seq}</span>
                        <span className="font-medium">{m.title}</span>
                        <Badge tone={MILESTONE_STATUS_TONE[m.status]} data-testid="milestone-status">
                          {m.status}
                        </Badge>
                      </div>
                      {m.description && <p className="text-sm text-fg-muted">{m.description}</p>}
                      <fetcher.Form method="post" className="flex flex-wrap items-end gap-2">
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
                              {s}
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
            description="SPEC §7「途中目標は予め大まかに決めておいて、適宜再設計する」"
          />
          <CardBody>
            <fetcher.Form method="post" data-testid="milestone-new-form" className="space-y-3">
              <input type="hidden" name="intent" value="milestone_create" />
              <div>
                <label htmlFor="milestone-title" className={labelClass}>
                  title
                </label>
                <input id="milestone-title" name="title" type="text" className={`${inputClass} mt-1.5 w-full`} />
              </div>
              <div>
                <label htmlFor="milestone-description" className={labelClass}>
                  description
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

      <section aria-labelledby="work-tree-heading" className="space-y-4">
        <SectionTitle icon="gitBranch" id="work-tree-heading" count={tasks.length}>
          仕事の木
        </SectionTitle>
        <p className="text-xs text-fg-subtle">
          SPEC §3.3「これをパッと見れば、おかしな方針を立てていないかが分かる」。ノードをクリックするとタスク詳細（
          <code>/tasks/:id</code>）へ移ります。
        </p>
        {tasks.length === 0 ? (
          <EmptyState icon="gitBranch" title="この案件のタスクはまだありません" />
        ) : (
          <WorkTree graph={graph} />
        )}
      </section>

      <section aria-labelledby="project-reports-heading" className="space-y-4">
        <SectionTitle icon="send" id="project-reports-heading" count={reports.items.length}>
          報告
        </SectionTitle>
        <p className="text-xs text-fg-subtle">
          SPEC §4「報告の流れ — 各所から上がってくる報告を高速で流し見する」。この案件のすべての段の報告（
          <HelpLink anchor="glossary" label="報告" />
          ）。全体の未読・秘書レベルは <code>/reports</code> で見られます。
        </p>
        {reports.items.length === 0 ? (
          <EmptyState icon="send" title="この案件の報告はまだありません" />
        ) : (
          <ReportsList items={reports.items} projects={[project]} org={org.items} fetchedAt={fetchedAt} />
        )}
      </section>
    </div>
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
