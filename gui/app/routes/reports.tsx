import { useMemo } from "react";
import {
  data,
  type FetcherWithComponents,
  Form,
  isRouteErrorResponse,
  Link,
  useFetcher,
  useSearchParams,
} from "react-router";
import type { ActionError, NotifyTestOutcome, ReportOpOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { CelerisError, type CelerisRouteErrorData, celerisErrorResponse, isCelerisUnavailable } from "~/celeris/errors";
import { sendNotifyTest } from "~/celeris/notify-admin.server";
import { markReportsNotified, markReportsRead } from "~/celeris/reports-admin.server";
import type { NotifyView, OrgList, OrgNode, Project, ProjectList, ReportKind, ReportList } from "~/celeris/types";
import { ErrorFlash, NotifyTestFlash, ReportActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { NotificationsEnableButton } from "~/components/NotificationsEnable";
import { ReportsList } from "~/components/ReportsList";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { checkboxClass, chipLabelClass, labelClass, selectClass, touchLinkClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import { notifyKindLabel, notifyResultLabel, notifyResultTone, notifyTargetHref } from "~/lib/notify";
import { buildReportsQuery, filterReportsByKind } from "~/lib/reports";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { cn } from "~/lib/utils";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/reports";

/**
 * `/reports`（報告の流れ、SPEC §3.5・§4 の 4、ADR-0033 D3、ADR-0034、docs/gui/api.md §3.50〜3.53）。
 * 既定は秘書レベル（`level=0`）の未読を新しい順に、1 件 1 行で流し見できる密度で出す
 * （`GET /reports` 自体が新しい順を返す。docs/gui/api.md §3.50「新しい順（created_at 降順）」）。
 * `kind` の絞り込みは celeris 側 API に無いので GUI 側だけで行う（`~/lib/reports.ts` のコメント参照）。
 * 案件名・担当ノード名は `GET /projects` / `GET /org` から解決する（celeris 側に判断値を作らせない）。
 *
 * Discord への通知（ADR-0037、Phase 39、docs/celeris-api-v1.md §3.64〜3.65）の設定・テスト送信・直近の送信も
 * この画面の「通知」節に足す（ブラウザ通知の節はそのまま）。`GET /notify` は管理系ではないが celeris に届かない
 * こともあるため、`GET /secrets`（`app/routes/accounts.tsx`）と同じ形で `notifyError` に落として画面全体は
 * 壊さない。
 */

export interface ReportsData {
  reports: ReportList;
  projects: Project[];
  org: OrgNode[];
  notify: NotifyView | null;
  notifyError: ActionError | null;
  fetchedAt: string;
}

/**
 * `CelerisError` / `CelerisUnavailable`（`GET /notify` の失敗）を `ActionError` にする。`loadReports` はテストから
 * loader を介さず直接呼ばれるため、`actions.server.ts` の `toActionError` をそのまま使うと「loader/action 以外の
 * export から `.server` モジュールを参照できない」制約に触れる（`app/routes/accounts.tsx` の `secretsListError`
 * と同じ理由・同じ複製）。
 */
function notifyViewError(e: unknown): ActionError {
  if (isCelerisUnavailable(e)) {
    return {
      status: 503,
      code: "unavailable",
      detail: `celeris に接続できません（${e.baseUrl}）`,
      conflict: false,
      fields: {},
      messages: [],
    };
  }
  if (e instanceof CelerisError) {
    return { status: e.status, code: e.code, detail: e.detail, conflict: e.status === 409, fields: {}, messages: [] };
  }
  throw e;
}

export async function loadReports(client: CelerisClient, request: Request): Promise<ReportsData> {
  const searchParams = new URL(request.url).searchParams;
  const query = buildReportsQuery(searchParams);
  const [reports, projects, org] = await Promise.all([
    client.get<ReportList>("/reports", { query, signal: request.signal }),
    client.get<ProjectList>("/projects", { signal: request.signal }).catch(() => ({ items: [] }) as ProjectList),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
  ]);
  let notify: NotifyView | null = null;
  let notifyError: ActionError | null = null;
  try {
    notify = await client.get<NotifyView>("/notify", { signal: request.signal });
  } catch (e) {
    notifyError = notifyViewError(e);
  }
  return {
    reports,
    projects: projects.items,
    org: org.items,
    notify,
    notifyError,
    fetchedAt: new Date().toISOString(),
  };
}

export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ReportsData> {
  try {
    return await loadReports(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "報告 - Celeris" }];
}

/**
 * `reports_read`（選択 1 件・一括とも同じ intent。`ids` を複数付けられる）と `reports_notified`
 * （`NotificationsWatcher` がブラウザ通知を出した直後にも呼ぶ）はいずれも管理系（§3.52〜3.53）。
 * `notify_test`（ADR-0037 D4、§3.65。**管理系**、`token_file` 未設定でも 401）は Discord へのテスト送信。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getCelerisClient();

  let outcome: ReportOpOutcome | NotifyTestOutcome;
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
    case "notify_test":
      outcome = await sendNotifyTest(client, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

const LEVEL_OPTIONS: { value: string; label: string }[] = [
  { value: "", label: "すべて" },
  { value: "0", label: "CoS" },
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
  const { reports, projects, org, notify, notifyError, fetchedAt } = loaderData;
  const [searchParams] = useSearchParams();
  const filter = searchParams.get("filter") === "all" ? "all" : "unread";
  const level = searchParams.has("level") ? searchParams.get("level") : "0";
  const selectedKinds = searchParams.getAll("kind") as ReportKind[];
  const project = searchParams.get("project") ?? "";

  const visible = useMemo(() => filterReportsByKind(reports.items, selectedKinds), [reports.items, selectedKinds]);

  const readFetcher = useFetcher<ReportOpOutcome>();
  const notifyTestFetcher = useFetcher<NotifyTestOutcome>();
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
        {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
        <span className="text-sm text-fg-subtle lg:text-xs">
          悪い知らせは即座に、それ以外は数時間ごとにブラウザの通知でお知らせします。
        </span>
      </div>

      <DiscordSection notify={notify} notifyError={notifyError} fetcher={notifyTestFetcher} />

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

/**
 * Discord への通知の区画（ADR-0037、Phase 39、docs/celeris-api-v1.md §3.64〜3.65）。ブラウザ通知の節はそのまま
 * （`NotificationsEnableButton`）で、ここは celeris の tick が決定的に判定・送信する 5 種
 * （`~/lib/notify.ts` の `NOTIFY_KIND_LABEL`）の設定・テスト送信・直近 10 件を出す。
 */
function DiscordSection({
  notify,
  notifyError,
  fetcher,
}: {
  notify: NotifyView | null;
  notifyError: ActionError | null;
  fetcher: FetcherWithComponents<NotifyTestOutcome>;
}) {
  const submitting = fetcher.state !== "idle";
  return (
    <section aria-labelledby="discord-heading" className="space-y-3" data-testid="discord-section">
      <SectionTitle icon="message" id="discord-heading">
        通知（Discord）
      </SectionTitle>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="text-sm text-fg-subtle lg:text-xs">
        途中目標の仕事が終わった・認可の要求が来た・質問で止まっている・悪い知らせが届いた・CoS から方針の提案が 届いた
        — この 5 つ、人の判断が要るときだけ Discord にも 1 通届きます。結果が出たことは知らせません
        （それはこの「報告」の流れで見ます）。
      </p>

      {notifyError ? (
        <ErrorFlash error={notifyError} />
      ) : !notify ? (
        <EmptyState icon="message" title="通知の設定を取得できませんでした" />
      ) : (
        <>
          <div data-testid="discord-configured" data-configured={notify.configured ? "true" : "false"}>
            {notify.configured ? (
              <Alert tone="success" title="設定済み">
                <p>
                  fingerprint <Mono>{notify.fingerprint}</Mono>
                </p>
                <fetcher.Form method="post" className="mt-2">
                  <input type="hidden" name="intent" value="notify_test" />
                  <Button type="submit" variant="secondary" size="sm" disabled={submitting} data-testid="discord-test">
                    <Icon name="send" />
                    テスト送信
                  </Button>
                </fetcher.Form>
                <NotifyTestFlash outcome={fetcher.data} />
              </Alert>
            ) : (
              <Alert tone="warning" title="未設定">
                <p>
                  Discord への通知は未設定です。
                  <Link to="/accounts#secrets" className={cn(touchLinkClass, "underline underline-offset-2")}>
                    アカウント → API キー
                  </Link>
                  に id <Mono>{notify.secret_id}</Mono> で Webhook URL を登録してください（値は二度と表示されません）。
                </p>
              </Alert>
            )}
          </div>

          <div className="space-y-1.5">
            {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
            <p className="text-sm font-medium text-fg-subtle lg:text-xs">直近の送信</p>
            {notify.recent.length === 0 ? (
              <p className="text-sm text-fg-subtle lg:text-xs">まだありません。</p>
            ) : (
              <ul className="space-y-1">
                {notify.recent.map((r) => {
                  const href = notifyTargetHref(r);
                  return (
                    <li
                      key={`${r.kind}-${r.key}-${r.created_at}`}
                      data-testid="discord-recent-row"
                      data-notification-kind={r.kind}
                      className="flex flex-wrap items-center gap-2 rounded-lg border border-border px-3 py-1.5 text-sm lg:text-xs"
                    >
                      <Badge tone={notifyResultTone(r)}>{notifyKindLabel(r.kind)}</Badge>
                      {href ? (
                        <Link to={href} className="underline underline-offset-2">
                          {r.key}
                        </Link>
                      ) : (
                        <span className="text-fg-subtle">{r.key}</span>
                      )}
                      <span className="text-fg-subtle">{r.created_at}</span>
                      <span>{notifyResultLabel(r)}</span>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
        </>
      )}
    </section>
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
