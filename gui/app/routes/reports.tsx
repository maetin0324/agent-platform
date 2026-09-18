import { useMemo } from "react";
import { data, Form, isRouteErrorResponse, useFetcher, useSearchParams } from "react-router";
import { ReportActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { NotificationsEnableButton } from "~/components/NotificationsEnable";
import { ReportsList } from "~/components/ReportsList";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { checkboxClass, chipLabelClass, labelClass, selectClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { EmptyState, PageHeader, SectionTitle } from "~/components/ui/misc";
import { buildReportsQuery, filterReportsByKind } from "~/lib/reports";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import type { ReportOpOutcome } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { markReportsNotified, markReportsRead } from "~/taskd/reports-admin.server";
import type { OrgList, OrgNode, Project, ProjectList, ReportKind, ReportList } from "~/taskd/types";
import type { Route } from "./+types/reports";

/**
 * `/reports`（報告の流れ、SPEC §3.5・§4 の 4、ADR-0033 D3、ADR-0034、docs/gui/api.md §3.50〜3.53）。
 * 既定は秘書レベル（`level=0`）の未読を新しい順に、1 件 1 行で流し見できる密度で出す
 * （`GET /reports` 自体が新しい順を返す。docs/gui/api.md §3.50「新しい順（created_at 降順）」）。
 * `kind` の絞り込みは taskd 側 API に無いので GUI 側だけで行う（`~/lib/reports.ts` のコメント参照）。
 * 案件名・担当ノード名は `GET /projects` / `GET /org` から解決する（taskd 側に判断値を作らせない）。
 */

export interface ReportsData {
  reports: ReportList;
  projects: Project[];
  org: OrgNode[];
  fetchedAt: string;
}

export async function loadReports(client: TaskdClient, request: Request): Promise<ReportsData> {
  const searchParams = new URL(request.url).searchParams;
  const query = buildReportsQuery(searchParams);
  const [reports, projects, org] = await Promise.all([
    client.get<ReportList>("/reports", { query, signal: request.signal }),
    client.get<ProjectList>("/projects", { signal: request.signal }).catch(() => ({ items: [] }) as ProjectList),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
  ]);
  return { reports, projects: projects.items, org: org.items, fetchedAt: new Date().toISOString() };
}

export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ReportsData> {
  try {
    return await loadReports(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "報告 - taskd-gui" }];
}

/**
 * `reports_read`（選択 1 件・一括とも同じ intent。`ids` を複数付けられる）と `reports_notified`
 * （`NotificationsWatcher` がブラウザ通知を出した直後にも呼ぶ）。いずれも管理系（§3.52〜3.53）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getTaskdClient();

  let outcome: ReportOpOutcome;
  switch (intent) {
    case "reports_read": {
      const ids = form
        .getAll("ids")
        .map(String)
        .filter((id) => id.length > 0);
      outcome = await markReportsRead(client, ids, request.signal);
      break;
    }
    case "reports_notified":
      outcome = await markReportsNotified(client, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

const LEVEL_OPTIONS: { value: string; label: string }[] = [
  { value: "", label: "すべて" },
  { value: "0", label: "秘書" },
  { value: "1", label: "部" },
  { value: "2", label: "課" },
];

const KIND_OPTIONS: { value: ReportKind; label: string }[] = [
  { value: "bad_news", label: "悪い知らせ" },
  { value: "result", label: "結果" },
  { value: "proposal", label: "提案" },
  { value: "question", label: "質問" },
  { value: "progress", label: "経過" },
];

export default function ReportsPage({ loaderData }: Route.ComponentProps) {
  const { reports, projects, org, fetchedAt } = loaderData;
  const [searchParams] = useSearchParams();
  const filter = searchParams.get("filter") === "all" ? "all" : "unread";
  const level = searchParams.has("level") ? searchParams.get("level") : "0";
  const selectedKinds = searchParams.getAll("kind") as ReportKind[];
  const project = searchParams.get("project") ?? "";

  const visible = useMemo(() => filterReportsByKind(reports.items, selectedKinds), [reports.items, selectedKinds]);

  const readFetcher = useFetcher<ReportOpOutcome>();
  const markAllRead = () => {
    const unreadIds = visible.filter((r) => r.read_at == null).map((r) => r.id);
    if (unreadIds.length === 0) return;
    const form = new FormData();
    form.set("intent", "reports_read");
    for (const id of unreadIds) form.append("ids", id);
    readFetcher.submit(form, { method: "post", action: "/reports" });
  };
  const unreadVisibleCount = visible.filter((r) => r.read_at == null).length;

  return (
    <div className="space-y-8">
      <PageHeader
        icon="send"
        title={
          <>
            報告
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="上に行くほどレビューが入り、圧縮されます。良い知らせも悪い知らせも、ここで高速に流し見します。"
      />

      <div className="flex flex-wrap items-center gap-3">
        <NotificationsEnableButton />
        <span className="text-xs text-fg-subtle">
          悪い知らせは即座に、それ以外は数時間ごとにブラウザの通知でお知らせします。
        </span>
      </div>

      <ReportActionFlash outcome={readFetcher.data} />

      <Card>
        <CardHeader icon="filter" title="絞り込み" />
        <CardBody>
          <Form method="get" className="space-y-4" data-testid="reports-filter-form">
            <div className="flex flex-wrap items-end gap-4">
              <div>
                <label htmlFor="reports-filter-select" className={labelClass}>
                  未読
                </label>
                <select
                  id="reports-filter-select"
                  name="filter"
                  defaultValue={filter}
                  className={`${selectClass} mt-1.5`}
                >
                  <option value="unread">未読だけ</option>
                  <option value="all">全部</option>
                </select>
              </div>
              <div>
                <label htmlFor="reports-filter-level" className={labelClass}>
                  どの段まで
                </label>
                <select
                  id="reports-filter-level"
                  name="level"
                  data-testid="reports-filter-level"
                  defaultValue={level ?? ""}
                  className={`${selectClass} mt-1.5`}
                >
                  {LEVEL_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="reports-filter-project" className={labelClass}>
                  案件
                </label>
                <select
                  id="reports-filter-project"
                  name="project"
                  defaultValue={project}
                  className={`${selectClass} mt-1.5`}
                >
                  <option value="">すべて</option>
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.title}
                    </option>
                  ))}
                </select>
              </div>
              <Button type="submit" variant="secondary" size="sm">
                <Icon name="filter" />
                絞り込む
              </Button>
            </div>
            <fieldset>
              <legend className={labelClass}>知らせの種類</legend>
              <div className="mt-2 flex flex-wrap gap-2">
                {KIND_OPTIONS.map((opt) => (
                  <label key={opt.value} className={chipLabelClass}>
                    <input
                      type="checkbox"
                      name="kind"
                      value={opt.value}
                      defaultChecked={selectedKinds.includes(opt.value)}
                      className={checkboxClass}
                    />
                    {opt.label}
                  </label>
                ))}
              </div>
            </fieldset>
          </Form>
        </CardBody>
      </Card>

      <section aria-labelledby="reports-heading" className="space-y-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <SectionTitle icon="send" id="reports-heading" count={visible.length}>
            報告
          </SectionTitle>
          <Button
            variant="secondary"
            size="sm"
            disabled={unreadVisibleCount === 0 || readFetcher.state !== "idle"}
            onClick={markAllRead}
            data-testid="reports-mark-all-read"
          >
            <Icon name="checkCircle" />
            表示中の未読をすべて既読にする（{unreadVisibleCount}）
          </Button>
        </div>
        {visible.length === 0 ? (
          <EmptyState icon="send" title="報告がありません">
            絞り込みを変えると出てくるかもしれません。
          </EmptyState>
        ) : (
          <ReportsList items={visible} projects={projects} org={org} fetchedAt={fetchedAt} />
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
