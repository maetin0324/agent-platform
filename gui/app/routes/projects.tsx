import { useId, useState } from "react";
import { data, isRouteErrorResponse, Link, redirect, useFetcher } from "react-router";
import { ErrorFlash, FieldErrors } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { RepoFields } from "~/components/RepoFields";
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
import { WorkspaceFields } from "~/components/WorkspaceFields";
import { projectStatusLabel } from "~/lib/labels";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import type { CreateFailure } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { createProject, readProjectCreateInput } from "~/taskd/projects-admin.server";
import { createRepo, readExtraRepoCreateBodies } from "~/taskd/repos-admin.server";
import type { Clusters, ClusterView, Project, ProjectDetail, ProjectList, ProjectStatus } from "~/taskd/types";
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
  /** 作業場所（`GET /clusters`）の選択肢（ADR-0039 D1、Phase G13k）。taskd に届かないときは空。 */
  clusters: ClusterView[];
}

export async function loadProjects(client: TaskdClient, request: Request): Promise<ProjectsData> {
  const [list, clusters] = await Promise.all([
    client.get<ProjectList>("/projects", { signal: request.signal }),
    // 作業場所（クラスタ）の選択肢（ADR-0039 D1、Phase G13k）。`GET /projects/{id}` の N+1 と同じく、
    // 落ちても一覧・作成フォーム自体は出す（クラスタは「まだ決めない」で作れる）。
    client.get<Clusters>("/clusters", { signal: request.signal }).catch(() => ({ items: [] }) as Clusters),
  ]);
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
  return { rows, clusters: clusters.items };
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

/**
 * 作成の失敗（`CreateFailure`）に、**案件だけは作れた**ときの id を添えたもの（ADR-0043 D1、Phase G16）。
 * 「追加のリポジトリ」は案件を作ってから 1 行ずつ `POST /projects/{id}/repos` するので、案件が 201 の
 * あとにリポジトリで 422 / 409 になることがある。そのときは案件へのリンクを添えて taskd の文言を出す。
 */
export interface ProjectCreateFailure extends CreateFailure {
  projectId?: string;
}

/**
 * `POST /projects`。成功したら詳細へ移る（`/tasks/new` と同じ作り）。
 * 従来の単一の `workspace` フォームはそのまま（`readProjectCreateInput`）。ADR-0043 D1 の
 * 「追加のリポジトリ」がある場合だけ、201 のあとに `POST /projects/{id}/repos` を行ごとに送る
 * （`POST /projects` は 1 つの作業場所しか受けないため。docs/taskd-api-v1.md §3.46 / §3.69）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const client = getTaskdClient();
  const result = await createProject(client, readProjectCreateInput(form), request.signal);
  if (!result.ok) return data(result satisfies CreateFailure, { status: result.error.status });
  for (const body of readExtraRepoCreateBodies(form)) {
    const added = await createRepo(client, result.project.id, body, request.signal);
    if (!added.ok) {
      return data({ ok: false, error: added.error, projectId: result.project.id } satisfies ProjectCreateFailure, {
        status: added.error.status,
      });
    }
  }
  return redirect(`/projects/${result.project.id}`);
}

const PROJECT_STATUS_TONE: Record<ProjectStatus, Tone> = {
  proposed: "info",
  active: "primary",
  paused: "warning",
  done: "success",
};

export default function ProjectsPage({ loaderData }: Route.ComponentProps) {
  const { rows, clusters } = loaderData;
  // 失敗（422 等）が SSE の再検証で消えないよう fetcher に載せる（Phase G13f-1、監査 H1）。
  // 成功したら action が `redirect` を返し、fetcher でもそのまま詳細へ移る。
  const fetcher = useFetcher<ProjectCreateFailure>();
  const submitting = fetcher.state !== "idle";
  const result = fetcher.data;
  const error = result && !result.ok ? result.error : undefined;

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
        description="案件は秘書が受け取り、組織の上から下へ分解されて流れます。一覧から案件を開くと、途中目標と仕事の木が見られます。"
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
                  <th className={thClass}>題名</th>
                  <th className={thClass}>状態</th>
                  <th className={thClass}>投げた日</th>
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
                        {projectStatusLabel(project.status)}
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
            description="曖昧なままでかまいません。投げるとすぐ秘書が、理解の確認・大まかな方針・最初の途中目標を返します。"
          />
          <CardBody>
            <p className={`${hintClass} mb-3`} data-testid="project-new-secretary-hint">
              <Link to="/org/secretary" className="underline underline-offset-2">
                秘書に話しかけても同じです
              </Link>
              。そちらは本文だけ書けば、先頭 40 字が題名になります。
            </p>
            <ErrorFlash error={error} />
            {/* 案件は作れたが「追加のリポジトリ」で失敗した場合（ADR-0043 D1、Phase G16）。
                案件そのものは残っているので、続きは案件の画面でやってもらう。 */}
            {result?.projectId && (
              <p className="my-2 text-sm text-fg-muted" data-testid="project-new-partial">
                案件は作成されました（
                <Link to={`/projects/${result.projectId}`} className="underline underline-offset-2">
                  案件を開く
                </Link>
                ）。リポジトリの追加は案件の画面で続けてください。
              </p>
            )}
            <fetcher.Form method="post" data-testid="project-new-form" className="space-y-4">
              <div>
                <label htmlFor="project-title" className={labelClass}>
                  題名
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
                  依頼
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
                  投げた依頼はこのまま担当に渡ります（関連研究調査 → 計画 → 実験 …
                  のように、必要な仕事へ分解されて進みます）。
                </p>
                <FieldErrors error={error} field="request" />
              </div>
              <WorkspaceFields idPrefix="project-new-workspace" clusters={clusters} error={error} />
              <ExtraRepoRows clusters={clusters} error={error} />
              <Button type="submit" variant="primary" disabled={submitting} data-testid="project-new-submit">
                <Icon name="send" />
                投げる
              </Button>
            </fetcher.Form>
          </CardBody>
        </Card>
      </section>
    </div>
  );
}

/**
 * 「追加のリポジトリ」（ADR-0043 D1、docs/taskd-api-v1.md §3.69。Phase G16）。
 * 上の `WorkspaceFields`（従来どおりの単一の `workspace`）が**主なリポジトリ**になり、ここに足した行は
 * 案件を作ったあとに 1 行ずつ `POST /projects/{id}/repos` される（読み手は `readExtraRepoCreateBodies`）。
 * 行を足しただけでパスを書かなかったものは送られない。既定では 1 行も出さない（従来の画面と同じ見た目）。
 */
function ExtraRepoRows({
  clusters,
  error,
}: {
  clusters: readonly ClusterView[];
  error: CreateFailure["error"] | undefined;
}) {
  const baseId = useId();
  const [rowIds, setRowIds] = useState<number[]>([]);
  const [nextId, setNextId] = useState(0);

  return (
    <div className="space-y-3" data-testid="project-new-extra-repos">
      {rowIds.map((rowId, index) => (
        <div key={rowId} className="rounded-lg border border-border bg-surface-2/40 p-3" data-testid="extra-repo-row">
          <div className="mb-2 flex items-center justify-between gap-2">
            <span className={labelClass}>追加のリポジトリ {index + 1}</span>
            <Button
              type="button"
              variant="ghost"
              size="xs"
              data-testid="extra-repo-remove"
              onClick={() => setRowIds((ids) => ids.filter((id) => id !== rowId))}
            >
              <Icon name="x" />
              この行を消す
            </Button>
          </div>
          <RepoFields
            idPrefix={`${baseId}-extra-repo-${rowId}`}
            namePrefix="extra_repo"
            clusters={clusters}
            error={error}
          />
        </div>
      ))}
      <Button
        type="button"
        variant="secondary"
        size="sm"
        data-testid="project-new-extra-repo-add"
        onClick={() => {
          setRowIds((ids) => [...ids, nextId]);
          setNextId((n) => n + 1);
        }}
      >
        <Icon name="plus" />
        追加のリポジトリ
      </Button>
      <p className={hintClass}>
        論文とコードのように、1
        つの案件で複数のリポジトリを使うときに足してください。上の「作業場所」が主なリポジトリになります。
      </p>
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
