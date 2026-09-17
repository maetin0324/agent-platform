import { useMemo } from "react";
import {
  data,
  type FetcherWithComponents,
  isRouteErrorResponse,
  Link,
  useFetcher,
  useSearchParams,
} from "react-router";
import { OrgActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Badge, StatusBadge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, selectClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { DataItem, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import { buildOrgTree, countWorkload, flattenProjectTasks, type OrgTreeNode, type Workload } from "~/lib/org-tree";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { cn } from "~/lib/utils";
import { TaskdBanner } from "~/root";
import type { OrgOpOutcome } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { formString } from "~/taskd/forms";
import {
  buildOrgCreateInput,
  buildOrgPatchInput,
  createOrgNode,
  deleteOrgNode,
  patchOrgNode,
} from "~/taskd/org-admin.server";
import type { ConfigView, OrgKind, OrgList, OrgNode, ProjectDetail, ProjectList, ProjectTaskView } from "~/taskd/types";
import type { Route } from "./+types/org";

/**
 * `/org`（組織の木、SPEC §3.2、ADR-0033 D1、docs/gui/api.md §3.42〜3.45）。
 * `GET /org` は木にしない（API は position 順の平らな配列）ので、`parent_id` から GUI 側で組む
 * （`~/lib/org-tree.ts`）。「抱えている仕事の数」は `GET /tasks?assignee=` が無いため
 * （taskd-requests.md に依頼を記録）、各案件の仕事の木（`ProjectTaskView.assignee`）を束ねて数える。
 */

export interface OrgData {
  org: OrgList;
  genres: string[];
  workload: Record<string, Workload>;
  assignedTasks: (ProjectTaskView & { project_id: string; project_title: string })[];
}

export async function loadOrg(client: TaskdClient, request: Request): Promise<OrgData> {
  const [org, config, projects] = await Promise.all([
    client.get<OrgList>("/org", { signal: request.signal }),
    client.get<ConfigView>("/config", { signal: request.signal }).catch(() => null),
    client.get<ProjectList>("/projects", { signal: request.signal }),
  ]);
  const withTasks = await Promise.all(
    projects.items.map(async (project) => {
      try {
        const detail = await client.get<ProjectDetail>(`/projects/${encodeURIComponent(project.id)}`, {
          signal: request.signal,
        });
        return { project, tasks: detail.tasks };
      } catch {
        return { project, tasks: [] as ProjectTaskView[] };
      }
    }),
  );
  const assignedTasks = flattenProjectTasks(withTasks);
  const workload = Object.fromEntries(countWorkload(assignedTasks));
  return { org, genres: (config?.genres ?? []).map((g) => g.id), workload, assignedTasks };
}

export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<OrgData> {
  try {
    return await loadOrg(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "組織 - taskd-gui" }];
}

/** 追加・編集・削除（すべて管理系。ADR-0033 D1）。GUI 側では判断しない: フォームの `intent` を写すだけ。 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getTaskdClient();
  const id = formString(form, "id") ?? "";

  let outcome: OrgOpOutcome;
  switch (intent) {
    case "org_create":
      outcome = await createOrgNode(client, buildOrgCreateInput(form), request.signal);
      break;
    case "org_patch":
      outcome = await patchOrgNode(client, id, buildOrgPatchInput(form), request.signal);
      break;
    case "org_delete":
      outcome = await deleteOrgNode(client, id, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

const ORG_KINDS: OrgKind[] = ["secretary", "department", "section"];

export default function OrgPage({ loaderData }: Route.ComponentProps) {
  const { org, genres, workload, assignedTasks } = loaderData;
  const [searchParams] = useSearchParams();
  const selectedId = searchParams.get("selected");
  const { roots } = useMemo(() => buildOrgTree(org.items), [org.items]);
  const selected = selectedId ? (org.items.find((n) => n.id === selectedId) ?? null) : null;
  const fetcher = useFetcher<OrgOpOutcome>();
  const submitting = fetcher.state !== "idle";

  const nodeTasks = selected ? assignedTasks.filter((t) => t.assignee === selected.id) : [];

  return (
    <div className="space-y-8">
      <PageHeader
        icon="users"
        title={
          <>
            組織
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="SPEC §3.2「組織（一つ、役割の木）」。誰が何を抱えているかを見て、ノードを選ぶと詳細が出ます（「話す」は G13b）。"
      />

      <OrgActionFlash outcome={fetcher.data} />

      <section aria-labelledby="org-heading" data-testid="org-section" className="grid gap-6 lg:grid-cols-[22rem_1fr]">
        <div>
          <SectionTitle icon="users" id="org-heading" count={org.items.length} className="mb-3">
            組織の木
          </SectionTitle>
          <Card>
            <CardBody>
              {roots.length === 0 ? (
                <EmptyState icon="users" title="組織が空です" />
              ) : (
                <ul data-testid="org-tree" className="space-y-1">
                  {roots.map((r) => (
                    <OrgTreeItem key={r.node.id} item={r} depth={0} selectedId={selectedId} workload={workload} />
                  ))}
                </ul>
              )}
            </CardBody>
          </Card>
        </div>

        <div>
          <SectionTitle icon="user" className="mb-3">
            詳細
          </SectionTitle>
          <Card>
            {selected ? (
              <OrgNodeDetail
                key={selected.id}
                node={selected}
                org={org.items}
                genres={genres}
                workload={workload[selected.id]}
                tasks={nodeTasks}
                fetcher={fetcher}
                submitting={submitting}
              />
            ) : (
              <CardBody>
                <EmptyState icon="user" title="ノードを選んでください">
                  左の組織の木から 1 件選ぶと、担当・抱えているタスク・編集フォームが出ます。
                </EmptyState>
              </CardBody>
            )}
          </Card>
        </div>
      </section>

      <section aria-labelledby="org-add-heading" className="space-y-4">
        <SectionTitle icon="plus" id="org-add-heading">
          役職を追加
        </SectionTitle>
        <Card>
          <CardHeader
            icon="plus"
            title="新規ノード"
            description="部・課を足します（SPEC §3.2「途中で役職を足す・分ける・消すことはできる」）。"
          />
          <CardBody>
            <fetcher.Form method="post" data-testid="org-add-form" className="grid grid-cols-2 gap-4 sm:grid-cols-3">
              <input type="hidden" name="intent" value="org_create" />
              <div>
                <label htmlFor="org-add-id" className={labelClass}>
                  id
                </label>
                <input
                  id="org-add-id"
                  name="id"
                  type="text"
                  data-testid="org-add-id"
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
                <p className={hintClass}>英小文字ケバブ（例: coding-poc）。</p>
              </div>
              <div>
                <label htmlFor="org-add-name" className={labelClass}>
                  name
                </label>
                <input id="org-add-name" name="name" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
              </div>
              <div>
                <label htmlFor="org-add-kind" className={labelClass}>
                  kind
                </label>
                <select
                  id="org-add-kind"
                  name="kind"
                  defaultValue="section"
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  {ORG_KINDS.map((k) => (
                    <option key={k} value={k}>
                      {k}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="org-add-parent" className={labelClass}>
                  parent_id
                </label>
                <select
                  id="org-add-parent"
                  name="parent_id"
                  defaultValue=""
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  <option value="">(なし・根)</option>
                  {org.items.map((n) => (
                    <option key={n.id} value={n.id}>
                      {n.id}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="org-add-genre" className={labelClass}>
                  genre
                </label>
                {genres.length > 0 ? (
                  <select id="org-add-genre" name="genre" defaultValue="" className={cn(selectClass, "mt-1.5 w-full")}>
                    <option value="">(なし)</option>
                    {genres.map((g) => (
                      <option key={g} value={g}>
                        {g}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input id="org-add-genre" name="genre" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
                )}
              </div>
              <div>
                <label htmlFor="org-add-position" className={labelClass}>
                  position
                </label>
                <input
                  id="org-add-position"
                  name="position"
                  type="number"
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>
              <div className="col-span-2 sm:col-span-3">
                <label htmlFor="org-add-brief" className={labelClass}>
                  brief
                </label>
                <input id="org-add-brief" name="brief" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
                <p className={hintClass}>担当の一言（プロンプトに前置きされます）。</p>
              </div>
              <div className="col-span-2 sm:col-span-3">
                <Button type="submit" variant="primary" disabled={submitting} data-testid="org-add-submit">
                  <Icon name="plus" />
                  追加
                </Button>
              </div>
            </fetcher.Form>
          </CardBody>
        </Card>
      </section>
    </div>
  );
}

function OrgTreeItem({
  item,
  depth,
  selectedId,
  workload,
}: {
  item: OrgTreeNode;
  depth: number;
  selectedId: string | null;
  workload: Record<string, Workload>;
}) {
  const { node, children } = item;
  const active = node.id === selectedId;
  const open = workload[node.id]?.open ?? 0;
  return (
    <li>
      <Link
        to={`/org?selected=${encodeURIComponent(node.id)}`}
        data-testid="org-node"
        data-org-id={node.id}
        className={cn(
          "flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm no-underline transition-colors",
          active ? "bg-primary-soft text-primary-soft-fg" : "text-fg hover:bg-surface-2",
        )}
        style={{ marginLeft: depth * 14 }}
      >
        <Badge tone="neutral" className="shrink-0">
          {node.kind}
        </Badge>
        <span className="min-w-0 flex-1 truncate font-medium">{node.name}</span>
        {node.genre && (
          <span className="shrink-0 rounded-full border border-dashed border-border-strong px-1.5 py-0.5 text-[0.7rem] text-fg-subtle">
            {node.genre}
          </span>
        )}
        <span className="shrink-0 rounded-full bg-surface-2 px-1.5 py-0.5 text-[0.7rem] tabular-nums text-fg-subtle">
          {open}
        </span>
      </Link>
      {children.length > 0 && (
        <ul className="mt-1 space-y-1 border-l border-border pl-2">
          {children.map((c) => (
            <OrgTreeItem key={c.node.id} item={c} depth={depth + 1} selectedId={selectedId} workload={workload} />
          ))}
        </ul>
      )}
    </li>
  );
}

function OrgNodeDetail({
  node,
  org,
  genres,
  workload,
  tasks,
  fetcher,
  submitting,
}: {
  node: OrgNode;
  org: OrgNode[];
  genres: string[];
  workload: Workload | undefined;
  tasks: (ProjectTaskView & { project_id: string; project_title: string })[];
  fetcher: FetcherWithComponents<OrgOpOutcome>;
  submitting: boolean;
}) {
  return (
    <div data-testid="org-node-detail">
      <CardHeader
        icon="user"
        title={<Mono className="text-sm font-semibold text-fg">{node.name}</Mono>}
        description={node.id}
        actions={
          <>
            <Badge tone="neutral">{node.kind}</Badge>
            {node.genre && <Badge tone="teal">{node.genre}</Badge>}
          </>
        }
      />
      <CardBody className="space-y-4">
        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm">
          <DataItem label="brief" wide>
            {node.brief || "-"}
          </DataItem>
          <DataItem label="抱えている仕事（未終了）">
            <span data-testid="org-node-open-count" className="tabular-nums">
              {workload?.open ?? 0}
            </span>
          </DataItem>
          <DataItem label="担当した仕事（累計）">
            <span className="tabular-nums">{workload?.total ?? 0}</span>
          </DataItem>
        </dl>

        <Button variant="ghost" size="sm" disabled data-testid="org-node-talk" title="G13b で実装します">
          <Icon name="message" />
          話す（G13b）
        </Button>

        <div>
          <p className={labelClass}>抱えているタスク</p>
          {tasks.length === 0 ? (
            <p className={cn(hintClass, "mt-1")}>今のところありません。</p>
          ) : (
            <ul className="mt-1.5 space-y-1" data-testid="org-node-tasks">
              {tasks.slice(0, 20).map((t) => (
                <li key={t.id} className="flex items-center gap-2 text-sm">
                  <StatusBadge status={t.status} />
                  <Link to={`/tasks/${t.id}`} className="min-w-0 flex-1 truncate underline underline-offset-2">
                    {t.title}
                  </Link>
                  <span className="shrink-0 text-xs text-fg-subtle">{t.project_title}</span>
                </li>
              ))}
            </ul>
          )}
        </div>

        <details className="group">
          <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-border bg-surface px-3 text-sm text-fg shadow-xs hover:bg-surface-2">
            <Icon name="settings" className="size-4" />
            編集
          </summary>
          <fetcher.Form
            method="post"
            data-testid="org-edit-form"
            className="mt-3 space-y-3 rounded-lg border border-border bg-surface-2/40 p-3"
          >
            <input type="hidden" name="intent" value="org_patch" />
            <input type="hidden" name="id" value={node.id} />
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className={labelClass} htmlFor={`org-edit-name-${node.id}`}>
                  name
                </label>
                <input
                  id={`org-edit-name-${node.id}`}
                  name="name"
                  type="text"
                  defaultValue={node.name}
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>
              <div>
                <label className={labelClass} htmlFor={`org-edit-kind-${node.id}`}>
                  kind
                </label>
                <select
                  id={`org-edit-kind-${node.id}`}
                  name="kind"
                  defaultValue={node.kind}
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  {ORG_KINDS.map((k) => (
                    <option key={k} value={k}>
                      {k}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className={labelClass} htmlFor={`org-edit-parent-${node.id}`}>
                  parent_id
                </label>
                <select
                  id={`org-edit-parent-${node.id}`}
                  name="parent_id"
                  defaultValue={node.parent_id ?? ""}
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  <option value="">(なし・根)</option>
                  {org
                    .filter((n) => n.id !== node.id)
                    .map((n) => (
                      <option key={n.id} value={n.id}>
                        {n.id}
                      </option>
                    ))}
                </select>
              </div>
              <div>
                <label className={labelClass} htmlFor={`org-edit-genre-${node.id}`}>
                  genre
                </label>
                {genres.length > 0 ? (
                  <select
                    id={`org-edit-genre-${node.id}`}
                    name="genre"
                    defaultValue={node.genre ?? ""}
                    className={cn(selectClass, "mt-1.5 w-full")}
                  >
                    <option value="">(なし)</option>
                    {genres.map((g) => (
                      <option key={g} value={g}>
                        {g}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    id={`org-edit-genre-${node.id}`}
                    name="genre"
                    type="text"
                    defaultValue={node.genre ?? ""}
                    className={cn(inputClass, "mt-1.5 w-full")}
                  />
                )}
              </div>
              <div className="col-span-2">
                <label className={labelClass} htmlFor={`org-edit-brief-${node.id}`}>
                  brief
                </label>
                <input
                  id={`org-edit-brief-${node.id}`}
                  name="brief"
                  type="text"
                  defaultValue={node.brief}
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>
              <div>
                <label className={labelClass} htmlFor={`org-edit-position-${node.id}`}>
                  position
                </label>
                <input
                  id={`org-edit-position-${node.id}`}
                  name="position"
                  type="number"
                  defaultValue={node.position ?? 0}
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>
            </div>
            <Button type="submit" variant="primary" size="sm" disabled={submitting} data-testid="org-edit-submit">
              <Icon name="check" />
              保存
            </Button>
          </fetcher.Form>
        </details>

        <details className="group">
          <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-danger-border bg-danger-soft px-3 text-sm text-danger-soft-fg shadow-xs hover:bg-danger hover:text-white">
            <Icon name="xCircle" className="size-4" />
            削除
          </summary>
          <fetcher.Form method="post" className="mt-3 rounded-lg border border-danger-border bg-danger-soft/40 p-3">
            <input type="hidden" name="intent" value="org_delete" />
            <input type="hidden" name="id" value={node.id} />
            <p className="mb-2 text-sm text-fg-muted">
              本当に <span className="font-mono">{node.id}</span> を削除しますか？
              仕事を抱えている・子ノードがあると断られます（409 <code>org_node_in_use</code>）。
            </p>
            <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="org-delete">
              <Icon name="xCircle" />
              削除する
            </Button>
          </fetcher.Form>
        </details>
      </CardBody>
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
