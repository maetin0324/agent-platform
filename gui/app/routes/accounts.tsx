import { data, type FetcherWithComponents, isRouteErrorResponse, useFetcher } from "react-router";
import { AccountActionFlash, ErrorFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import { TONE_SOLID_BG, type Tone } from "~/components/ui/tone";
import { formatDuration, secondsBetween } from "~/lib/time-delta";
import { TaskdBanner } from "~/root";
import {
  cancelAccountLogin,
  checkAccount,
  createAccount,
  deleteAccount,
  readAccountId,
  readLoginCode,
  startAccountLogin,
  submitAccountLoginCode,
} from "~/taskd/accounts-admin.server";
import type { AccountOpOutcome } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { AccountList, AccountView } from "~/taskd/types";
import type { Route } from "./+types/accounts";

/**
 * `/accounts`（Claude アカウントのプール、ADR-GUI-0012 D3、docs/taskd-api-v1.md §3.29）。
 * `GET /accounts` をそのまま描く。値の再計算（スコアやリセット判定）はしない。
 * `observed_at` の相対時刻表示・cooldown/resets_at の残り時間だけは表示のための変換として行う
 * （`/providers` の cooldown 残り時間と同じ扱い、docs/adr/0007 D3）。
 */
export interface AccountsData {
  accounts: AccountList;
  fetchedAt: string;
}

export async function loadAccounts(client: TaskdClient, request: Request): Promise<AccountsData> {
  const accounts = await client.get<AccountList>("/accounts", { signal: request.signal });
  return { accounts, fetchedAt: new Date().toISOString() };
}

export async function loader({ request }: Route.LoaderArgs): Promise<AccountsData> {
  try {
    return await loadAccounts(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "アカウント - taskd-gui" }];
}

/**
 * 追加・削除・確認・ログイン中継（ADR-GUI-0012 D3）。管理系はすべて `accounts-admin.server.ts` に任せ、
 * ここはフォームの `intent` を対応する呼び出しに写すだけ（GUI 側で判断ロジックは持たない）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getTaskdClient();
  const id = readAccountId(form);
  let outcome: AccountOpOutcome;
  switch (intent) {
    case "create":
      outcome = await createAccount(client, id, request.signal);
      break;
    case "delete":
      outcome = await deleteAccount(client, id, request.signal);
      break;
    case "check":
      outcome = await checkAccount(client, id, request.signal);
      break;
    case "login_start":
      outcome = await startAccountLogin(client, id, request.signal);
      break;
    case "login_code":
      outcome = await submitAccountLoginCode(client, id, readLoginCode(form), request.signal);
      break;
    case "login_cancel":
      outcome = await cancelAccountLogin(client, id, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

const EXCLUDED_REASON_LABEL: Record<string, string> = {
  not_logged_in: "未ログイン",
  at_capacity: "上限に達しています",
  cooldown: "cooldown 中",
  five_hour_exhausted: "5 時間枠を使い切りました",
  seven_day_exhausted: "週次枠を使い切りました",
  rejected: "拒否されました",
};

const COOLDOWN_REASON_LABEL: Record<string, string> = {
  auth_failed: "認証エラー（再ログインが必要）",
  throttled: "スロットル",
  exhausted: "枯渇",
};

function usageTone(utilization: number): Tone {
  if (utilization >= 0.9) return "danger";
  if (utilization >= 0.7) return "warning";
  return "primary";
}

export default function AccountsPage({ loaderData }: Route.ComponentProps) {
  const { accounts, fetchedAt } = loaderData;
  // taskd の SSE（daemon tick）による自動再検証のたびに `<Form>` の actionData は消える（React Router の仕様、
  // `app/hooks/useTaskdStream.ts`）。ログイン URL は「もう一度出せない」ものなので特に影響が大きい: 1 つの
  // `useFetcher()` にまとめ、その `fetcher.data` を表示する（fetcher の状態は revalidate() の影響を受けない）。
  const fetcher = useFetcher<AccountOpOutcome>();
  const submitting = fetcher.state !== "idle";

  return (
    <div className="space-y-8" data-testid="accounts-page">
      <PageHeader
        icon="users"
        title={
          <>
            アカウント
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="account_pool = true のプロバイダが使う Claude アカウントのプール。ログイン・残量の確認・削除をここで行います。"
      />

      <AccountActionFlash outcome={fetcher.data} />

      {accounts.root == null ? (
        <EmptyState icon="users" title="[accounts] が設定されていません">
          taskd.toml に <Mono>[accounts]</Mono> セクションを足すとプールが使えます。例:
          <pre className="mt-2 overflow-x-auto rounded-lg bg-surface-2 p-3 text-left text-xs">
            {'[accounts]\nclaude_dir = "claude-accounts"'}
          </pre>
        </EmptyState>
      ) : (
        <>
          <section aria-labelledby="accounts-heading" className="space-y-4">
            <SectionTitle icon="users" id="accounts-heading" count={accounts.items.length}>
              プール（{accounts.root}）
            </SectionTitle>
            <DataItem label="max_runs_per_account">
              <span data-testid="accounts-max-runs">{accounts.max_runs_per_account}</span>
            </DataItem>

            {accounts.items.length === 0 ? (
              <EmptyState icon="users" title="アカウントがありません" />
            ) : (
              <div className="grid gap-4 xl:grid-cols-2">
                {accounts.items.map((item) => (
                  <AccountCard
                    key={item.id}
                    item={item}
                    maxRuns={accounts.max_runs_per_account}
                    fetchedAt={fetchedAt}
                    fetcher={fetcher}
                    submitting={submitting}
                  />
                ))}
              </div>
            )}
          </section>

          <section aria-labelledby="account-add-heading" className="space-y-4">
            <SectionTitle icon="plus" id="account-add-heading">
              アカウントを追加
            </SectionTitle>
            <Card>
              <CardHeader
                icon="plus"
                title="新規アカウント"
                description="claude_dir 配下に <id>/ を 0700 で作ります。"
              />
              <CardBody>
                <fetcher.Form method="post" data-testid="account-add-form" className="flex flex-wrap items-end gap-3">
                  <input type="hidden" name="intent" value="create" />
                  <div>
                    <label htmlFor="account-add-id" className={labelClass}>
                      id
                    </label>
                    <input id="account-add-id" name="id" type="text" required className={`${inputClass} mt-1.5`} />
                  </div>
                  <Button type="submit" variant="primary" disabled={submitting} data-testid="account-add-submit">
                    <Icon name="plus" />
                    追加
                  </Button>
                </fetcher.Form>
              </CardBody>
            </Card>
          </section>
        </>
      )}
    </div>
  );
}

function AccountCard({
  item,
  maxRuns,
  fetchedAt,
  fetcher,
  submitting,
}: {
  item: AccountView;
  maxRuns: number;
  fetchedAt: string;
  fetcher: FetcherWithComponents<AccountOpOutcome>;
  submitting: boolean;
}) {
  const actionData = fetcher.data;
  const loginStart =
    actionData && actionData.ok && actionData.op === "login_start" && actionData.id === item.id
      ? actionData
      : undefined;
  const loginCodeError =
    actionData && !actionData.ok && actionData.id === item.id && actionData.op === "login_code"
      ? actionData.error
      : undefined;
  const showLoginPanel = !!loginStart || item.login_pending;
  const tone: Tone = item.cooldown ? "warning" : item.logged_in ? "success" : "neutral";

  return (
    <Card data-testid="account-card" data-account-id={item.id} className="hover:shadow-md">
      <CardHeader
        icon="user"
        tone={tone}
        title={<Mono className="text-sm font-semibold text-fg">{item.id}</Mono>}
        description={item.dir}
        actions={
          <Badge tone={item.logged_in ? "success" : "warning"} dot data-testid="account-logged-in">
            {item.logged_in ? "ログイン済み" : "未ログイン"}
          </Badge>
        }
      />
      <CardBody className="space-y-4">
        <UsageBar
          label="5 時間枠"
          testId="account-usage-five-hour"
          window={item.usage?.five_hour ?? null}
          fetchedAt={fetchedAt}
        />
        <UsageBar
          label="週次枠"
          testId="account-usage-seven-day"
          window={item.usage?.seven_day ?? null}
          fetchedAt={fetchedAt}
        />

        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm sm:grid-cols-3">
          <DataItem label="status">{item.usage?.status ?? "-"}</DataItem>
          <DataItem label="observed_at" wide>
            {item.usage ? (
              <>
                {item.usage.observed_at}（{formatDuration(secondsBetween(item.usage.observed_at, fetchedAt))} 前・
                {item.usage.source}）
              </>
            ) : (
              "-"
            )}
          </DataItem>
          <DataItem label="score">
            {item.score != null ? (
              <span data-testid="account-score">{item.score.toFixed(2)}</span>
            ) : (
              <span data-testid="account-excluded">
                {item.excluded_reason ? (EXCLUDED_REASON_LABEL[item.excluded_reason] ?? item.excluded_reason) : "-"}
                {item.excluded_reason && <span className="ml-1 text-fg-subtle">（{item.excluded_reason}）</span>}
              </span>
            )}
          </DataItem>
          <DataItem label="in_use / max">
            {item.in_use} / {maxRuns}
          </DataItem>
        </dl>

        {item.cooldown && (
          <Alert tone="warning" title="cooldown">
            <dl className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3">
              <DataItem label="reason">
                {COOLDOWN_REASON_LABEL[item.cooldown.reason] ?? item.cooldown.reason}
                <span className="ml-1 text-fg-subtle">（{item.cooldown.reason}）</span>
              </DataItem>
              <DataItem label="until">
                <span className="text-fg-subtle">{item.cooldown.until}</span>
              </DataItem>
            </dl>
          </Alert>
        )}

        <Alert
          tone={item.last_check ? (item.last_check.result === "ok" ? "success" : "danger") : "neutral"}
          title="最後の確認"
        >
          {item.last_check ? (
            <>
              {item.last_check.result}
              {item.last_check.detail && <> — {item.last_check.detail}</>}（{item.last_check.at}）
            </>
          ) : (
            "未確認"
          )}
        </Alert>

        <dl className="grid grid-cols-3 gap-x-4 gap-y-3 text-sm sm:grid-cols-4">
          <DataItem label="runs">{item.stats.runs}</DataItem>
          <DataItem label="done">{item.stats.done}</DataItem>
          <DataItem label="error">{item.stats.error}</DataItem>
          <DataItem label="tokens (input+output)" wide>
            {item.stats.input_tokens + item.stats.output_tokens}
          </DataItem>
        </dl>

        <div className="flex flex-wrap items-center gap-2 border-t border-border pt-3">
          <fetcher.Form method="post">
            <input type="hidden" name="intent" value="check" />
            <input type="hidden" name="id" value={item.id} />
            <Button type="submit" variant="secondary" size="sm" disabled={submitting} data-testid="account-check">
              <Icon name="activity" />
              確認
            </Button>
          </fetcher.Form>

          {!showLoginPanel && (
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="login_start" />
              <input type="hidden" name="id" value={item.id} />
              <Button type="submit" variant="soft" size="sm" disabled={submitting} data-testid="account-login-start">
                <Icon name="link" />
                ログイン
              </Button>
            </fetcher.Form>
          )}

          <details className="group">
            <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-danger-border bg-danger-soft px-3 text-sm text-danger-soft-fg shadow-xs hover:bg-danger hover:text-white">
              <Icon name="xCircle" className="size-4" />
              削除
            </summary>
            <fetcher.Form method="post" className="mt-3 rounded-lg border border-danger-border bg-danger-soft/40 p-3">
              <input type="hidden" name="intent" value="delete" />
              <input type="hidden" name="id" value={item.id} />
              <p className="mb-2 text-sm text-fg-muted">
                本当に <span className="font-mono">{item.id}</span> を削除しますか？（認証ファイルは消さず
                claude_dir/.removed/ に移します）
              </p>
              <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="account-delete">
                <Icon name="xCircle" />
                削除する
              </Button>
            </fetcher.Form>
          </details>
        </div>

        {showLoginPanel && (
          <div className="space-y-3 rounded-lg border border-primary-border bg-primary-soft/40 p-3">
            <Alert tone="warning" title="セキュリティ上の注意">
              認可コードは平文 HTTP を通ります（ADR-0024 D7）。信頼できるネットワークでだけ使ってください。
            </Alert>
            {loginStart ? (
              <p>
                このリンクをブラウザで開いて認可し、表示されたコードを下に入力してください:{" "}
                <a
                  href={loginStart.login.url}
                  target="_blank"
                  rel="noreferrer noopener"
                  data-testid="account-login-url"
                  className="break-all underline underline-offset-2"
                >
                  {loginStart.login.url}
                </a>
              </p>
            ) : (
              <p className="text-sm text-fg-muted">
                ログイン処理が進行中です。URL
                はこの画面を離れると再表示できません。もう一度「ログイン」を押すとやり直せます。
              </p>
            )}
            {loginCodeError && <ErrorFlash error={loginCodeError} />}
            <fetcher.Form method="post" className="flex flex-wrap items-end gap-3">
              <input type="hidden" name="intent" value="login_code" />
              <input type="hidden" name="id" value={item.id} />
              <div>
                <label htmlFor={`login-code-${item.id}`} className={labelClass}>
                  認可コード
                </label>
                <input
                  id={`login-code-${item.id}`}
                  name="code"
                  type="text"
                  autoComplete="off"
                  className={`${inputClass} mt-1.5`}
                  data-testid="account-login-code"
                />
                <p className={hintClass}>コードはログにも応答にも残りません。</p>
              </div>
              <Button
                type="submit"
                variant="primary"
                size="sm"
                disabled={submitting}
                data-testid="account-login-submit"
              >
                <Icon name="check" />
                送信
              </Button>
            </fetcher.Form>
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="login_cancel" />
              <input type="hidden" name="id" value={item.id} />
              <Button type="submit" variant="ghost" size="sm" disabled={submitting} data-testid="account-login-cancel">
                <Icon name="x" />
                中止
              </Button>
            </fetcher.Form>
          </div>
        )}
      </CardBody>
    </Card>
  );
}

function UsageBar({
  label,
  testId,
  window,
  fetchedAt,
}: {
  label: string;
  testId: string;
  window: { utilization: number; resets_at: string } | null;
  fetchedAt: string;
}) {
  if (!window) {
    return (
      <div data-testid={testId}>
        <div className="flex items-center justify-between text-xs text-fg-subtle">
          <span>{label}</span>
          <span>-</span>
        </div>
      </div>
    );
  }
  const pct = Math.round(window.utilization * 100);
  const tone = usageTone(window.utilization);
  const remaining = secondsBetween(fetchedAt, window.resets_at);
  return (
    <div data-testid={testId}>
      <div className="flex items-center justify-between text-xs text-fg-muted">
        <span>{label}</span>
        <span className="tabular-nums">{pct}%</span>
      </div>
      <div className="mt-1 h-2 w-full overflow-hidden rounded-full bg-surface-2">
        <div className={`h-full rounded-full ${TONE_SOLID_BG[tone]}`} style={{ width: `${Math.min(100, pct)}%` }} />
      </div>
      <p className="mt-1 text-xs text-fg-subtle">
        resets_at: {window.resets_at}（リセットまで {formatDuration(remaining)}）
      </p>
    </div>
  );
}

/**
 * loader が `taskdErrorResponse` で投げた `Response` を判別する（`/providers` と同じ方針）。
 */
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
