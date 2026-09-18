import { Form, isRouteErrorResponse } from "react-router";
import { ArtifactsList } from "~/components/ArtifactsList";
import { HelpLink } from "~/components/HelpLink";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { labelClass, selectClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, PageHeader, SectionTitle } from "~/components/ui/misc";
import {
  buildProjectArtifactRows,
  type ProjectArtifactRow,
  type TaskArtifactBundle,
  workspacePlace,
} from "~/lib/artifacts";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { TaskdError, type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { ArtifactList, OrgList, Project, ProjectDetail, ProjectList, TaskDetail, TaskId } from "~/taskd/types";
import type { Route } from "./+types/artifacts";

/**
 * 1 タスクぶんの成果物 + 置き場所を束ねる（N+1。`GET /tasks/{id}` と `GET /tasks/{id}/artifacts`）。
 * `~/routes/projects.$id.tsx::loadProjectDetail` にも同じ形の私的ヘルパーがある（docs/DESIGN.md §6.3 の
 * 「BFF の loader は taskd を直接呼ぶ」規則により、GET の集約はモジュール分割せず各 loader に閉じる。
 * `.server.ts` への切り出しは React Router のクライアントバンドル除去の対象が `loader`/`action` 等に
 * 限られるため、公開関数からの参照は避ける）。
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

/**
 * `/artifacts`（成果物、SPEC §2.1・§2.2・§3.7・§4 の 6、Phase G13c）。
 *
 * 案件を選ぶ（`GET /projects`）と、その案件のタスク（`GET /projects/{id}` の `tasks`。仕事の木と同じ集合、
 * ADR-0033 D2）の成果物を横断して一覧する。1 タスクごとに `GET /tasks/{id}/artifacts` と
 * `GET /tasks/{id}`（`workspace_dir` / `task.workspace` を「置き場所」に使う）を束ねる（N+1。G13a と
 * 同じ判断: 一人で使う前提で案件のタスク数は少ない）。担当ノード名は `GET /org` から解決する
 * （taskd 側に判断値を作らせない。`~/lib/work-tree.ts` と同じ規則）。
 */

export interface ArtifactsData {
  projects: Project[];
  selectedProjectId: string | null;
  projectNotFound: boolean;
  rows: ProjectArtifactRow[];
  fetchedAt: string;
}

export async function loadArtifacts(client: TaskdClient, request: Request): Promise<ArtifactsData> {
  const url = new URL(request.url);
  const projectId = url.searchParams.get("project");
  const fetchedAt = new Date().toISOString();
  const [projectList, orgList] = await Promise.all([
    client.get<ProjectList>("/projects", { signal: request.signal }),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
  ]);
  const projects = projectList.items;
  if (!projectId) {
    return { projects, selectedProjectId: null, projectNotFound: false, rows: [], fetchedAt };
  }
  let detail: ProjectDetail;
  try {
    detail = await client.get<ProjectDetail>(`/projects/${encodeURIComponent(projectId)}`, {
      signal: request.signal,
    });
  } catch (e) {
    if (e instanceof TaskdError && e.status === 404) {
      return { projects, selectedProjectId: projectId, projectNotFound: true, rows: [], fetchedAt };
    }
    throw e;
  }
  const orgById = new Map(orgList.items.map((n) => [n.id, n]));
  const bundles = await loadTaskArtifactBundles(
    client,
    detail.tasks.map((t) => t.id),
    request.signal,
  );
  const rows = buildProjectArtifactRows(detail.tasks, bundles, orgById);
  return { projects, selectedProjectId: projectId, projectNotFound: false, rows, fetchedAt };
}

export async function loader({ request }: Route.LoaderArgs): Promise<ArtifactsData> {
  try {
    return await loadArtifacts(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "成果物 - taskd-gui" }];
}

export default function ArtifactsPage({ loaderData }: Route.ComponentProps) {
  const { projects, selectedProjectId, projectNotFound, rows, fetchedAt } = loaderData;

  return (
    <div className="space-y-8" data-testid="artifacts-section">
      <PageHeader
        icon="file"
        title={
          <>
            成果物
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="調査文書・リンク集はここで読みます。コードは置き場所（普段のパス）へのリンクで示します。"
      />

      <Card>
        <CardHeader icon="folder" title="案件を選ぶ" description="選んだ案件の成果物を、担当をまたいで一覧します。" />
        <CardBody>
          <Form method="get" className="flex flex-wrap items-end gap-3">
            <div>
              <label htmlFor="artifacts-project-select" className={labelClass}>
                案件
              </label>
              <select
                id="artifacts-project-select"
                name="project"
                data-testid="artifacts-project-select"
                defaultValue={selectedProjectId ?? ""}
                className={`${selectClass} mt-1.5`}
              >
                <option value="">選んでください</option>
                {projects.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.title}
                  </option>
                ))}
              </select>
            </div>
            <Button type="submit" variant="secondary" size="sm">
              <Icon name="filter" />
              表示
            </Button>
          </Form>
        </CardBody>
      </Card>

      {!selectedProjectId ? (
        <EmptyState icon="file" title="案件を選んでください">
          調査結果の文書と、見るべき関連研究へのリンクがまとまって読めます。
        </EmptyState>
      ) : projectNotFound ? (
        <Alert tone="danger" title="案件が見つかりません">
          <p>選んだ案件は既に無いようです。案件の一覧から選び直してください。</p>
        </Alert>
      ) : rows.length === 0 ? (
        <EmptyState icon="file" title="この案件の成果物はまだありません" />
      ) : (
        <section aria-labelledby="artifacts-heading" className="space-y-4">
          <SectionTitle icon="file" id="artifacts-heading" count={rows.length}>
            成果物
          </SectionTitle>
          <ArtifactsList rows={rows} fetchedAt={fetchedAt} />
        </section>
      )}
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
