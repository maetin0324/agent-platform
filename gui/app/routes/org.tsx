import { useMemo, useState } from "react";
import {
  data,
  type FetcherWithComponents,
  Form,
  isRouteErrorResponse,
  Link,
  useFetcher,
  useSearchParams,
} from "react-router";
import type { OrgOpOutcome, OrgSkillMountOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { CelerisError, type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { formString } from "~/celeris/forms";
import {
  buildOrgCreateInput,
  buildOrgPatchInput,
  createOrgNode,
  deleteOrgNode,
  patchOrgNode,
} from "~/celeris/org-admin.server";
import { mountSkill, unmountSkill } from "~/celeris/skills-admin.server";
import type {
  ConfigView,
  EffectiveProfile,
  MemoryView,
  NodeSessionSummary,
  OrgKind,
  OrgList,
  OrgNode,
  Profile,
  Project,
  ProjectList,
  SkillList,
  StandingRule,
  StandingRuleList,
  TaskList,
  TaskSummary,
} from "~/celeris/types";
import { ErrorFlash, OrgActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Badge, statusTone } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import {
  checkboxClass,
  chipLabelClass,
  hintClass,
  inputClass,
  labelClass,
  selectClass,
  textareaClass,
  touchLinkClass,
} from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { DataItem, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import { standingRuleTargetName } from "~/lib/approvals";
import {
  harnessOptions,
  KNOWLEDGE_KINDS,
  knowledgeKindLabel,
  orgKindMark,
  PROFILE_RUNS,
  PROFILE_TOOLS,
  PROFILE_TOOLS_EXTRA_HINT,
  profileFieldLabel,
  profileRunLabel,
  TIERS,
  taskStatusLabel,
  tierLabel,
} from "~/lib/labels";
import { buildOrgTree, countWorkload, type OrgTreeNode, tasksByAssignee, type Workload } from "~/lib/org-tree";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { splitMountedSkills } from "~/lib/skills";
import { cn } from "~/lib/utils";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/org";

/**
 * `/org`（組織の木、SPEC §3.2、ADR-0033 D1、docs/gui/api.md §3.42〜3.45）。
 * `GET /org` は木にしない（API は position 順の平らな配列）ので、`parent_id` から GUI 側で組む
 * （`~/lib/org-tree.ts`）。「抱えている仕事の数」は `GET /tasks`（`TaskSummary.assignee`、Phase 27 で追加。
 * celeris-requests.md R3 が解決済み）を 1 回呼んで数える（`app/routes/tasks.new.tsx` と同じ `limit=500` の
 * 「全件を 1 回で」パターン）。対話用タスク（`TaskSummary.conversation`）は数えない（GUI-R3）。
 * G13a では `assignee` が `TaskSummary` に無かったため `GET /projects/{id}` を案件数ぶん束ねる N+1 で
 * 代替していたが、その代替はやめた。
 */

export interface OrgData {
  org: OrgList;
  genres: string[];
  workload: Record<string, Workload>;
  tasksByAssignee: Record<string, TaskSummary[]>;
  /** 選ばれた担当宛ての永続の認可（全員向け + その担当向け。ADR-0033 D5、§3.58）。未選択なら空。 */
  standingRules: StandingRule[];
  /** 案件の選択肢（記憶の「この案件の引き出し」用。`GET /projects`。落ちても空でよい）。 */
  projects: Project[];
  /** 記憶（`GET /org/{id}/memory`。ADR-0033 D6、§3.62）。担当が未選択・記憶が無効なら null。 */
  memory: MemoryView | null;
  /** `[memory]` が設定されていない（409 `memory_unavailable`）。 */
  memoryUnavailable: boolean;
  /**
   * skill を mount する先の選択肢（ADR-0056 D3 続き、§3.112。Phase 82 / G35）。`GET /skills`
   * （`[knowledge] root` が無ければ 409 → `null`。読めなくても「mount された skills」節は
   * own/inherited の表示だけはできるので、画面は壊さない）。
   */
  skills: SkillList | null;
}

export async function loadOrg(client: CelerisClient, request: Request): Promise<OrgData> {
  const url = new URL(request.url);
  const selected = url.searchParams.get("selected");
  // 記憶の「この案件の引き出し」を見るための案件（`?project=`）。空文字は「選んでいない」。
  const projectParam = url.searchParams.get("project");
  const projectId = projectParam !== null && projectParam.length > 0 ? projectParam : null;
  const [org, config, tasks, standingRules, projects, memoryResult, skills] = await Promise.all([
    client.get<OrgList>("/org", { signal: request.signal }),
    client.get<ConfigView>("/config", { signal: request.signal }).catch(() => null),
    client.get<TaskList>("/tasks", { query: { limit: 500, order: "created_desc" }, signal: request.signal }),
    selected
      ? client
          .get<StandingRuleList>("/standing-rules", { query: { node: selected }, signal: request.signal })
          .catch(() => ({ items: [] }) as StandingRuleList)
      : Promise.resolve({ items: [] } as StandingRuleList),
    client.get<ProjectList>("/projects", { signal: request.signal }).catch(() => ({ items: [] }) as ProjectList),
    // 記憶（SPEC §3.2「記憶は案件をまたぐ」、§3.62）。担当を選んでいるときだけ読む。
    // `[memory]` 未設定は 409 `memory_unavailable`（その旨を出すだけで画面は壊さない）。
    selected
      ? client
          .get<MemoryView>(`/org/${encodeURIComponent(selected)}/memory`, {
            query: { project: projectId },
            signal: request.signal,
          })
          .then((memory) => ({ memory, unavailable: false }))
          .catch((e) => ({
            memory: null,
            unavailable: e instanceof CelerisError && e.status === 409 && e.code === "memory_unavailable",
          }))
      : Promise.resolve({ memory: null, unavailable: false }),
    // ADR-0056 D3 続き（Phase 82 / G35）: mount の picker の選択肢（`GET /skills`）。`[knowledge] root`
    // が無い構成では 409 になるだけなので、その場合は「mount された skills」節を own/inherited の
    // 表示だけにして壊さない。
    client.get<SkillList>("/skills", { signal: request.signal }).catch(() => null),
  ]);
  const workload = Object.fromEntries(countWorkload(tasks.items));
  return {
    org,
    genres: (config?.genres ?? []).map((g) => g.id),
    workload,
    tasksByAssignee: Object.fromEntries(tasksByAssignee(tasks.items)),
    standingRules: standingRules.items,
    projects: projects.items,
    memory: memoryResult.memory,
    memoryUnavailable: memoryResult.unavailable,
    skills,
  };
}

export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<OrgData> {
  try {
    return await loadOrg(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "組織 - Celeris" }];
}

/**
 * 追加・編集・削除（すべて管理系。ADR-0033 D1）と、skill の mount / unmount（ADR-0056 D3 続き、
 * §3.116〜3.117。Phase 82 / G35）。GUI 側では判断しない: フォームの `intent` を写すだけ。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getCelerisClient();
  const id = formString(form, "id") ?? "";

  let outcome: OrgOpOutcome | OrgSkillMountOutcome;
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
    case "skill_mount":
      outcome = await mountSkill(client, id, formString(form, "skill") ?? "", request.signal);
      break;
    case "skill_unmount":
      outcome = await unmountSkill(client, id, formString(form, "skill") ?? "", request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

const ORG_KINDS: OrgKind[] = ["secretary", "department", "section"];

/** 役職の種類の日本語（画面には英語の `kind` を出さない。監査 4/5）。 */
const ORG_KIND_LABEL: Record<OrgKind, string> = { secretary: "CoS", department: "部", section: "課" };

export default function OrgPage({ loaderData }: Route.ComponentProps) {
  const {
    org,
    genres,
    workload,
    tasksByAssignee: workByAssignee,
    standingRules,
    projects,
    memory,
    memoryUnavailable,
    skills,
  } = loaderData;
  const [searchParams] = useSearchParams();
  const selectedId = searchParams.get("selected");
  const selectedProjectId = searchParams.get("project") ?? "";
  const { roots } = useMemo(() => buildOrgTree(org.items), [org.items]);
  // フェーズ 72（ADR-0055 D2 ラウンド 4）: ノードカードに「既定のハーネス」と動かす場所（一語のバッジ）
  // を添える。値は `GET /org` の `effective_profiles[]`（根→葉で継いだ結果。GUI は継承を再計算しない）。
  const effectiveProfileById = useMemo(
    () => Object.fromEntries((org.effective_profiles ?? []).map((p) => [p.node_id, p])),
    [org.effective_profiles],
  );
  const selected = selectedId ? (org.items.find((n) => n.id === selectedId) ?? null) : null;
  const fetcher = useFetcher<OrgOpOutcome>();
  const submitting = fetcher.state !== "idle";
  // ADR-0056 D3 続き（Phase 82 / G35）: mount / unmount は名前空間が違う結果（`OrgSkillMountOutcome`）
  // なので、他の編集フォーム（作成・削除・profile 編集）と `fetcher.data` を混ぜないよう別の fetcher にする。
  const skillsFetcher = useFetcher<OrgSkillMountOutcome>({ key: "org-skills" });
  const skillsSubmitting = skillsFetcher.state !== "idle";

  const nodeTasks = selected ? (workByAssignee[selected.id] ?? []) : [];

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
        description="Chief of Staff（CoS）を根にした、たった一つの組織です。誰が何を抱えているかを見て、担当を選ぶとその担当に直接話せます。"
      />

      <OrgActionFlash outcome={fetcher.data} />

      <section aria-labelledby="org-heading" data-testid="org-section" className="grid gap-6 lg:grid-cols-[26rem_1fr]">
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
                    <OrgTreeItem
                      key={r.node.id}
                      item={r}
                      depth={0}
                      selectedId={selectedId}
                      workload={workload}
                      effectiveProfileById={effectiveProfileById}
                    />
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
                effectiveProfile={org.effective_profiles?.find((p) => p.node_id === selected.id)}
                leadSession={org.lead_sessions?.find((s) => s.node_id === selected.id)}
                workload={workload[selected.id]}
                tasks={nodeTasks}
                standingRules={standingRules}
                projects={projects}
                selectedProjectId={selectedProjectId}
                memory={memory}
                memoryUnavailable={memoryUnavailable}
                fetcher={fetcher}
                submitting={submitting}
                skills={skills}
                skillsFetcher={skillsFetcher}
                skillsSubmitting={skillsSubmitting}
              />
            ) : (
              <CardBody>
                <EmptyState icon="user" title="担当を選んでください">
                  組織の木から 1 人選ぶと、その担当の一言・抱えている仕事・話す導線が出ます。
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
            title="新しい役職"
            description="部・課を足します（役職は途中で足す・分ける・消すことができます）。"
          />
          <CardBody>
            <fetcher.Form method="post" data-testid="org-add-form" className="grid grid-cols-2 gap-4 sm:grid-cols-3">
              <input type="hidden" name="intent" value="org_create" />
              <div>
                <label htmlFor="org-add-id" className={labelClass}>
                  識別子
                </label>
                <input
                  id="org-add-id"
                  name="id"
                  type="text"
                  data-testid="org-add-id"
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
                <p className={hintClass}>英小文字とハイフン（例: coding-poc）。後から変えられません。</p>
              </div>
              <div>
                <label htmlFor="org-add-name" className={labelClass}>
                  名前
                </label>
                <input id="org-add-name" name="name" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
              </div>
              <div>
                <label htmlFor="org-add-kind" className={labelClass}>
                  種類
                </label>
                <select
                  id="org-add-kind"
                  name="kind"
                  defaultValue="section"
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  {ORG_KINDS.map((k) => (
                    <option key={k} value={k}>
                      {ORG_KIND_LABEL[k]}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="org-add-parent" className={labelClass}>
                  上の担当
                </label>
                <select
                  id="org-add-parent"
                  name="parent_id"
                  defaultValue=""
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  <option value="">（なし・いちばん上）</option>
                  {org.items.map((n) => (
                    <option key={n.id} value={n.id}>
                      {n.name}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="org-add-genre" className={labelClass}>
                  分野
                </label>
                {genres.length > 0 ? (
                  <select id="org-add-genre" name="genre" defaultValue="" className={cn(selectClass, "mt-1.5 w-full")}>
                    <option value="">（なし）</option>
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
                  並び順
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
                  一言
                </label>
                <input id="org-add-brief" name="brief" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
                <p className={hintClass}>その担当の役目を一言で（仕事を頼むときに毎回前置きされます）。</p>
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
  effectiveProfileById,
}: {
  item: OrgTreeNode;
  depth: number;
  selectedId: string | null;
  workload: Record<string, Workload>;
  effectiveProfileById: Record<string, EffectiveProfile>;
}) {
  const { node, children } = item;
  const active = node.id === selectedId;
  const open = workload[node.id]?.open ?? 0;
  const hasChildren = children.length > 0;
  // フェーズ 72（U10 系譜、ADR-0055 D2 ラウンド 4）: 木を「サブツリーごとに開閉できる、字下げした一覧」に
  // した（深い組織で 1 画面が長くなりすぎないため）。既定は開いた状態（挙動を変えない）。
  const [expanded, setExpanded] = useState(true);
  const profile = effectiveProfileById[node.id];
  const harnessDefault = profile?.harness_default;
  const mode = profile?.run;

  // P-G46-3（Phase 96、ADR-0055 D2 ラウンド 20）: 深い組織（depth 4〜5、`coding-poc-alpha-1-x` 等）で
  // 字下げが線形（depth * 14px）に増え続けると、狭いスマホ幅では最深ノードの名前を置く余白が足りず
  // 折り返っていた。木の表示ロジック（開閉・入れ子の構造）自体は変えず、字下げの伸び方だけ depth 3 以降は
  // 半分（7px刻み）に緩める。
  const indentStep = 14;
  const indentTaperDepth = 3;
  const marginLeft =
    depth <= indentTaperDepth
      ? depth * indentStep
      : indentTaperDepth * indentStep + (depth - indentTaperDepth) * (indentStep / 2);

  return (
    <li>
      <div className="flex items-center gap-0.5" style={{ marginLeft }}>
        {hasChildren ? (
          <button
            type="button"
            aria-expanded={expanded}
            aria-controls={`org-subtree-${node.id}`}
            aria-label={expanded ? `${node.name} の下を畳む` : `${node.name} の下を開く`}
            onClick={() => setExpanded((v) => !v)}
            data-testid="org-node-toggle"
            className="flex size-11 shrink-0 items-center justify-center rounded-lg text-fg-subtle hover:bg-surface-2 lg:size-7"
          >
            <Icon name={expanded ? "chevronDown" : "chevronRight"} className="size-4" />
          </button>
        ) : (
          <span className="size-11 shrink-0 lg:size-7" aria-hidden="true" />
        )}
        <Link
          to={`/org?selected=${encodeURIComponent(node.id)}`}
          data-testid="org-node"
          data-org-id={node.id}
          className={cn(
            // ADR-0055 D1-2: タップ領域 44×44 以上。
            "flex min-h-11 min-w-0 flex-1 items-center gap-2 rounded-lg px-2 py-1.5 text-sm no-underline transition-colors",
            active ? "bg-primary-soft text-primary-soft-fg" : "text-fg hover:bg-surface-2",
          )}
        >
          {/* 「人」に見せる（監査 4）: 名前を先頭に太く、その下に一言、右端に小さく分野。
              部・課の英語のバッジは出さない（木の形で分かる。必要な 1 文字だけ添える）。
              ADR-0055 D1-4: 小さい注記はモバイル text-sm、デスクトップは lg: で元の大きさのまま。 */}
          <span className="min-w-0 flex-1">
            <span className="flex min-w-0 flex-wrap items-baseline gap-1.5">
              {/* P-G46-3: 深い階層で字下げが積み重なると幅が足りず名前が折り返っていたので、
                  最小幅を確保できないときは省略して全文は title（長押し/ホバー）に回す。 */}
              <span className="min-w-0 truncate font-semibold" data-testid="org-node-name" title={node.name}>
                {node.name}
              </span>
              {orgKindMark(node.kind) && (
                <span className="text-sm text-fg-subtle lg:text-[0.7rem]">{orgKindMark(node.kind)}</span>
              )}
              {mode && (
                <Badge tone="neutral" data-status-badge="org-mode" data-testid="org-node-mode">
                  {profileRunLabel(mode)}
                </Badge>
              )}
            </span>
            {node.brief && (
              <span className="mt-0.5 line-clamp-1 text-sm text-fg-muted lg:text-xs" data-testid="org-node-brief">
                {node.brief}
              </span>
            )}
            {harnessDefault && (
              <span
                className="mt-0.5 block truncate text-sm text-fg-subtle lg:text-[0.7rem]"
                data-testid="org-node-harness-default"
                title={`既定のハーネス: ${harnessDefault}`}
              >
                既定: {harnessDefault}
              </span>
            )}
          </span>
          {node.genre && (
            <span className="mt-0.5 shrink-0 text-sm text-fg-subtle lg:text-[0.7rem]" data-testid="org-node-genre">
              {node.genre}
            </span>
          )}
          <span
            className="mt-0.5 shrink-0 rounded-full bg-surface-2 px-1.5 py-0.5 text-sm tabular-nums text-fg-subtle lg:text-[0.7rem]"
            title="抱えている仕事の数"
          >
            {open}
          </span>
        </Link>
      </div>
      {hasChildren && expanded && (
        <ul id={`org-subtree-${node.id}`} className="mt-1 space-y-1 border-l border-border pl-2">
          {children.map((c) => (
            <OrgTreeItem
              key={c.node.id}
              item={c}
              depth={depth + 1}
              selectedId={selectedId}
              workload={workload}
              effectiveProfileById={effectiveProfileById}
            />
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
  effectiveProfile,
  leadSession,
  workload,
  tasks,
  standingRules,
  projects,
  selectedProjectId,
  memory,
  memoryUnavailable,
  fetcher,
  submitting,
  skills,
  skillsFetcher,
  skillsSubmitting,
}: {
  node: OrgNode;
  org: OrgNode[];
  genres: string[];
  effectiveProfile: EffectiveProfile | undefined;
  /** ADR-0054 D3（Phase 68）: 部門長（`kind = "department"`）の継続セッション。無ければ `undefined`。 */
  leadSession: NodeSessionSummary | undefined;
  workload: Workload | undefined;
  tasks: TaskSummary[];
  standingRules: StandingRule[];
  projects: Project[];
  selectedProjectId: string;
  memory: MemoryView | null;
  memoryUnavailable: boolean;
  fetcher: FetcherWithComponents<OrgOpOutcome>;
  submitting: boolean;
  /** ADR-0056 D3 続き（Phase 82 / G35）: mount の picker の選択肢（`GET /skills`）。読めなければ `null`。 */
  skills: SkillList | null;
  skillsFetcher: FetcherWithComponents<OrgSkillMountOutcome>;
  skillsSubmitting: boolean;
}) {
  return (
    <div data-testid="org-node-detail">
      <CardHeader
        icon="user"
        title={<span className="text-base font-semibold text-fg">{node.name}</span>}
        description={node.brief || undefined}
        actions={
          node.genre ? (
            <span className="text-xs text-fg-subtle" data-testid="org-node-detail-genre">
              {node.genre}
            </span>
          ) : undefined
        }
      />
      <CardBody className="space-y-4">
        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm">
          <DataItem label="抱えている仕事（未終了）">
            <span data-testid="org-node-open-count" className="tabular-nums">
              {workload?.open ?? 0}
            </span>
          </DataItem>
          <DataItem label="担当した仕事（累計）">
            <span className="tabular-nums">{workload?.total ?? 0}</span>
          </DataItem>
        </dl>

        {/* ADR-0054 D3（Phase 68）: 部門長（レビュー・切り分け run。ADR-0051）の継続セッション。
            無いノード（部門長でない・まだ 1 度もレビューしていない）には出さない。フェーズ 73
            （ADR-0055 D2 ラウンド 5）: 393px でも窮屈にならないよう、上の 2 列の `dl` から出して
            独立した小さいカードにした（3 つの値を `flex-wrap` で並べ、折り返しても横はみ出しない）。 */}
        {leadSession && (
          <div data-testid="org-node-lead-session" className="rounded-lg border border-border bg-surface-2/40 p-3">
            <p className={labelClass}>継続中のセッション</p>
            <dl className="mt-1.5 flex flex-wrap gap-x-4 gap-y-2 text-sm">
              <div className="min-w-0">
                <dt className="text-sm text-fg-subtle lg:text-xs">turns</dt>
                <dd className="tabular-nums" data-testid="org-node-lead-session-turns">
                  {leadSession.turns}
                </dd>
              </div>
              <div className="min-w-0">
                <dt className="text-sm text-fg-subtle lg:text-xs">tokens</dt>
                <dd className="tabular-nums" data-testid="org-node-lead-session-tokens">
                  {leadSession.approx_tokens.toLocaleString("ja-JP")}
                </dd>
              </div>
              <div className="min-w-0 flex-1">
                <dt className="text-sm text-fg-subtle lg:text-xs">最終使用</dt>
                <dd className="break-words font-mono text-xs" data-testid="org-node-lead-session-last-used">
                  {leadSession.last_used_at}
                </dd>
              </div>
            </dl>
          </div>
        )}

        {/* SPEC §4 の 2「ノードを選ぶとその『人』に直接話せる」（Phase G13b-2）。 */}
        <Link
          to={node.id === "secretary" ? "/org/secretary" : `/org/${encodeURIComponent(node.id)}`}
          data-testid="org-talk"
          className={buttonClass({ variant: "secondary", size: "sm" })}
        >
          <Icon name="message" />
          話す
        </Link>

        <div>
          <p className={labelClass}>抱えている仕事</p>
          {/* 対話用タスク（`conversation`）は含まない（GUI-R3、Phase 27。SPEC「タスクは裏方」）。
              `TaskSummary` には `project_id` が無いため、案件名は添えられない（celeris-requests.md R3）。 */}
          {tasks.length === 0 ? (
            <p className={cn(hintClass, "mt-1")}>今のところありません。</p>
          ) : (
            <ul className="mt-1.5 space-y-1" data-testid="org-node-tasks">
              {tasks.slice(0, 20).map((t) => (
                <li key={t.id} className="flex items-center gap-2 text-sm">
                  <Badge tone={statusTone(t.status)} dot>
                    {taskStatusLabel(t.status)}
                  </Badge>
                  <Link to={`/tasks/${t.id}`} className="min-w-0 flex-1 truncate underline underline-offset-2">
                    {t.title}
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </div>

        {/* 記憶（SPEC §3.2「各担当は長期記憶を持ち、記憶は案件をまたぐ」。ADR-0033 D6、§3.62）。
            読み取り専用: 直したいときは `notes_path` のファイルを直接編集する（書き込み API は無い）。 */}
        <div data-testid="org-node-memory">
          <p className={labelClass}>覚えていること（案件をまたぐ）</p>
          {memoryUnavailable ? (
            <p className={cn(hintClass, "mt-1")} data-testid="org-node-memory-unavailable">
              記憶の置き場所が設定されていません（celeris の <code>[memory]</code> を設定すると、この担当が
              案件をまたいで覚えたことをここで読めます）。
            </p>
          ) : memory === null ? (
            <p className={cn(hintClass, "mt-1")}>まだ何も覚えていません。</p>
          ) : (
            <>
              {memory.notes.trim().length > 0 ? (
                <div className="mt-1.5 rounded-lg border border-border bg-surface-2/40 p-3 text-sm">
                  <MarkdownViewer content={memory.notes} />
                </div>
              ) : (
                <p className={cn(hintClass, "mt-1")}>まだ何も覚えていません。</p>
              )}
              <p className={cn(hintClass, "mt-1")}>
                直すならこのファイル: <span className="font-mono break-all">{memory.notes_path}</span>
              </p>

              <p className={cn(labelClass, "mt-4")}>この案件の引き出し</p>
              <Form method="get" className="mt-1.5 flex flex-wrap items-end gap-2">
                <input type="hidden" name="selected" value={node.id} />
                <select
                  name="project"
                  aria-label="記憶を見る案件"
                  data-testid="org-node-memory-project"
                  defaultValue={selectedProjectId}
                  className={cn(selectClass, "h-8 max-w-xs text-xs")}
                >
                  <option value="">案件を選ぶ</option>
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.title}
                    </option>
                  ))}
                </select>
                <Button type="submit" variant="secondary" size="xs">
                  <Icon name="refresh" />
                  表示
                </Button>
              </Form>
              {selectedProjectId === "" ? (
                <p className={cn(hintClass, "mt-1")}>案件を選ぶと、その案件について覚えていることが出ます。</p>
              ) : (memory.project ?? "").trim().length > 0 ? (
                <div
                  className="mt-1.5 rounded-lg border border-border bg-surface-2/40 p-3 text-sm"
                  data-testid="org-node-memory-project-body"
                >
                  <MarkdownViewer content={memory.project ?? ""} />
                  <p className={cn(hintClass, "mt-2")}>
                    直すならこのファイル: <span className="font-mono break-all">{memory.project_path}</span>
                  </p>
                </div>
              ) : (
                <p className={cn(hintClass, "mt-1")}>この案件について覚えていることはまだありません。</p>
              )}
            </>
          )}
        </div>

        <div>
          <p className={labelClass}>この担当への永続の認可</p>
          {/* SPEC §3.6「永続の認可は文字で記録してエージェントに注入する」。ADR-0033 D5、docs/celeris-api-v1.md
              §3.58「node を書けば全員向け + そのノード向け」。追加・削除は `/approvals` から行う。 */}
          {standingRules.length === 0 ? (
            <p className={cn(hintClass, "mt-1")}>今のところありません。</p>
          ) : (
            <ul className="mt-1.5 space-y-1" data-testid="org-node-standing-rules">
              {standingRules.map((r) => (
                <li
                  key={r.id}
                  data-testid="org-node-standing-rule"
                  data-rule-id={r.id}
                  className="flex items-start gap-2 text-sm"
                >
                  <Badge tone={r.node_id ? "neutral" : "teal"}>{standingRuleTargetName(r, org)}</Badge>
                  <span className="min-w-0 flex-1">{r.rule}</span>
                </li>
              ))}
            </ul>
          )}
          <Link
            to="/approvals"
            className={cn(touchLinkClass, "mt-1.5 text-sm text-fg-subtle underline underline-offset-2 hover:text-fg")}
          >
            追加・削除は「認可」から
          </Link>
        </div>

        <MountedSkillsSection
          node={node}
          effectiveProfile={effectiveProfile}
          skills={skills}
          fetcher={skillsFetcher}
          submitting={skillsSubmitting}
        />

        <EffectiveProfileView profile={effectiveProfile} />

        <details className="group">
          <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-border bg-surface px-3 text-sm text-fg shadow-xs hover:bg-surface-2">
            <Icon name="sparkles" className="size-4" />
            profile を編集
          </summary>
          <ProfileEditForm key={node.id} node={node} genres={genres} fetcher={fetcher} submitting={submitting} />
        </details>

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
                  名前
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
                  種類
                </label>
                <select
                  id={`org-edit-kind-${node.id}`}
                  name="kind"
                  defaultValue={node.kind}
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  {ORG_KINDS.map((k) => (
                    <option key={k} value={k}>
                      {ORG_KIND_LABEL[k]}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label className={labelClass} htmlFor={`org-edit-parent-${node.id}`}>
                  上の担当
                </label>
                <select
                  id={`org-edit-parent-${node.id}`}
                  name="parent_id"
                  defaultValue={node.parent_id ?? ""}
                  className={cn(selectClass, "mt-1.5 w-full")}
                >
                  <option value="">（なし・いちばん上）</option>
                  {org
                    .filter((n) => n.id !== node.id)
                    .map((n) => (
                      <option key={n.id} value={n.id}>
                        {n.name}
                      </option>
                    ))}
                </select>
              </div>
              <div>
                <label className={labelClass} htmlFor={`org-edit-genre-${node.id}`}>
                  分野
                </label>
                {genres.length > 0 ? (
                  <select
                    id={`org-edit-genre-${node.id}`}
                    name="genre"
                    defaultValue={node.genre ?? ""}
                    className={cn(selectClass, "mt-1.5 w-full")}
                  >
                    <option value="">（なし）</option>
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
                  一言
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
                  並び順
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
              本当に「{node.name}」を削除しますか？ 仕事を抱えている・下に担当がいると断られます。
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

/** そのノードの `knowledge[]` を人が読む 1 行に（`profile-admin` の `label()` と同じ考え方。表示だけ）。 */
function knowledgeMountLine(m: {
  kind: string;
  scope?: string | null;
  name?: string | null;
  path?: string | null;
  docs?: string | null;
}) {
  const parts = [m.scope, m.name, m.path].filter((p): p is string => Boolean(p));
  const head = parts.length > 0 ? `${knowledgeKindLabel(m.kind)}: ${parts.join(":")}` : knowledgeKindLabel(m.kind);
  return m.docs ? `${head}（docs: ${m.docs}）` : head;
}

/**
 * ADR-0056 D3 続き（skills を GUI から見る・mount する。Phase 82 / G35）: そのノードが mount している
 * skill（own = `node.profile.skills_mounts`）と、親から継いだもの（inherited = `effectiveProfile` に
 * あって own に無いもの）を分けて見せる。own はここで外せる（`DELETE /org/{id}/skills/{skill}`）が、
 * inherited はそのノードでは外せない（ADR-0056 D3「mount が門」。親のノードで外す）。picker は
 * `GET /skills` の一覧から選ぶ（KB に実在するかは celeris 側で検査しない仕様だが、GUI は実在するものだけ
 * 選ばせる）。own/inherited への分割そのものは `~/lib/skills.ts::splitMountedSkills`（純粋関数）。
 */
function MountedSkillsSection({
  node,
  effectiveProfile,
  skills,
  fetcher,
  submitting,
}: {
  node: OrgNode;
  effectiveProfile: EffectiveProfile | undefined;
  skills: SkillList | null;
  fetcher: FetcherWithComponents<OrgSkillMountOutcome>;
  submitting: boolean;
}) {
  const own = node.profile?.skills_mounts ?? [];
  const effective = effectiveProfile?.skills_mounts ?? [];
  const { inherited } = splitMountedSkills(own, effective);
  const items = skills?.items ?? [];
  const descriptionOf = (name: string) => items.find((i) => i.name === name)?.description;
  const options = items.filter((i) => !own.includes(i.name));

  return (
    <div data-testid="org-node-skills">
      <p className={labelClass}>mount された skills</p>
      {own.length === 0 && inherited.length === 0 ? (
        <p className={cn(hintClass, "mt-1")} data-testid="org-node-skills-empty">
          まだ mount していません。
        </p>
      ) : (
        <ul className="mt-1.5 space-y-1.5" data-testid="org-node-skills-list">
          {own.map((name) => (
            <li
              key={`own:${name}`}
              className="flex flex-wrap items-center gap-2 text-sm"
              data-testid="org-node-skill-own"
            >
              <Badge tone="teal">{name}</Badge>
              {/* P-G46-6（Phase 96）: `truncate` を `<Link>`（`flex` コンテナ自身）に直接付けていたため、
                  ブラウザが省略記号（…）を出せず、文字の途中でただ切れて読みにくかった
                  （`text-overflow: ellipsis` はブロック化された要素には効くが、flex コンテナ自身の
                  内容には効かない）。省略は内側の `<span>`（flex item として自動でブロック化される）に
                  持たせ、全文は `title` で参照できるようにした（ADR-0055 D2 の id/パス省略と同じ規律）。 */}
              <Link
                to={`/knowledge/skills?name=${encodeURIComponent(name)}`}
                className="flex min-h-11 min-w-0 flex-1 items-center underline underline-offset-2"
                title={descriptionOf(name) || name}
              >
                <span className="min-w-0 flex-1 truncate">{descriptionOf(name) || name}</span>
              </Link>
              <fetcher.Form method="post">
                <input type="hidden" name="intent" value="skill_unmount" />
                <input type="hidden" name="id" value={node.id} />
                <input type="hidden" name="skill" value={name} />
                <Button
                  type="submit"
                  variant="ghost"
                  size="sm"
                  disabled={submitting}
                  data-testid="org-node-skill-unmount"
                >
                  外す
                </Button>
              </fetcher.Form>
            </li>
          ))}
          {inherited.map((name) => (
            <li
              key={`inherited:${name}`}
              className="flex flex-wrap items-center gap-2 text-sm"
              data-testid="org-node-skill-inherited"
            >
              <Badge tone="neutral">{name}</Badge>
              <span
                className="text-sm text-fg-subtle lg:text-xs"
                title="親から継いだ mount（このノードでは外せません）"
              >
                継承
              </span>
              {descriptionOf(name) && (
                <span className="min-w-0 flex-1 truncate text-fg-subtle">{descriptionOf(name)}</span>
              )}
            </li>
          ))}
        </ul>
      )}

      {skills === null ? (
        <p className={cn(hintClass, "mt-2")} data-testid="org-node-skills-unavailable">
          skill の一覧を読めませんでした（<Mono>[knowledge] root</Mono> が未設定かもしれません）。
        </p>
      ) : items.length === 0 ? (
        <p className={cn(hintClass, "mt-2")} data-testid="org-node-skills-none">
          まだ skill がありません（
          <Link to="/knowledge/skills" className={cn(touchLinkClass, "underline underline-offset-2")}>
            skills へ
          </Link>
          ）。
        </p>
      ) : (
        <fetcher.Form method="post" className="mt-2 flex flex-wrap items-end gap-2">
          <input type="hidden" name="intent" value="skill_mount" />
          <input type="hidden" name="id" value={node.id} />
          <select
            name="skill"
            aria-label="mount する skill"
            required
            defaultValue=""
            className={cn(selectClass, "max-w-xs")}
            data-testid="org-node-skill-select"
          >
            <option value="" disabled>
              skill を選ぶ
            </option>
            {options.map((item) => (
              <option key={item.name} value={item.name}>
                {item.name}
              </option>
            ))}
          </select>
          <Button
            type="submit"
            variant="secondary"
            size="xs"
            disabled={submitting || options.length === 0}
            data-testid="org-node-skill-mount-submit"
          >
            <Icon name="plus" />
            mount
          </Button>
          {options.length === 0 && (
            <span className="text-sm text-fg-subtle lg:text-xs">選べる skill はもうありません。</span>
          )}
        </fetcher.Form>
      )}

      {fetcher.data && !fetcher.data.ok && (
        <div className="mt-2" data-testid="org-node-skills-error">
          <ErrorFlash error={fetcher.data.error} />
        </div>
      )}
    </div>
  );
}

/**
 * ADR-0046 D1（Phase 59 / G21）: 実効 profile（根→葉で継いだ結果）の読み取り専用の表示。
 * `GET /org` の `effective_profiles[]` をそのまま出す（GUI は継承を再計算しない。§4-1）。
 */
function EffectiveProfileView({ profile }: { profile: EffectiveProfile | undefined }) {
  const isTrivial =
    !profile ||
    ((profile.skills?.length ?? 0) === 0 &&
      (profile.knowledge?.length ?? 0) === 0 &&
      (profile.harnesses_allowed?.length ?? 0) === 0 &&
      !profile.harness_default &&
      (profile.tools?.length ?? 0) === 0 &&
      (profile.deny_tools?.length ?? 0) === 0 &&
      !profile.run &&
      !profile.tier &&
      (profile.policy?.length ?? 0) === 0 &&
      !profile.review_harness &&
      !profile.review_tier &&
      (profile.approvals?.length ?? 0) === 0);

  return (
    <div data-testid="org-node-profile">
      <p className={labelClass}>実効 profile（組織の木から継いだもの）</p>
      {isTrivial || !profile ? (
        <p className={cn(hintClass, "mt-1")} data-testid="org-node-profile-empty">
          まだ何も設定していません（下の「profile を編集」から足せます）。
        </p>
      ) : (
        <dl
          className="mt-1.5 grid grid-cols-1 gap-x-4 gap-y-3 text-sm sm:grid-cols-2"
          data-testid="org-node-profile-body"
        >
          {profile.chain && profile.chain.length > 0 && (
            <DataItem label={profileFieldLabel("chain")} wide>
              {profile.chain.join(" → ")}
            </DataItem>
          )}
          {profile.skills && profile.skills.length > 0 && (
            <DataItem label={profileFieldLabel("skills")} wide>
              <div className="flex flex-wrap gap-1">
                {profile.skills.map((s) => (
                  <Badge key={s} tone="neutral">
                    {s}
                  </Badge>
                ))}
              </div>
            </DataItem>
          )}
          {(profile.harnesses_allowed?.length ?? 0) > 0 && (
            <DataItem label={profileFieldLabel("harnesses_allowed")}>
              {profile.harnesses_allowed?.join("、")}
              {profile.harness_default && (
                <span className="ml-1 text-xs text-fg-subtle">（既定: {profile.harness_default}）</span>
              )}
            </DataItem>
          )}
          {!profile.harnesses_allowed?.length && profile.harness_default && (
            <DataItem label={profileFieldLabel("harness_default")}>{profile.harness_default}</DataItem>
          )}
          {profile.knowledge && profile.knowledge.length > 0 && (
            <DataItem label={profileFieldLabel("knowledge")} wide>
              <ul className="space-y-0.5">
                {profile.knowledge.map((k, i) => (
                  // biome-ignore lint/suspicious/noArrayIndexKey: マウントは同じ内容が重複しないので並びで十分
                  <li key={i}>{knowledgeMountLine(k)}</li>
                ))}
              </ul>
            </DataItem>
          )}
          {(profile.tools?.length ?? 0) > 0 && (
            <DataItem label={profileFieldLabel("tools")}>{profile.tools?.join("、")}</DataItem>
          )}
          {(profile.deny_tools?.length ?? 0) > 0 && (
            <DataItem label={profileFieldLabel("deny_tools")}>{profile.deny_tools?.join("、")}</DataItem>
          )}
          {profile.run && <DataItem label={profileFieldLabel("run")}>{profileRunLabel(profile.run)}</DataItem>}
          {profile.tier && (
            <DataItem label={profileFieldLabel("tier")}>
              {tierLabel(profile.tier)}
              {(profile.allowed_tiers?.length ?? 0) > 0 && (
                <span className="ml-1 text-xs text-fg-subtle">
                  （許可: {profile.allowed_tiers?.map(tierLabel).join("、")}）
                </span>
              )}
            </DataItem>
          )}
          {profile.review_harness && (
            <DataItem label={profileFieldLabel("review_harness")}>{profile.review_harness}</DataItem>
          )}
          {profile.review_tier && (
            <DataItem label={profileFieldLabel("review_tier")}>{tierLabel(profile.review_tier)}</DataItem>
          )}
          {(profile.approvals?.length ?? 0) > 0 && (
            <DataItem label={profileFieldLabel("approvals")} wide>
              {profile.approvals?.join("、")}
            </DataItem>
          )}
          {profile.policy && profile.policy.length > 0 && (
            <DataItem label={profileFieldLabel("policy")} wide>
              <ul className="list-disc space-y-0.5 pl-4">
                {profile.policy.map((p, i) => (
                  // biome-ignore lint/suspicious/noArrayIndexKey: 方針は根→葉の連結で重複しうる。並びが意味を持つ
                  <li key={i}>{p}</li>
                ))}
              </ul>
            </DataItem>
          )}
        </dl>
      )}
    </div>
  );
}

/** 知識のマウントを編集する行の数（既存の行 + 追加用の空行）。ADR-0046 D1。 */
const KNOWLEDGE_EXTRA_ROWS = 3;

/**
 * ADR-0046 D1（Phase 59 / G21）: profile の編集フォーム。名前・種類等のフォーム（`org-edit-form`）とは
 * **別に送る**（`~/celeris/org-admin.server.ts` の `buildOrgPatchInput` の規律。片方だけ変えられる）。
 * `profile` は丸ごと差し替えなので、このフォームは常にそのノード自身の `profile`（継ぐ前）を初期値にする
 * （`node.profile`。継いだ後の値は上の「実効 profile」に出るだけで、ここには入れない）。
 */
function ProfileEditForm({
  node,
  genres,
  fetcher,
  submitting,
}: {
  node: OrgNode;
  genres: string[];
  fetcher: FetcherWithComponents<OrgOpOutcome>;
  submitting: boolean;
}) {
  const profile: Profile = node.profile ?? {};
  const harnesses = harnessOptions(genres);
  const knowledgeRows = [...(profile.knowledge ?? []), ...Array(KNOWLEDGE_EXTRA_ROWS).fill(null)].slice(
    0,
    Math.max((profile.knowledge?.length ?? 0) + KNOWLEDGE_EXTRA_ROWS, KNOWLEDGE_EXTRA_ROWS),
  );
  const uid = (suffix: string) => `profile-${node.id}-${suffix}`;

  return (
    <fetcher.Form
      method="post"
      data-testid="org-profile-edit-form"
      className="mt-3 space-y-4 rounded-lg border border-border bg-surface-2/40 p-3"
    >
      <input type="hidden" name="intent" value="org_patch" />
      <input type="hidden" name="id" value={node.id} />
      {/* 丸ごと差し替え（§3.44）の印。これが無ければ `profile` は本文に入らず、今の値のまま。 */}
      <input type="hidden" name="profile_present" value="1" />

      <div>
        <label className={labelClass} htmlFor={uid("skills")}>
          能力タグ（skills）
        </label>
        <input
          id={uid("skills")}
          name="profile_skills"
          type="text"
          defaultValue={profile.skills?.join(", ") ?? ""}
          className={cn(inputClass, "mt-1.5 w-full")}
        />
        <p className={hintClass}>空白かカンマ区切り（例: rust, sqlite）。小文字の `[a-z0-9._-]` だけ。</p>
      </div>

      <div>
        <p className={labelClass}>受けられるハーネス（harnesses.allowed）</p>
        <div className="mt-1.5 flex flex-wrap gap-1.5">
          {harnesses.map((h) => (
            <label key={h} className={chipLabelClass}>
              <input
                type="checkbox"
                name="profile_harnesses_allowed"
                value={h}
                defaultChecked={profile.harnesses?.allowed?.includes(h) ?? false}
                className={checkboxClass}
              />
              {h}
            </label>
          ))}
        </div>
        <label className={cn(labelClass, "mt-3 block")} htmlFor={uid("harness-default")}>
          既定のハーネス（harnesses.default）
        </label>
        <select
          id={uid("harness-default")}
          name="profile_harness_default"
          defaultValue={profile.harnesses?.default ?? ""}
          className={cn(selectClass, "mt-1.5 w-full sm:w-64")}
        >
          <option value="">（子が決める・無し）</option>
          {harnesses.map((h) => (
            <option key={h} value={h}>
              {h}
            </option>
          ))}
        </select>
      </div>

      <div>
        <p className={labelClass}>使ってよい道具（tools）</p>
        <div className="mt-1.5 flex flex-wrap gap-1.5">
          {PROFILE_TOOLS.map((t) => (
            <label key={t} className={chipLabelClass}>
              <input
                type="checkbox"
                name="profile_tools"
                value={t}
                defaultChecked={profile.tools?.includes(t) ?? false}
                className={checkboxClass}
              />
              {t}
            </label>
          ))}
        </div>
        <input
          name="profile_tools_extra"
          type="text"
          defaultValue={
            profile.tools?.filter((t) => !(PROFILE_TOOLS as readonly string[]).includes(t)).join(", ") ?? ""
          }
          placeholder="cluster:pegasus"
          className={cn(inputClass, "mt-1.5 w-full")}
        />
        <p className={hintClass}>{PROFILE_TOOLS_EXTRA_HINT}</p>
      </div>

      <div>
        <p className={labelClass}>禁止する道具（deny_tools。常に勝つ）</p>
        <div className="mt-1.5 flex flex-wrap gap-1.5">
          {PROFILE_TOOLS.map((t) => (
            <label key={t} className={chipLabelClass}>
              <input
                type="checkbox"
                name="profile_deny_tools"
                value={t}
                defaultChecked={profile.deny_tools?.includes(t) ?? false}
                className={checkboxClass}
              />
              {t}
            </label>
          ))}
        </div>
        <input
          name="profile_deny_tools_extra"
          type="text"
          defaultValue={
            profile.deny_tools?.filter((t) => !(PROFILE_TOOLS as readonly string[]).includes(t)).join(", ") ?? ""
          }
          placeholder="cluster:pegasus"
          className={cn(inputClass, "mt-1.5 w-full")}
        />
      </div>

      <div>
        <p className={labelClass}>知識（knowledge）</p>
        <div className="mt-1.5 space-y-2">
          {knowledgeRows.map((row, i) => (
            <div
              // biome-ignore lint/suspicious/noArrayIndexKey: 行は固定数の並行配列で並びに意味がある
              key={i}
              className="grid grid-cols-2 gap-2 rounded-lg border border-border bg-surface p-2 sm:grid-cols-5"
            >
              <select
                name="profile_knowledge_kind"
                defaultValue={row?.kind ?? ""}
                aria-label="種類"
                className={cn(selectClass, "h-8 text-xs")}
              >
                <option value="">（未使用）</option>
                {KNOWLEDGE_KINDS.map((k) => (
                  <option key={k} value={k}>
                    {knowledgeKindLabel(k)}
                  </option>
                ))}
              </select>
              <input
                name="profile_knowledge_scope"
                type="text"
                defaultValue={row?.scope ?? ""}
                placeholder="scope（kb 用）"
                aria-label="scope"
                className={cn(inputClass, "h-8 text-xs")}
              />
              <input
                name="profile_knowledge_name"
                type="text"
                defaultValue={row?.name ?? ""}
                placeholder="name（repo/memory 用）"
                aria-label="name"
                className={cn(inputClass, "h-8 text-xs")}
              />
              <input
                name="profile_knowledge_path"
                type="text"
                defaultValue={row?.path ?? ""}
                placeholder="path"
                aria-label="path"
                className={cn(inputClass, "h-8 text-xs")}
              />
              <input
                name="profile_knowledge_docs"
                type="text"
                defaultValue={row?.docs ?? ""}
                placeholder="docs（repo 用）"
                aria-label="docs"
                className={cn(inputClass, "h-8 text-xs")}
              />
            </div>
          ))}
        </div>
        <p className={hintClass}>種類を選んだ行だけが保存されます（空の行は無視されます）。</p>
      </div>

      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        <div>
          <label className={labelClass} htmlFor={uid("run")}>
            実行場所（run）
          </label>
          <select
            id={uid("run")}
            name="profile_run"
            defaultValue={profile.run ?? ""}
            className={cn(selectClass, "mt-1.5 w-full")}
          >
            <option value="">（子が決める・無し）</option>
            {PROFILE_RUNS.map((r) => (
              <option key={r} value={r}>
                {profileRunLabel(r)}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className={labelClass} htmlFor={uid("model-tier")}>
            モデルの段（tier）
          </label>
          <select
            id={uid("model-tier")}
            name="profile_model_tier"
            defaultValue={profile.model?.tier ?? ""}
            className={cn(selectClass, "mt-1.5 w-full")}
          >
            <option value="">（子が決める・無し）</option>
            {TIERS.map((t) => (
              <option key={t} value={t}>
                {tierLabel(t)}
              </option>
            ))}
          </select>
        </div>
        <div className="col-span-2">
          <p className={labelClass}>許すモデルの段（allowed_tiers。交わり）</p>
          <div className="mt-1.5 flex flex-wrap gap-1.5">
            {TIERS.map((t) => (
              <label key={t} className={chipLabelClass}>
                <input
                  type="checkbox"
                  name="profile_model_allowed_tiers"
                  value={t}
                  defaultChecked={profile.model?.allowed_tiers?.includes(t) ?? false}
                  className={checkboxClass}
                />
                {tierLabel(t)}
              </label>
            ))}
          </div>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4">
        <div>
          <label className={labelClass} htmlFor={uid("review-harness")}>
            レビューのハーネス（review.harness）
          </label>
          <select
            id={uid("review-harness")}
            name="profile_review_harness"
            defaultValue={profile.review?.harness ?? ""}
            className={cn(selectClass, "mt-1.5 w-full")}
          >
            <option value="">（子が決める・無し）</option>
            {harnesses.map((h) => (
              <option key={h} value={h}>
                {h}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className={labelClass} htmlFor={uid("review-tier")}>
            レビューのモデルの段（review.tier）
          </label>
          <select
            id={uid("review-tier")}
            name="profile_review_tier"
            defaultValue={profile.review?.tier ?? ""}
            className={cn(selectClass, "mt-1.5 w-full")}
          >
            <option value="">（子が決める・無し）</option>
            {TIERS.map((t) => (
              <option key={t} value={t}>
                {tierLabel(t)}
              </option>
            ))}
          </select>
        </div>
      </div>

      <div>
        <label className={labelClass} htmlFor={uid("policy")}>
          組織の方針（policy。根→葉の順に連結。1 行 1 件）
        </label>
        <textarea
          id={uid("policy")}
          name="profile_policy"
          rows={3}
          defaultValue={profile.policy?.join("\n") ?? ""}
          className={cn(textareaClass, "mt-1.5 w-full")}
        />
      </div>

      <div>
        <label className={labelClass} htmlFor={uid("approvals")}>
          認可が要る操作（permissions.approvals。1 行 1 件）
        </label>
        <textarea
          id={uid("approvals")}
          name="profile_approvals"
          rows={2}
          defaultValue={profile.permissions?.approvals?.join("\n") ?? ""}
          className={cn(textareaClass, "mt-1.5 w-full")}
        />
      </div>

      <Button type="submit" variant="primary" size="sm" disabled={submitting} data-testid="org-profile-edit-submit">
        <Icon name="check" />
        profile を保存
      </Button>
    </fetcher.Form>
  );
}

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
