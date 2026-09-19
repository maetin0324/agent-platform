import { data, type FetcherWithComponents, isRouteErrorResponse, useFetcher } from "react-router";
import { AccountActionFlash, ErrorFlash, SecretActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, selectClass } from "~/components/ui/form";
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
  readAccountAdapter,
  readAccountId,
  readLoginCode,
  startAccountLogin,
  submitAccountLoginCode,
} from "~/taskd/accounts-admin.server";
import type { AccountAdapter, AccountOpOutcome, ActionError, SecretActionResult } from "~/taskd/action-types";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { isTaskdUnavailable, TaskdError, type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { deleteSecret, putSecret, readSecretId, readSecretValue } from "~/taskd/secrets-admin.server";
import type { AccountList, AccountView, SecretList, SecretView } from "~/taskd/types";
import type { Route } from "./+types/accounts";

/**
 * `/accounts`（Claude アカウントのプール、ADR-GUI-0012 D3、docs/taskd-api-v1.md §3.29）。
 * `GET /accounts` をそのまま描く。値の再計算（スコアやリセット判定）はしない。
 * `observed_at` の相対時刻表示・cooldown/resets_at の残り時間だけは表示のための変換として行う
 * （`/providers` の cooldown 残り時間と同じ扱い、docs/adr/0007 D3）。
 */
export interface AccountsData {
  accounts: AccountList;
  /** `GET /secrets`（ADR-0030 D3〜D4）。管理系のため 401 になりうる: その場合は `secretsError` に入れ、
   * 画面全体は壊さず「API キー」節だけにトークン案内を出す（`GET /accounts` 自体は管理系ではない）。 */
  secrets: SecretList | null;
  secretsError: ActionError | null;
  fetchedAt: string;
}

/**
 * `TaskdError` / `TaskdUnavailable`（`GET /secrets` の 401 等）を `ActionError` にする。`actions.server.ts` の
 * `toActionError` と同じ変換だが、`loadAccounts` はテストから loader を介さず直接呼ばれるため、サーバ専用
 * モジュールを `loader`/`action` 以外の export から参照できない制約（React Router の dot-server 除去）を避けて
 * ここに複製する（`GET /secrets` が返すのは 401/409/503 のみで `errors[]` を持たないので fields/messages は空でよい）。
 */
function secretsListError(e: unknown): ActionError {
  if (isTaskdUnavailable(e)) {
    return {
      status: 503,
      code: "unavailable",
      detail: `taskd に接続できません（${e.baseUrl}）`,
      conflict: false,
      fields: {},
      messages: [],
    };
  }
  if (e instanceof TaskdError) {
    return { status: e.status, code: e.code, detail: e.detail, conflict: e.status === 409, fields: {}, messages: [] };
  }
  throw e;
}

export async function loadAccounts(client: TaskdClient, request: Request): Promise<AccountsData> {
  const accounts = await client.get<AccountList>("/accounts", { signal: request.signal });
  let secrets: SecretList | null = null;
  let secretsError: ActionError | null = null;
  try {
    secrets = await client.get<SecretList>("/secrets", { signal: request.signal });
  } catch (e) {
    secretsError = secretsListError(e);
  }
  return { accounts, secrets, secretsError, fetchedAt: new Date().toISOString() };
}

export async function loader({ request }: Route.LoaderArgs): Promise<AccountsData> {
  try {
    return await loadAccounts(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "アカウント - Celeris" }];
}

/**
 * 追加・削除・確認・ログイン・API キーの追加/更新/削除の中継（ADR-GUI-0012 D3、ADR-0030 D4）。管理系はすべて
 * `accounts-admin.server.ts` / `secrets-admin.server.ts` に任せ、ここはフォームの `intent` を対応する呼び出しに
 * 写すだけ（GUI 側で判断ロジックは持たない）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getTaskdClient();

  if (intent === "secret_put" || intent === "secret_delete") {
    const secretId = readSecretId(form);
    const result =
      intent === "secret_put"
        ? await putSecret(client, secretId, readSecretValue(form), request.signal)
        : await deleteSecret(client, secretId, request.signal);
    return data(result, { status: result.op.ok ? 200 : result.op.error.status });
  }

  const id = readAccountId(form);
  const adapter = readAccountAdapter(form);
  let outcome: AccountOpOutcome;
  switch (intent) {
    case "create":
      outcome = await createAccount(client, id, adapter, request.signal);
      break;
    case "delete":
      outcome = await deleteAccount(client, id, adapter, request.signal);
      break;
    case "check":
      outcome = await checkAccount(client, id, adapter, request.signal);
      break;
    case "login_start":
      outcome = await startAccountLogin(client, id, adapter, request.signal);
      break;
    case "login_code":
      outcome = await submitAccountLoginCode(client, id, adapter, readLoginCode(form), request.signal);
      break;
    case "login_cancel":
      outcome = await cancelAccountLogin(client, id, adapter, request.signal);
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

const ADAPTER_TONE: Record<AccountAdapter, Tone> = { "claude-code": "info", codex: "teal" };

/**
 * `roots`（ADR-0025 D6）から、根ディレクトリが設定されているアダプタだけを `claude-code` → `codex` の順で返す。
 * `roots` が無い（古い taskd）場合は `root`（claude-code の別名）だけにフォールバックする。
 */
function configuredAdapters(accounts: AccountList): { adapter: AccountAdapter; root: string }[] {
  const claudeRoot = accounts.roots?.["claude-code"] ?? accounts.root ?? null;
  const codexRoot = accounts.roots?.codex ?? null;
  const result: { adapter: AccountAdapter; root: string }[] = [];
  if (claudeRoot) result.push({ adapter: "claude-code", root: claudeRoot });
  if (codexRoot) result.push({ adapter: "codex", root: codexRoot });
  return result;
}

function accountAdapter(item: AccountView): AccountAdapter {
  return item.adapter === "codex" ? "codex" : "claude-code";
}

export default function AccountsPage({ loaderData }: Route.ComponentProps) {
  const { accounts, secrets, secretsError, fetchedAt } = loaderData;
  // taskd の SSE（daemon tick）による自動再検証のたびに `<Form>` の actionData は消える（React Router の仕様、
  // `app/hooks/useTaskdStream.ts`）。ログイン URL は「もう一度出せない」ものなので特に影響が大きい: 1 つの
  // `useFetcher()` にまとめ、その `fetcher.data` を表示する（fetcher の状態は revalidate() の影響を受けない）。
  const fetcher = useFetcher<AccountOpOutcome>();
  const submitting = fetcher.state !== "idle";
  // API キー節は別の fetcher にする（アカウントの操作結果と型が違う。ADR-0030 D4）。
  const secretFetcher = useFetcher<SecretActionResult>();
  const secretSubmitting = secretFetcher.state !== "idle";

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
        description="account_pool = true のプロバイダが使う claude-code / codex アカウントのプール。ログイン・残量の確認・削除をここで行います。"
      />

      <AccountActionFlash outcome={fetcher.data} />

      {(() => {
        const configured = configuredAdapters(accounts);
        if (configured.length === 0) {
          return (
            <EmptyState icon="users" title="[accounts] が設定されていません">
              taskd.toml に <Mono>[accounts]</Mono> セクションを足すとプールが使えます（<Mono>claude_dir</Mono>・
              <Mono>codex_dir</Mono> のどちらか、または両方）。例:
              <pre className="mt-2 overflow-x-auto rounded-lg bg-surface-2 p-3 text-left text-xs">
                {'[accounts]\nclaude_dir = "claude-accounts"\ncodex_dir = "codex-accounts"'}
              </pre>
            </EmptyState>
          );
        }
        return (
          <>
            <DataItem label="max_runs_per_account">
              <span data-testid="accounts-max-runs">{accounts.max_runs_per_account}</span>
            </DataItem>

            {configured.map(({ adapter, root }) => {
              const items = accounts.items.filter((item) => accountAdapter(item) === adapter);
              return (
                <section key={adapter} aria-labelledby={`accounts-heading-${adapter}`} className="space-y-4">
                  <SectionTitle icon="users" id={`accounts-heading-${adapter}`} count={items.length}>
                    <Badge tone={ADAPTER_TONE[adapter]}>{adapter}</Badge> プール（{root}）
                  </SectionTitle>

                  {items.length === 0 ? (
                    <EmptyState icon="users" title="アカウントがありません" />
                  ) : (
                    <div className="grid gap-4 xl:grid-cols-2">
                      {items.map((item) => (
                        <AccountCard
                          key={`${adapter}-${item.id}`}
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
              );
            })}

            <section aria-labelledby="account-add-heading" className="space-y-4">
              <SectionTitle icon="plus" id="account-add-heading">
                アカウントを追加
              </SectionTitle>
              <Card>
                <CardHeader
                  icon="plus"
                  title="新規アカウント"
                  description="選んだアダプタの根ディレクトリ配下に <id>/ を 0700 で作ります。"
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
                    <div>
                      <label htmlFor="account-add-adapter" className={labelClass}>
                        adapter
                      </label>
                      <select
                        id="account-add-adapter"
                        name="adapter"
                        data-testid="account-add-adapter"
                        defaultValue={configured[0].adapter}
                        className={`${selectClass} mt-1.5`}
                      >
                        {configured.map(({ adapter }) => (
                          <option key={adapter} value={adapter}>
                            {adapter}
                          </option>
                        ))}
                      </select>
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
        );
      })()}

      <SecretsSection
        secrets={secrets}
        secretsError={secretsError}
        fetcher={secretFetcher}
        submitting={secretSubmitting}
        fetchedAt={fetchedAt}
      />
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
  const adapter = accountAdapter(item);
  const actionData = fetcher.data;
  const loginStart =
    actionData?.ok && actionData.op === "login_start" && actionData.id === item.id && actionData.adapter === adapter
      ? actionData
      : undefined;
  const loginCodeError =
    actionData &&
    !actionData.ok &&
    actionData.id === item.id &&
    actionData.adapter === adapter &&
    actionData.op === "login_code"
      ? actionData.error
      : undefined;
  // ログインが終わった（`logged_in`）ら閉じる。`loginStart` は次に何か送信するまで fetcher.data に残り続ける
  // ため（`useFetcher` は SSE の再検証では消えない、上のコメント参照）、これが無いと codex（コード入力が無く
  // 完了を待つだけ）のパネルが完了後も表示され続けてしまう。
  const showLoginPanel = (!!loginStart || item.login_pending) && !item.logged_in;
  // codex には `paste_code` は無い（ADR-0025 D5）ので、fetcher にまだ何も無い（画面を開き直した）場合は
  // アダプタから決め打ちできる。これはアダプタ→流儀の固定対応であって、スコア等の再計算ではない。
  const kind =
    loginStart?.login.kind === "device_code"
      ? "device_code"
      : loginStart
        ? "paste_code"
        : adapter === "codex"
          ? "device_code"
          : "paste_code";
  const tone: Tone = item.cooldown ? "warning" : item.logged_in ? "success" : "neutral";

  return (
    <Card
      data-testid="account-card"
      data-account-id={item.id}
      data-account-adapter={adapter}
      className="hover:shadow-md"
    >
      <CardHeader
        icon="user"
        tone={tone}
        title={<Mono className="text-sm font-semibold text-fg">{item.id}</Mono>}
        description={item.dir}
        actions={
          <>
            <Badge tone={ADAPTER_TONE[adapter]} data-testid="account-adapter">
              {adapter}
            </Badge>
            <Badge tone={item.logged_in ? "success" : "warning"} dot data-testid="account-logged-in">
              {item.logged_in ? "ログイン済み" : "未ログイン"}
            </Badge>
          </>
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
            <input type="hidden" name="adapter" value={adapter} />
            <Button type="submit" variant="secondary" size="sm" disabled={submitting} data-testid="account-check">
              <Icon name="activity" />
              確認
            </Button>
          </fetcher.Form>

          {/* 進行中でも、この画面に出せる URL / コードが無い（開き直した等）ならやり直せるようにする。
              コードは taskd も保存しないので、失ったら開始し直すしかない（前のコードは無効になる）。 */}
          {(!showLoginPanel || !loginStart) && (
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="login_start" />
              <input type="hidden" name="id" value={item.id} />
              <input type="hidden" name="adapter" value={adapter} />
              <Button type="submit" variant="soft" size="sm" disabled={submitting} data-testid="account-login-start">
                <Icon name="link" />
                {showLoginPanel ? "ログインをやり直す" : "ログイン"}
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
              <input type="hidden" name="adapter" value={adapter} />
              <p className="mb-2 text-sm text-fg-muted">
                本当に <span className="font-mono">{item.id}</span>（{adapter}）を削除しますか？（認証ファイルは消さず
                根ディレクトリの .removed/ に移します）
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
              認可コード・URL は平文 HTTP を通ります（ADR-0024 D7）。信頼できるネットワークでだけ使ってください。
            </Alert>
            <p className="hidden" data-testid="account-login-kind">
              {kind}
            </p>
            {kind === "device_code" ? (
              <>
                {loginStart ? (
                  <p>
                    このリンクを<strong>別のデバイス</strong>のブラウザで開き、下のコードをその画面で入力してください
                    （このコードはここには貼り戻しません）:{" "}
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
                {loginStart?.login.user_code && (
                  <div>
                    <p className={labelClass}>コード（別のデバイスで入力してください）</p>
                    <p
                      data-testid="account-login-user-code"
                      className="mt-1.5 select-all rounded-lg bg-surface-2 px-4 py-3 text-center font-mono text-2xl font-semibold tracking-widest text-fg"
                    >
                      {loginStart.login.user_code}
                    </p>
                  </div>
                )}
                <p className="text-sm text-fg-muted">
                  入力が終わると taskd が自動的に検知し、この画面も自動で更新されます（最大 15
                  分待ちます）。ここにコードを貼り付ける必要はありません。
                </p>
              </>
            ) : (
              <>
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
                  <input type="hidden" name="adapter" value={adapter} />
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
              </>
            )}
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="login_cancel" />
              <input type="hidden" name="id" value={item.id} />
              <input type="hidden" name="adapter" value={adapter} />
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

const USED_BY_SCOPE_LABEL: Record<string, string> = { adapter: "アダプタ", provider: "プロバイダ" };

/**
 * 「API キー」節（ADR-0030 D4）: `[secrets]` から預かった秘密の一覧・追加・更新・削除。
 * `GET /secrets` は管理系のため、トークンが無い構成では `secretsError` になる（画面全体は壊さず、この節だけに
 * 案内を出す。`GET /accounts` 自体は非管理系なのでページ本体は表示できる）。
 */
function SecretsSection({
  secrets,
  secretsError,
  fetcher,
  submitting,
  fetchedAt,
}: {
  secrets: SecretList | null;
  secretsError: ActionError | null;
  fetcher: FetcherWithComponents<SecretActionResult>;
  submitting: boolean;
  fetchedAt: string;
}) {
  return (
    <section id="secrets" aria-labelledby="secrets-heading" className="space-y-4" data-testid="secrets-section">
      <SectionTitle icon="lock" id="secrets-heading" count={secrets?.items.length}>
        API キー
      </SectionTitle>

      <SecretActionFlash result={fetcher.data} />

      {secretsError ? (
        <ErrorFlash error={secretsError} />
      ) : !secrets || secrets.dir == null ? (
        <EmptyState icon="lock" title="[secrets] が設定されていません">
          taskd.toml に <Mono>[secrets]</Mono> セクションを足すと、GUI から API キー（Tavily / Exa 等）を預かれます。
          例:
          <pre className="mt-2 overflow-x-auto rounded-lg bg-surface-2 p-3 text-left text-xs">
            {'[secrets]\ndir = "secrets"'}
          </pre>
        </EmptyState>
      ) : (
        <>
          <Alert tone="warning" title="セキュリティ上の注意">
            値は taskd を動かしているホストに 0600 のファイルとして保存されます。
            <strong className="font-semibold">保存すると値は二度と表示されません</strong>
            （更新・削除だけができます）。値は平文 HTTP を通ります（ADR-0030
            D4）。信頼できるネットワークでだけ使ってください。
          </Alert>

          <DataItem label="dir">
            <Mono>{secrets.dir}</Mono>
          </DataItem>

          {secrets.items.length === 0 ? (
            <EmptyState icon="lock" title="API キーがありません" />
          ) : (
            <div className="grid gap-4 xl:grid-cols-2">
              {secrets.items.map((item) => (
                <SecretCard key={item.id} item={item} fetchedAt={fetchedAt} fetcher={fetcher} submitting={submitting} />
              ))}
            </div>
          )}

          <Card>
            <CardHeader
              icon="plus"
              title="API キーを追加"
              description="id はアダプタ・プロバイダの env_from_secrets が参照する名前と揃えてください（例: tavily、exa）。"
            />
            <CardBody>
              <fetcher.Form method="post" data-testid="secret-add-form" className="flex flex-wrap items-end gap-3">
                <input type="hidden" name="intent" value="secret_put" />
                <div>
                  <label htmlFor="secret-add-id" className={labelClass}>
                    id
                  </label>
                  <input
                    id="secret-add-id"
                    name="id"
                    type="text"
                    required
                    data-testid="secret-add-id"
                    className={`${inputClass} mt-1.5`}
                  />
                </div>
                <div>
                  <label htmlFor="secret-add-value" className={labelClass}>
                    value
                  </label>
                  <input
                    id="secret-add-value"
                    name="value"
                    type="password"
                    autoComplete="off"
                    required
                    data-testid="secret-add-value"
                    className={`${inputClass} mt-1.5`}
                  />
                  <p className={hintClass}>保存後は値を再表示できません。</p>
                </div>
                <Button type="submit" variant="primary" disabled={submitting} data-testid="secret-add-submit">
                  <Icon name="plus" />
                  追加
                </Button>
              </fetcher.Form>
            </CardBody>
          </Card>
        </>
      )}
    </section>
  );
}

function SecretCard({
  item,
  fetchedAt,
  fetcher,
  submitting,
}: {
  item: SecretView;
  fetchedAt: string;
  fetcher: FetcherWithComponents<SecretActionResult>;
  submitting: boolean;
}) {
  const isSet = item.updated_at != null;
  return (
    <Card data-testid="secret-card" data-secret-id={item.id} className="hover:shadow-md">
      <CardHeader
        icon="lock"
        tone={isSet ? "success" : "warning"}
        title={<Mono className="text-sm font-semibold text-fg">{item.id}</Mono>}
        actions={
          isSet ? (
            <Badge tone="success" dot>
              設定済み
            </Badge>
          ) : (
            <Badge tone="warning" dot data-testid="secret-unset">
              未設定
            </Badge>
          )
        }
      />
      <CardBody className="space-y-4">
        <DataItem label="使われている場所" wide>
          {item.used_by.length === 0 ? (
            <span className="text-fg-subtle">-</span>
          ) : (
            <ul className="space-y-1" data-testid="secret-used-by">
              {item.used_by.map((use) => (
                <li key={`${use.scope}-${use.name}-${use.env}`}>
                  <Mono>{use.env}</Mono>
                  <span className="ml-1 text-fg-subtle">
                    （{USED_BY_SCOPE_LABEL[use.scope] ?? use.scope}: {use.name}）
                  </span>
                </li>
              ))}
            </ul>
          )}
        </DataItem>

        <DataItem label="更新時刻">
          <span data-testid="secret-updated-at">
            {item.updated_at ? (
              <>
                {item.updated_at}（{formatDuration(secondsBetween(item.updated_at, fetchedAt))} 前）
              </>
            ) : (
              "未設定（設定はこの秘密を参照していますが、まだ値が入っていません）"
            )}
          </span>
        </DataItem>

        <DataItem label="fingerprint">
          {item.fingerprint ? (
            <>
              <Mono data-testid="secret-fingerprint">{item.fingerprint}</Mono>
              <span className="ml-1 text-xs text-fg-subtle">（値の sha256 の先頭 8 桁。値そのものではありません）</span>
            </>
          ) : (
            <span className="text-fg-subtle">-</span>
          )}
        </DataItem>

        <div className="flex flex-wrap items-end gap-3 border-t border-border pt-3">
          <fetcher.Form method="post" data-testid="secret-update-form" className="flex flex-wrap items-end gap-2">
            <input type="hidden" name="intent" value="secret_put" />
            <input type="hidden" name="id" value={item.id} />
            <div>
              <label htmlFor={`secret-update-value-${item.id}`} className={labelClass}>
                新しい値
              </label>
              <input
                id={`secret-update-value-${item.id}`}
                name="value"
                type="password"
                autoComplete="off"
                required
                data-testid="secret-update-value"
                className={`${inputClass} mt-1.5`}
              />
            </div>
            <Button
              type="submit"
              variant="secondary"
              size="sm"
              disabled={submitting}
              data-testid="secret-update-submit"
            >
              <Icon name="check" />
              更新
            </Button>
          </fetcher.Form>

          <details className="group">
            <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-danger-border bg-danger-soft px-3 text-sm text-danger-soft-fg shadow-xs hover:bg-danger hover:text-white">
              <Icon name="xCircle" className="size-4" />
              削除
            </summary>
            <fetcher.Form method="post" className="mt-3 rounded-lg border border-danger-border bg-danger-soft/40 p-3">
              <input type="hidden" name="intent" value="secret_delete" />
              <input type="hidden" name="id" value={item.id} />
              <p className="mb-2 text-sm text-fg-muted">
                本当に <span className="font-mono">{item.id}</span> を削除しますか？（env_from_secrets がこの id
                を参照するプロバイダ・アダプタは、次の run から鍵無しのエラーになります）
              </p>
              <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="secret-delete">
                <Icon name="xCircle" />
                削除する
              </Button>
            </fetcher.Form>
          </details>
        </div>
      </CardBody>
    </Card>
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
