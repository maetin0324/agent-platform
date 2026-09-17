import { data, isRouteErrorResponse, Link, useFetcher } from "react-router";
import { ApprovalActionFlash, StandingRuleActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, labelClass, selectClass, textareaClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { DataItem, EmptyState, PageHeader, SectionTitle } from "~/components/ui/misc";
import {
  type ApprovalGroup,
  approvalGroupNodeNames,
  approvalNodeName,
  approvalProjectName,
  groupApprovals,
  standingRuleTargetName,
} from "~/lib/approvals";
import { decisionLabel } from "~/lib/labels";
import { relativeTimeLabel } from "~/lib/reports";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { cn } from "~/lib/utils";
import { TaskdBanner } from "~/root";
import type { ApprovalOpOutcome, StandingRuleOpOutcome } from "~/taskd/action-types";
import {
  buildApprovalDecideInput,
  buildStandingRuleCreateInput,
  createStandingRule,
  decideApproval,
  deleteStandingRule,
} from "~/taskd/approvals-admin.server";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { formString } from "~/taskd/forms";
import type {
  Approval,
  ApprovalList,
  OrgList,
  OrgNode,
  Project,
  ProjectList,
  StandingRule,
  StandingRuleList,
} from "~/taskd/types";
import type { Route } from "./+types/approvals";

/**
 * `/approvals`（認可の要求 + 永続の認可の一覧と編集、SPEC §3.6・§4 の 5、ADR-0033 D5、
 * docs/taskd-api-v1.md §3.56〜3.60。Phase G13d、taskd 側 Phase 26 に追従）。
 * 上に**未決の要求**、下に**決めたもの**の履歴、さらに下に**永続の認可の一覧と編集**
 * （`GET/POST/DELETE /standing-rules`）。案件名・ノード名は `GET /projects` / `GET /org` から解決する
 * （taskd 側に判断値を作らせない。`~/lib/approvals.ts`）。
 * **`GET /approvals?pending=true` / `?pending=false` の 2 回呼び**（Phase 27 で taskd 側が
 * `pending=false` を「決定済みだけ」に絞り込むよう直した。`docs/taskd-requests.md` R5 解決済み）。
 * G13d では実機で `pending=false` が絞り込まないことを確認し、フィルタ無しの 1 回取得 + GUI 側
 * `splitApprovals`（`Approval.decision` の有無で分ける）で回避していたが、taskd 側の絞り込みに戻した
 * （クエリの絞り込みを taskd に任せる方が本来の設計。`splitApprovals` は不要になったので削除した）。
 */

export interface ApprovalsData {
  pending: Approval[];
  decided: Approval[];
  org: OrgNode[];
  projects: Project[];
  standingRules: StandingRule[];
  fetchedAt: string;
}

export async function loadApprovals(client: TaskdClient, request: Request): Promise<ApprovalsData> {
  const [pendingList, decidedList, org, projects, standingRules] = await Promise.all([
    client.get<ApprovalList>("/approvals", { query: { pending: true }, signal: request.signal }),
    client.get<ApprovalList>("/approvals", { query: { pending: false }, signal: request.signal }),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
    client.get<ProjectList>("/projects", { signal: request.signal }).catch(() => ({ items: [] }) as ProjectList),
    client.get<StandingRuleList>("/standing-rules", { signal: request.signal }),
  ]);
  return {
    pending: pendingList.items,
    decided: decidedList.items,
    org: org.items,
    projects: projects.items,
    standingRules: standingRules.items,
    fetchedAt: new Date().toISOString(),
  };
}

export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ApprovalsData> {
  try {
    return await loadApprovals(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "認可 - taskd-gui" }];
}

/**
 * 3 つの intent（すべて管理系。ADR-0033 D5）。GUI 側では判断しない: フォームの値をそのまま taskd に送るだけ。
 * `approval_decide` は 3 つのボタン（`name="decision"`）のどれが押されたかで `once`/`standing`/`denied` が決まる
 * （`app/routes/tasks.$id.tsx` の approve/reject と違い、`answer` の欄を 1 つに共有するため 1 つの `<Form>` にした）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getTaskdClient();

  switch (intent) {
    case "approval_decide": {
      // 同じ文面の未決の要求は 1 枚にまとまっている（監査 8）ので、`id` が複数来ることがある。
      // 受信箱の「この Plan の子を全部受け入れ」と同じく順に送る（原子性は無い）。最初の失敗を返す。
      const ids = form
        .getAll("id")
        .map(String)
        .filter((id) => id.length > 0);
      const input = buildApprovalDecideInput(form);
      let last: ApprovalOpOutcome | null = null;
      for (const id of ids) {
        const outcome = await decideApproval(client, id, input, request.signal);
        if (!outcome.ok) return data(outcome, { status: outcome.error.status });
        last = outcome;
      }
      if (last === null) {
        const outcome = await decideApproval(client, "", input, request.signal);
        return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
      }
      return data(last, { status: 200 });
    }
    case "standing_rule_create": {
      const outcome = await createStandingRule(client, buildStandingRuleCreateInput(form), request.signal);
      return data(outcome, { status: outcome.ok ? 201 : outcome.error.status });
    }
    case "standing_rule_delete": {
      const id = formString(form, "id") ?? "";
      const outcome = await deleteStandingRule(client, id, request.signal);
      return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
    }
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
}

export default function ApprovalsPage({ loaderData }: Route.ComponentProps) {
  const { pending, decided, org, projects, standingRules, fetchedAt } = loaderData;
  const addFetcher = useFetcher<StandingRuleOpOutcome>({ key: "standing-rule-add" });
  const addSubmitting = addFetcher.state !== "idle";

  return (
    <div className="space-y-8">
      <PageHeader
        icon="shield"
        title={
          <>
            認可
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="担当が「少しでも聞くべきだ」と判断したことがここに並びます。「今回だけ」か「今後ずっと」で答えてください。今後ずっとの答えは規則文として記録され、以後その担当に前置きされます。"
      />

      <section aria-labelledby="approvals-heading" data-testid="approvals-section" className="space-y-6">
        <div>
          <SectionTitle icon="shield" id="approvals-heading" count={pending.length} className="mb-3">
            認可待ち
          </SectionTitle>
          {pending.length === 0 ? (
            <EmptyState icon="checkCircle" title="未決の要求はありません" />
          ) : (
            <ul className="space-y-2">
              {groupApprovals(pending).map((group) => (
                <PendingApprovalCard
                  key={group.head.id}
                  group={group}
                  org={org}
                  projects={projects}
                  fetchedAt={fetchedAt}
                />
              ))}
            </ul>
          )}
        </div>

        <div>
          <SectionTitle icon="clock" count={decided.length} className="mb-3">
            決めたもの
          </SectionTitle>
          {decided.length === 0 ? (
            <EmptyState icon="clock" title="まだ決めたものはありません" />
          ) : (
            <ul className="space-y-2">
              {decided.map((a) => (
                <DecidedApprovalRow key={a.id} approval={a} org={org} projects={projects} fetchedAt={fetchedAt} />
              ))}
            </ul>
          )}
        </div>
      </section>

      <section aria-labelledby="standing-rules-heading" data-testid="standing-rules-section" className="space-y-4">
        <SectionTitle icon="lock" id="standing-rules-heading" count={standingRules.length}>
          永続の認可
        </SectionTitle>
        {standingRules.length === 0 ? (
          <EmptyState icon="lock" title="永続の認可はまだありません" />
        ) : (
          <ul className="space-y-2">
            {standingRules.map((r) => (
              <StandingRuleRow key={r.id} rule={r} org={org} />
            ))}
          </ul>
        )}

        <Card>
          <CardHeader
            icon="plus"
            title="永続の認可を追加"
            description="聞かれるのを待たずに、規則文を直接足します。「誰に」を（全員）にすると組織のみんなに効きます。"
          />
          <CardBody>
            <addFetcher.Form method="post" data-testid="standing-rule-add-form" className="space-y-3">
              <input type="hidden" name="intent" value="standing_rule_create" />
              <div>
                <label htmlFor="standing-rule-add-node" className={labelClass}>
                  誰に
                </label>
                <select
                  id="standing-rule-add-node"
                  name="node_id"
                  defaultValue=""
                  className={cn(selectClass, "mt-1.5 w-full max-w-xs")}
                >
                  <option value="">（全員）</option>
                  {org.map((n) => (
                    <option key={n.id} value={n.id}>
                      {n.name}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="standing-rule-add-rule" className={labelClass}>
                  規則文
                </label>
                <textarea
                  id="standing-rule-add-rule"
                  name="rule"
                  rows={2}
                  className={cn(textareaClass, "mt-1.5 w-full")}
                />
                <p className={hintClass}>そのまま担当に前置きされます。</p>
              </div>
              <Button
                type="submit"
                variant="primary"
                size="sm"
                disabled={addSubmitting}
                data-testid="standing-rule-add-submit"
              >
                <Icon name="plus" />
                追加
              </Button>
            </addFetcher.Form>
            <StandingRuleActionFlash outcome={addFetcher.data} />
          </CardBody>
        </Card>
      </section>
    </div>
  );
}

/**
 * 未決の要求 1 枚（監査 8）。同じ文面の要求はここにまとまって「N 件」と出て、答えは全部にまとめて送る。
 * 高さを詰めるため、案内文は畳んで（`<details>`）必要なときだけ開く。
 */
function PendingApprovalCard({
  group,
  org,
  projects,
  fetchedAt,
}: {
  group: ApprovalGroup;
  org: OrgNode[];
  projects: Project[];
  fetchedAt: string;
}) {
  const { head, approvals } = group;
  const fetcher = useFetcher<ApprovalOpOutcome>({ key: `approval-${head.id}` });
  const submitting = fetcher.state !== "idle";
  const nodeNames = approvalGroupNodeNames(group, org);

  return (
    <li
      data-testid="approval-row"
      data-approval-id={head.id}
      data-approval-count={approvals.length}
      className="rounded-xl border border-border bg-surface px-3 py-2.5 shadow-xs"
    >
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-fg-subtle">
        <span className="font-medium text-fg">{nodeNames.join("・")}</span>
        <span>{approvalProjectName(head, projects)}</span>
        {approvals.length > 1 && (
          <Badge tone="warning" data-testid="approval-count">
            {approvals.length} 件
          </Badge>
        )}
        <span className="ml-auto flex items-center gap-3">
          {head.task_id && (
            <Link
              to={`/tasks/${head.task_id}`}
              className="underline underline-offset-2"
              data-testid="approval-task-link"
            >
              裏方のタスク
            </Link>
          )}
          <span>{relativeTimeLabel(head.created_at, fetchedAt)}</span>
        </span>
      </div>

      <div data-testid="approval-question" className="mt-1.5 text-sm">
        <MarkdownViewer content={head.question} />
      </div>

      <fetcher.Form method="post" className="mt-2 space-y-2">
        <input type="hidden" name="intent" value="approval_decide" />
        {approvals.map((a) => (
          <input key={a.id} type="hidden" name="id" value={a.id} />
        ))}
        <textarea
          id={`approval-answer-${head.id}`}
          name="answer"
          aria-label="答え"
          placeholder="答え（「今後ずっと」のときは規則文として書く）"
          data-testid="approval-answer"
          rows={2}
          className={cn(textareaClass, "w-full")}
        />
        <div className="flex flex-wrap items-center gap-2">
          <Button
            type="submit"
            name="decision"
            value="once"
            variant="secondary"
            size="sm"
            disabled={submitting}
            data-testid="approval-once"
          >
            今回だけ
          </Button>
          <Button
            type="submit"
            name="decision"
            value="standing"
            variant="primary"
            size="sm"
            disabled={submitting}
            data-testid="approval-standing"
          >
            今後ずっと
          </Button>
          <Button
            type="submit"
            name="decision"
            value="denied"
            variant="danger"
            size="sm"
            disabled={submitting}
            data-testid="approval-denied"
          >
            認めない
          </Button>
          <label htmlFor={`approval-scope-${head.id}`} className="ml-auto flex items-center gap-1.5 text-xs">
            <span className="text-fg-subtle">「今後ずっと」の範囲</span>
            <select
              id={`approval-scope-${head.id}`}
              name="scope"
              data-testid="approval-scope"
              defaultValue="node"
              className={cn(selectClass, "h-8 w-36 text-xs")}
            >
              <option value="node">この担当だけ</option>
              <option value="all">全員</option>
            </select>
          </label>
        </div>
        <details>
          <summary className={cn(hintClass, "cursor-pointer list-none underline underline-offset-2")}>
            「今後ずっと」の書き方
          </summary>
          <p className={hintClass}>
            規則文として書いてください（例:
            クラスタへの実験投入は毎回聞かずに進めてよい）。そのまま担当に前置きされます。
          </p>
        </details>
      </fetcher.Form>
      <ApprovalActionFlash outcome={fetcher.data} />
    </li>
  );
}

/** 決めたものの履歴 1 行（高さを詰めた読み取り専用の行）。 */
function DecidedApprovalRow({
  approval,
  org,
  projects,
  fetchedAt,
}: {
  approval: Approval;
  org: OrgNode[];
  projects: Project[];
  fetchedAt: string;
}) {
  return (
    <li
      data-testid="approval-row"
      data-approval-id={approval.id}
      className="rounded-xl border border-border bg-surface px-3 py-2.5 shadow-xs"
    >
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-fg-subtle">
        <span className="font-medium text-fg">{approvalNodeName(approval, org)}</span>
        <span>{approvalProjectName(approval, projects)}</span>
        {approval.decision && <Badge tone="neutral">{decisionLabel(approval.decision)}</Badge>}
        <span className="ml-auto flex items-center gap-3">
          {approval.task_id && (
            <Link
              to={`/tasks/${approval.task_id}`}
              className="underline underline-offset-2"
              data-testid="approval-task-link"
            >
              裏方のタスク
            </Link>
          )}
          <span>{relativeTimeLabel(approval.created_at, fetchedAt)}</span>
        </span>
      </div>
      <div data-testid="approval-question" className="mt-1.5 text-sm">
        <MarkdownViewer content={approval.question} />
      </div>
      <dl className="mt-1.5 grid grid-cols-1 gap-y-1 text-sm">
        <DataItem label="答え" wide>
          {approval.answer || "-"}
        </DataItem>
      </dl>
    </li>
  );
}

function StandingRuleRow({ rule, org }: { rule: StandingRule; org: OrgNode[] }) {
  const fetcher = useFetcher<StandingRuleOpOutcome>({ key: `standing-rule-${rule.id}` });
  const submitting = fetcher.state !== "idle";

  return (
    <li
      data-testid="standing-rule-row"
      data-rule-id={rule.id}
      className="flex flex-wrap items-start justify-between gap-3 rounded-xl border border-border bg-surface p-3"
    >
      <div className="min-w-0 flex-1">
        <Badge tone={rule.node_id ? "neutral" : "teal"}>{standingRuleTargetName(rule, org)}</Badge>
        <p className="mt-1.5 text-sm text-fg">{rule.rule}</p>
        <p className="mt-1 text-xs text-fg-subtle">{rule.created_at}</p>
      </div>
      <details className="group shrink-0">
        <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-danger-border bg-danger-soft px-3 text-sm text-danger-soft-fg shadow-xs hover:bg-danger hover:text-white">
          <Icon name="xCircle" className="size-4" />
          削除
        </summary>
        <fetcher.Form method="post" className="mt-2 w-56 rounded-lg border border-danger-border bg-danger-soft/40 p-3">
          <input type="hidden" name="intent" value="standing_rule_delete" />
          <input type="hidden" name="id" value={rule.id} />
          <p className="mb-2 text-sm text-fg-muted">本当に削除しますか？</p>
          <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="standing-rule-delete">
            <Icon name="xCircle" />
            削除する
          </Button>
        </fetcher.Form>
      </details>
    </li>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const errorData = error.data as TaskdRouteErrorData;
    if (errorData.kind === "unavailable") {
      return (
        <main className="p-4">
          <TaskdBanner taskdApiUrl={errorData.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="p-4">
        <h1 className="text-xl font-semibold">エラー {errorData.status}</h1>
        <p className="mt-2 text-sm text-fg-muted">{errorData.detail}</p>
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
