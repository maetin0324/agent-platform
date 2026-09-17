import { data, Form, isRouteErrorResponse, Link, redirect, useNavigation } from "react-router";
import { ErrorFlash, FieldErrors } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import {
  hintClass,
  inputClass,
  labelClass,
  tableClass,
  tdClass,
  textareaClass,
  thClass,
  theadClass,
  trHoverClass,
} from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { EmptyState, PageHeader, SectionTitle } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import type { CreateFailure } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { createProject, readProjectCreateInput } from "~/taskd/projects-admin.server";
import type { Project, ProjectDetail, ProjectList, ProjectStatus } from "~/taskd/types";
import type { Route } from "./+types/projects";

/**
 * `/projects`（案件の一覧と作成、SPEC §3.3・§4、ADR-0033 D2、docs/gui/api.md §3.46）。
 * 一覧は `GET /projects` に加え、「途中目標の数」を出すため各案件の `GET /projects/{id}` を束ねて取る
 * （`ProjectList` 自体には milestones が無い。件数は API が返した `milestones.length` そのままで、
 * GUI 側で新しい判断はしていない）。
 */

export interface ProjectRow {
  project: Project;
  milestoneCount: number;
}

export interface ProjectsData {
  rows: ProjectRow[];
}

export async function loadProjects(client: TaskdClient, request: Request): Promise<ProjectsData> {
  const list = await client.get<ProjectList>("/projects", { signal: request.signal });
  const rows = await Promise.all(
    list.items.map(async (project) => {
      try {
        const detail = await client.get<ProjectDetail>(`/projects/${encodeURIComponent(project.id)}`, {
          signal: request.signal,
        });
        return { project, milestoneCount: detail.milestones.length };
      } catch {
        return { project, milestoneCount: 0 };
      }
    }),
  );
  return { rows };
}

export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ProjectsData> {
  try {
    return await loadProjects(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "案件 - taskd-gui" }];
}

/** `POST /projects`。成功したら詳細へ移る（`/tasks/new` と同じ作り）。 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const result = await createProject(getTaskdClient(), readProjectCreateInput(form), request.signal);
  if (result.ok) return redirect(`/projects/${result.project.id}`);
  return data(result satisfies CreateFailure, { status: result.error.status });
}

const PROJECT_STATUS_TONE: Record<ProjectStatus, Tone> = {
  proposed: "info",
  active: "primary",
  paused: "warning",
  done: "success",
};

export default function ProjectsPage({ loaderData, actionData }: Route.ComponentProps) {
  const { rows } = loaderData;
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";
  const error = actionData && !actionData.ok ? actionData.error : undefined;

  return (
    <div className="space-y-8">
      <PageHeader
        icon="folder"
        title={
          <>
            案件
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="SPEC §3.3「案件と、仕事の木（DAG）」。案件は組織の上から入り、分解されて下へ流れます。"
      />

      <section aria-labelledby="projects-heading" data-testid="projects-section" className="space-y-4">
        <SectionTitle icon="folder" id="projects-heading" count={rows.length}>
          案件一覧
        </SectionTitle>
        {rows.length === 0 ? (
          <EmptyState icon="folder" title="案件がありません">
            下のフォームから最初の案件を投げてください。
          </EmptyState>
        ) : (
          <div className="overflow-x-auto rounded-lg border border-border">
            <table className={tableClass}>
              <thead className={theadClass}>
                <tr>
                  <th className={thClass}>title</th>
                  <th className={thClass}>status</th>
                  <th className={thClass}>作成日</th>
                  <th className={thClass}>途中目標</th>
                </tr>
              </thead>
              <tbody>
                {rows.map(({ project, milestoneCount }) => (
                  <tr key={project.id} className={trHoverClass} data-testid="project-row" data-project-id={project.id}>
                    <td className={tdClass}>
                      <Link to={`/projects/${project.id}`} className="font-medium underline underline-offset-2">
                        {project.title}
                      </Link>
                    </td>
                    <td className={tdClass}>
                      <Badge tone={PROJECT_STATUS_TONE[project.status]} data-testid="project-status">
                        {project.status}
                      </Badge>
                    </td>
                    <td className={tdClass}>
                      <span className="text-fg-subtle">{project.created_at}</span>
                    </td>
                    <td className={tdClass}>
                      <span className="tabular-nums" data-testid="project-milestone-count">
                        {milestoneCount}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section aria-labelledby="project-new-heading" className="space-y-4">
        <SectionTitle icon="plus" id="project-new-heading">
          新しい案件
        </SectionTitle>
        <Card>
          <CardHeader
            icon="plus"
            title="案件を投げる"
            description="SPEC §2.3 のとおり、曖昧なままでかまいません。作るとすぐ秘書が理解の確認・大まかな方針・最初の途中目標を返します（SPEC §7）。"
          />
          <CardBody>
            <p className={`${hintClass} mb-3`} data-testid="project-new-secretary-hint">
              <Link to="/org/secretary" className="underline underline-offset-2">
                秘書に話しかけても同じです
              </Link>
              （SPEC §4「秘書との対話 — 案件を投げる、状況を聞く、方針を変える」）。そちらは本文だけ書けば、先頭 40
              字が案件名になります。
            </p>
            <ErrorFlash error={error} />
            <Form method="post" data-testid="project-new-form" className="space-y-4">
              <div>
                <label htmlFor="project-title" className={labelClass}>
                  title
                </label>
                <input
                  id="project-title"
                  name="title"
                  type="text"
                  data-testid="project-title"
                  placeholder="例: Pluvio の新テーマ"
                  className={`${inputClass} mt-1.5 w-full`}
                />
                <FieldErrors error={error} field="title" />
              </div>
              <div>
                <label htmlFor="project-request" className={labelClass}>
                  request
                </label>
                <textarea
                  id="project-request"
                  name="request"
                  rows={4}
                  data-testid="project-request"
                  placeholder="例: Pluvio を基盤に用いた新たな研究テーマの模索、検証（「これとこれを組み合わせた研究がしたい」くらい曖昧でかまいません）"
                  className={`${textareaClass} mt-1.5 w-full`}
                />
                <p className={hintClass}>
                  SPEC §2.3「関連研究調査 → 実装の計画立案 → … のような種々の仕事に分解され、実行される」。
                </p>
                <FieldErrors error={error} field="request" />
              </div>
              <Button type="submit" variant="primary" disabled={submitting} data-testid="project-new-submit">
                <Icon name="send" />
                投げる
              </Button>
            </Form>
          </CardBody>
        </Card>
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
        <h1 className="text-xl font-semibold">エラー {data.status}</h1>
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
