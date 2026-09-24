import type { SyntheticEvent } from "react";
import { data, type FetcherWithComponents, isRouteErrorResponse, Link, useFetcher } from "react-router";
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
} from "~/celeris/accounts-admin.server";
import type { AccountAdapter, AccountOpOutcome, ActionError, SecretActionResult } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { CelerisError, type CelerisRouteErrorData, celerisErrorResponse, isCelerisUnavailable } from "~/celeris/errors";
import { deleteSecret, putSecret, readSecretId, readSecretValue } from "~/celeris/secrets-admin.server";
import type {
  AccountList,
  AccountView,
  LlmSourceAccountView,
  LlmSourcesView,
  LlmSourceView,
  McpCall,
  McpCallsView,
  McpClient,
  McpClientsView,
  Providers,
  ProviderView,
  SecretList,
  SecretView,
  Tier,
} from "~/celeris/types";
import { AccountActionFlash, ErrorFlash, SecretActionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { LocalTime } from "~/components/LocalTime";
import { RouteRecovery } from "~/components/RouteRecovery";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, selectClass, touchLinkClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, CopyButton, DataItem, EmptyState, Mono, PageHeader, PageToc, SectionTitle } from "~/components/ui/misc";
import { Skeleton } from "~/components/ui/skeleton";
import { TONE_SOLID_BG, type Tone } from "~/components/ui/tone";
import { shortId } from "~/lib/format";
import {
  allAccountsCoolingDown,
  cooldownRemainingLabel,
  cooldownUntilTitle,
  formatRemaining,
  isAccountCoolingDown,
  sourceLabel,
  sourceStatusWord,
  tierLabel,
  tierResolutionLabel,
  tierResolutionReason,
  tierResolutionReasonLabel,
} from "~/lib/llm-sources";
import { mcpAuthKindWord, mcpClientStatusWord, mcpConnectionUrlHint, mcpScopeLabel, sortMcpScopes } from "~/lib/mcp";
import { isTransientStatus } from "~/lib/recovery";
import { formatDuration, secondsBetween } from "~/lib/time-delta";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/accounts";

/**
 * `/accounts`（Claude Code / Codex アカウントのプール、ADR-GUI-0012 D3、docs/celeris-api-v1.md §3.29）。
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
  /** `GET /llm/sources`（ADR-0053 D4、Phase 66）。`[llm_proxy]` が無効なら 409
   * `llm_proxy_unavailable` になるので `llmSourcesUnavailable` に落とし、それ以外の失敗だけ
   * `llmSourcesError` に入れる（`/accounts` 自体は落とさない。secrets と同じ扱い）。 */
  llmSources: LlmSourcesView | null;
  llmSourcesUnavailable: boolean;
  llmSourcesError: ActionError | null;
  /** `GET /mcp/clients`（ADR-0056 D4、Phase 78/80）。読み取り専用だが `GET /llm/sources` と同じ規律で
   * トークンが要る。落ちても `/accounts` 自体は壊さず、この節だけに案内を出す（secrets と同じ扱い）。 */
  mcpClients: McpClientsView | null;
  mcpClientsError: ActionError | null;
  /** ADR-0069 Phase 118 D3: `GET /providers` を読み取り専用で読み、`tier_models`（プロバイダごとの
   * tier → 実行モデル/effort）を表示する。編集はこれまでどおり `/providers` 画面で行う（ここには
   * フォームを置かない）。落ちても `/accounts` 自体は壊さない（secrets と同じ扱い）。 */
  providers: Providers | null;
  providersError: ActionError | null;
  fetchedAt: string;
}

/**
 * `CelerisError` / `CelerisUnavailable`（`GET /secrets` の 401 等）を `ActionError` にする。`actions.server.ts` の
 * `toActionError` と同じ変換だが、`loadAccounts` はテストから loader を介さず直接呼ばれるため、サーバ専用
 * モジュールを `loader`/`action` 以外の export から参照できない制約（React Router の dot-server 除去）を避けて
 * ここに複製する（`GET /secrets` が返すのは 401/409/503 のみで `errors[]` を持たないので fields/messages は空でよい）。
 */
function secretsListError(e: unknown): ActionError {
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

export async function loadAccounts(client: CelerisClient, request: Request): Promise<AccountsData> {
  const accounts = await client.get<AccountList>("/accounts", { signal: request.signal });
  let secrets: SecretList | null = null;
  let secretsError: ActionError | null = null;
  try {
    secrets = await client.get<SecretList>("/secrets", { signal: request.signal });
  } catch (e) {
    secretsError = secretsListError(e);
  }
  let llmSources: LlmSourcesView | null = null;
  let llmSourcesUnavailable = false;
  let llmSourcesError: ActionError | null = null;
  try {
    llmSources = await client.get<LlmSourcesView>("/llm/sources", { signal: request.signal });
  } catch (e) {
    if (e instanceof CelerisError && e.status === 409 && e.code === "llm_proxy_unavailable") {
      llmSourcesUnavailable = true;
    } else {
      llmSourcesError = secretsListError(e);
    }
  }
  let mcpClients: McpClientsView | null = null;
  let mcpClientsError: ActionError | null = null;
  try {
    mcpClients = await client.get<McpClientsView>("/mcp/clients", { signal: request.signal });
  } catch (e) {
    mcpClientsError = secretsListError(e);
  }
  let providers: Providers | null = null;
  let providersError: ActionError | null = null;
  try {
    providers = await client.get<Providers>("/providers", { signal: request.signal });
  } catch (e) {
    providersError = secretsListError(e);
  }
  return {
    accounts,
    secrets,
    secretsError,
    llmSources,
    llmSourcesUnavailable,
    llmSourcesError,
    mcpClients,
    mcpClientsError,
    providers,
    providersError,
    fetchedAt: new Date().toISOString(),
  };
}

export async function loader({ request }: Route.LoaderArgs): Promise<AccountsData> {
  try {
    return await loadAccounts(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
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
  const client = getCelerisClient();

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
  five_hour_exhausted: "短期枠を使い切りました",
  seven_day_exhausted: "長期枠を使い切りました",
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
 * `roots` が無い（古い celeris）場合は `root`（claude-code の別名）だけにフォールバックする。
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
  const {
    accounts,
    secrets,
    secretsError,
    llmSources,
    llmSourcesUnavailable,
    llmSourcesError,
    mcpClients,
    mcpClientsError,
    providers,
    providersError,
    fetchedAt,
  } = loaderData;
  // celeris の SSE（daemon tick）による自動再検証のたびに `<Form>` の actionData は消える（React Router の仕様、
  // `app/hooks/useCelerisStream.ts`）。ログイン URL は「もう一度出せない」ものなので特に影響が大きい: 1 つの
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
        description="Claude／GPT のログイン・サブスクリプション・APIキーを管理します。実行モデルと階層はプロバイダ画面で設定し、この画面のアカウントIDまたはAPIキーIDを参照します。"
      />

      <p className="text-sm text-fg-muted">
        Codex の利用枠は推論を使わず定期更新します。「確認」で今すぐ更新できます。API キー認証には ChatGPT
        の利用枠はありません。
      </p>
      <AccountActionFlash outcome={fetcher.data} />

      {/* Phase 95（ADR-0055 ラウンド 19、所見、重さ「高」）: このページは
          アカウント（Claude/Codex ごとに複数枚のカード）→ プロバイダのモデル階層 → LLM source →
          MCP クライアント → API キー、と性質の違う節が縦に並ぶ長い 1 ページ（実測で 4 画面分超）。
          `/help` と同じ「目次から飛ぶ」パターン（`PageToc`）を、アカウント一覧より後ろの節
          （スクロールで埋もれやすい節）にだけ足す。既存の `id`（`llm-sources-heading` 等）や
          節の実装には触れない。 */}
      <PageToc
        label="アカウントの目次"
        items={[
          { id: "provider-tier-models-heading", icon: "cpu", label: "モデル階層" },
          { id: "llm-sources-heading", icon: "server", label: "LLM source" },
          { id: "mcp-clients-heading", icon: "network", label: "MCP クライアント" },
          { id: "secrets-heading", icon: "lock", label: "API キー" },
        ]}
      />

      {(() => {
        const configured = configuredAdapters(accounts);
        if (configured.length === 0) {
          return (
            <EmptyState icon="users" title="[accounts] が設定されていません">
              config.toml に <Mono>[accounts]</Mono> セクションを足すとプールが使えます（<Mono>claude_dir</Mono>・
              <Mono>codex_dir</Mono> のどちらか、または両方）。例:
              <pre className="mt-2 overflow-x-auto rounded-lg bg-surface-2 p-3 text-left font-mono text-xs">
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
                  <SectionTitle
                    icon="users"
                    id={`accounts-heading-${adapter}`}
                    count={items.length}
                    className="flex-wrap"
                  >
                    <Badge tone={ADAPTER_TONE[adapter]}>{adapter}</Badge>
                    <span className="min-w-0 basis-full break-all sm:basis-auto">プール（{root}）</span>
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

      <ProviderTierModelsSection providers={providers} error={providersError} />

      <LlmSourcesSection
        llmSources={llmSources}
        unavailable={llmSourcesUnavailable}
        error={llmSourcesError}
        fetchedAt={fetchedAt}
      />

      <McpClientsSection mcpClients={mcpClients} error={mcpClientsError} fetchedAt={fetchedAt} />

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

const PROVIDER_TIER_ORDER: Tier[] = ["frontier", "standard", "cheap"];

/**
 * 「モデル階層」節（ADR-0069 Phase 118 D3）。`GET /providers` の `tier_models`（プロバイダごとの
 * tier → 実行モデル/reasoning effort）をそのまま表にする。`/providers` 画面のプロバイダカードに
 * ある同じ情報の読み取り専用の要約で、編集フォームはここには置かない（編集は `/providers` で行う）。
 */
function ProviderTierModelsSection({ providers, error }: { providers: Providers | null; error: ActionError | null }) {
  const items = providers?.items ?? [];
  const withTierModels = items.filter((item) => Object.keys(item.tier_models ?? {}).length > 0);
  return (
    <section
      id="provider-tier-models"
      aria-labelledby="provider-tier-models-heading"
      className="space-y-4"
      data-testid="provider-tier-models-section"
    >
      <SectionTitle icon="cpu" id="provider-tier-models-heading" count={withTierModels.length}>
        モデル階層
      </SectionTitle>

      {error ? (
        <ErrorFlash error={error} />
      ) : withTierModels.length === 0 ? (
        <EmptyState icon="cpu" title="階層別モデルが設定されたプロバイダがありません">
          <Link to="/providers" className={touchLinkClass}>
            プロバイダ画面
          </Link>
          で <Mono>tier_models</Mono> を設定すると、tier ごとの実行モデルがここに出ます。
        </EmptyState>
      ) : (
        <div className="grid items-start gap-4 xl:grid-cols-2">
          {withTierModels.map((item) => (
            <ProviderTierModelsCard key={item.id} item={item} />
          ))}
        </div>
      )}
    </section>
  );
}

function ProviderTierModelsCard({ item }: { item: ProviderView }) {
  return (
    <Card data-testid="provider-tier-models-row" data-provider-id={item.id}>
      <CardHeader
        icon="cpu"
        title={<Mono className="text-sm font-semibold text-fg">{item.id}</Mono>}
        description={
          item.adapter === "codex" ? "GPT (Codex)" : item.adapter === "claude-code" ? "Claude" : item.adapter
        }
      />
      <CardBody>
        <dl className="space-y-2 text-sm">
          {PROVIDER_TIER_ORDER.map((tier) => {
            const binding = item.tier_models?.[tier];
            return (
              <div key={tier} className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                <dt className="w-20 shrink-0 font-medium text-fg-muted">{tier}</dt>
                <dd className="min-w-0 break-all" data-testid={`provider-tier-models-${item.id}-${tier}`}>
                  {binding ? (
                    <>
                      {binding.name}
                      {" -> "}
                      {binding.unavailable_reason ? (
                        <span className="text-danger-soft-fg">unavailable: {binding.unavailable_reason}</span>
                      ) : binding.model_id ? (
                        <Mono>{binding.model_id}</Mono>
                      ) : (
                        <span className="text-fg-subtle">(model_id 未設定)</span>
                      )}
                      {binding.reasoning_effort && (
                        <Badge tone="neutral" className="ml-1.5">
                          effort={binding.reasoning_effort}
                        </Badge>
                      )}
                    </>
                  ) : (
                    <span className="text-fg-subtle">(未設定)</span>
                  )}
                </dd>
              </div>
            );
          })}
        </dl>
      </CardBody>
    </Card>
  );
}

/**
 * 「LLM source」節（ADR-0053 D4、Phase 66）。`GET /llm/sources` をそのまま表示する:
 * 供給元ごとの到達性・アカウントの残量（短期/長期）・cooldown・直近 1 時間の要求/token 数と、
 * `celeris/<tier>` が今どこに解決するか。判断（選択・cooldown）は celeris の中で決まっているので、
 * ここでは値の整形だけ（`~/lib/llm-sources.ts`）。モバイル幅（393px）でも横はみ出しが出ないよう
 * カードは流動的な幅にする（ADR-0055 D2）。
 */
function LlmSourcesSection({
  llmSources,
  unavailable,
  error,
  fetchedAt,
}: {
  llmSources: LlmSourcesView | null;
  unavailable: boolean;
  error: ActionError | null;
  fetchedAt: string;
}) {
  const nowSec = Math.floor(new Date(fetchedAt).getTime() / 1000);
  return (
    <section
      id="llm-sources"
      aria-labelledby="llm-sources-heading"
      className="space-y-4"
      data-testid="llm-sources-section"
    >
      <SectionTitle icon="server" id="llm-sources-heading" count={llmSources?.sources.length}>
        LLM source
      </SectionTitle>

      {error ? (
        <ErrorFlash error={error} />
      ) : unavailable || !llmSources ? (
        <EmptyState icon="server" title="[llm_proxy] が設定されていません">
          config.toml に <Mono>[llm_proxy]</Mono> セクションと 1 つ以上の供給元 （<Mono>claude_oauth</Mono> /{" "}
          <Mono>codex_oauth</Mono> / <Mono>openai_compatible</Mono>）を足すと、
          <Mono>celeris/&lt;tier&gt;</Mono> の抽象プロキシと、この節の可視化が使えます（
          <Mono>docs/llm-source.md</Mono>）。
        </EmptyState>
      ) : (
        <>
          {(llmSources.celeris_tiers ?? []).length > 0 && (
            <dl className="grid grid-cols-1 gap-x-4 gap-y-3 text-sm sm:grid-cols-3" data-testid="llm-tier-resolution">
              {(llmSources.celeris_tiers ?? []).map((t) => {
                const reason = tierResolutionReason(t.resolves_to, llmSources.sources, nowSec);
                return (
                  <DataItem key={t.tier} label={`celeris/${tierLabel(t.tier)}`}>
                    <span data-testid={`llm-tier-resolves-${t.tier}`}>{tierResolutionLabel(t.resolves_to)}</span>
                    <span className="block text-sm text-fg-subtle" data-testid={`llm-tier-reason-${t.tier}`}>
                      {tierResolutionReasonLabel(reason)}
                    </span>
                  </DataItem>
                );
              })}
            </dl>
          )}

          {(() => {
            const claude = llmSources.sources.find((s) => s.id === "claude-oauth");
            return (
              claude &&
              allAccountsCoolingDown(claude.accounts, nowSec) && (
                <Alert
                  tone="warning"
                  title="Claude のアカウントが全て cooldown 中です"
                  data-testid="llm-claude-all-cooldown"
                >
                  <p>
                    復帰するまで <Mono className="text-xs">claude/&lt;tier&gt;</Mono> は使えません。無料の Qwen
                    が届いていれば <Mono className="text-xs">celeris/&lt;tier&gt;</Mono> はそちらへ倒れます
                    （届いていなければ Codex の GPT に倒れます）。
                  </p>
                </Alert>
              )
            );
          })()}

          {llmSources.sources.length === 0 ? (
            <EmptyState icon="server" title="供給元がありません" />
          ) : (
            <div className="grid gap-4 xl:grid-cols-2">
              {llmSources.sources.map((source) => (
                <LlmSourceCard key={source.id} source={source} nowSec={nowSec} />
              ))}
            </div>
          )}
        </>
      )}
    </section>
  );
}

function LlmSourceCard({ source, nowSec }: { source: LlmSourceView; nowSec: number }) {
  const statusWord = sourceStatusWord(source);
  const tone: Tone = statusWord === "reachable" ? "success" : statusWord === "unreachable" ? "danger" : "neutral";
  return (
    <Card data-testid="llm-source-row" data-source-id={source.id} className="min-w-0 hover:shadow-md">
      <CardHeader
        icon="server"
        tone={tone}
        title={<span className="break-all">{sourceLabel(source.id)}</span>}
        description={<Mono className="break-all text-xs text-fg-subtle">{source.id}</Mono>}
        actions={
          <Badge tone={tone} dot data-status-badge="llm-source" data-testid="llm-source-status">
            {statusWord}
          </Badge>
        }
      />
      <CardBody className="space-y-4">
        {statusWord === "unreachable" && source.unreachable_reason && (
          <p className="break-all text-sm text-fg-muted" data-testid="llm-source-unreachable-reason">
            {source.unreachable_reason}
          </p>
        )}
        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm sm:grid-cols-3">
          <DataItem label="直近1時間 要求">
            <span data-testid="llm-source-requests">{source.last_hour_requests}</span>
          </DataItem>
          <DataItem label="直近1時間 入力token">
            <span data-testid="llm-source-prompt-tokens">{source.last_hour_prompt_tokens}</span>
          </DataItem>
          <DataItem label="直近1時間 出力token">
            <span data-testid="llm-source-completion-tokens">{source.last_hour_completion_tokens}</span>
          </DataItem>
        </dl>

        {source.accounts.length > 0 && (
          <div className="space-y-2">
            {source.accounts.map((account) => (
              <LlmAccountRow key={account.id} account={account} nowSec={nowSec} />
            ))}
          </div>
        )}
      </CardBody>
    </Card>
  );
}

function LlmAccountRow({ account, nowSec }: { account: LlmSourceAccountView; nowSec: number }) {
  const cooling = isAccountCoolingDown(account, nowSec);
  return (
    <div
      className="flex min-w-0 flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-surface-2 px-3 py-2 text-sm"
      data-testid="llm-source-account"
    >
      <span className="flex min-w-0 items-center gap-2">
        <Mono className="break-all" title={account.id}>
          {shortId(account.id)}
        </Mono>
        {!account.logged_in && <Badge tone="neutral">未ログイン</Badge>}
      </span>
      <span className="flex flex-wrap items-center gap-3 text-fg-muted">
        <span data-testid="llm-account-remaining-short">短期 {formatRemaining(account.remaining_short)}</span>
        <span data-testid="llm-account-remaining-long">長期 {formatRemaining(account.remaining_long)}</span>
        {cooling && account.cooldown_until != null && (
          <Badge tone="warning" title={cooldownUntilTitle(account.cooldown_until)} data-testid="llm-account-cooldown">
            cooldown {cooldownRemainingLabel(account.cooldown_until, nowSec)}
          </Badge>
        )}
      </span>
    </div>
  );
}

/**
 * 「MCP クライアント」節（ADR-0056 D4、GUI Phase 80）: `GET /mcp/clients` をそのまま表示する
 * （`LlmSourcesSection` と同じ作り。値の再計算はしない。判断＝認証・スコープ・流量制限は
 * `crates/celeris-mcp` の中で決まっている）。「接続のしかた」は `/help#mcp`（`docs/mcp.md` の要約）
 * へのリンクだけ持つ。
 */
function McpClientsSection({
  mcpClients,
  error,
  fetchedAt,
}: {
  mcpClients: McpClientsView | null;
  error: ActionError | null;
  fetchedAt: string;
}) {
  return (
    <section
      id="mcp-clients"
      aria-labelledby="mcp-clients-heading"
      className="space-y-4"
      data-testid="mcp-clients-section"
    >
      <SectionTitle icon="network" id="mcp-clients-heading" count={mcpClients?.items.length}>
        MCP クライアント
        <HelpLink anchor="mcp" label="MCP で外から使う" />
      </SectionTitle>

      <p className="text-sm text-fg-muted" data-testid="mcp-clients-hint">
        外部エージェント（ChatGPT・Claude Code・Codex 等）が MCP 経由で Celeris を操作するときの入口です。接続のしかたは
        <Link to="/help#mcp" className={`${touchLinkClass} mx-1 underline underline-offset-2 hover:text-fg`}>
          使い方の「MCP で外から使う」
        </Link>
        （<Mono>docs/mcp.md</Mono>）を参照してください。
      </p>

      {error ? (
        <ErrorFlash error={error} />
      ) : !mcpClients || mcpClients.items.length === 0 ? (
        <EmptyState icon="network" title="MCP クライアントがありません">
          <Mono>celerisctl mcp client add &lt;name&gt;</Mono> で客を発行すると、ここに並びます。
        </EmptyState>
      ) : (
        <div className="grid gap-4 xl:grid-cols-2">
          {mcpClients.items.map((c) => (
            <McpClientCard key={c.id} client={c} fetchedAt={fetchedAt} />
          ))}
        </div>
      )}
    </section>
  );
}

function McpClientCard({ client, fetchedAt }: { client: McpClient; fetchedAt: string }) {
  const authWord = mcpAuthKindWord(client);
  const statusWord = mcpClientStatusWord(client);
  const scopes = sortMcpScopes(client.scopes);
  return (
    <Card data-testid="mcp-client-card" data-client-id={client.id} className="min-w-0 hover:shadow-md">
      <CardHeader
        icon="terminal"
        tone={statusWord === "revoked" ? "neutral" : "info"}
        title={<span className="break-all text-sm font-semibold text-fg">{client.name}</span>}
        description={<Mono className="break-all text-xs text-fg-subtle">{client.id}</Mono>}
        actions={
          <>
            <Badge
              tone={authWord === "token" ? "info" : "neutral"}
              data-status-badge="mcp-auth"
              data-testid="mcp-client-auth"
            >
              {authWord}
            </Badge>
            <Badge
              tone={statusWord === "revoked" ? "danger" : "success"}
              dot
              data-status-badge="mcp-client"
              data-testid="mcp-client-status"
            >
              {statusWord}
            </Badge>
          </>
        }
      />
      <CardBody className="space-y-4">
        {scopes.length > 0 && (
          <div>
            <p className={labelClass}>スコープ</p>
            <div className="mt-1.5 flex flex-wrap gap-1.5" data-testid="mcp-client-scopes">
              {scopes.map((s) => (
                <Badge key={s} tone="neutral" title={s} data-testid="mcp-client-scope">
                  {mcpScopeLabel(s)}
                </Badge>
              ))}
            </div>
          </div>
        )}

        <dl className="grid grid-cols-1 gap-x-4 gap-y-3 text-sm sm:grid-cols-2">
          <DataItem label="created_at">
            <LocalTime iso={client.created_at} fetchedAtIso={fetchedAt} />
          </DataItem>
          <DataItem label="last_used_at">
            {client.last_used_at ? (
              <LocalTime iso={client.last_used_at} fetchedAtIso={fetchedAt} dataTestId="mcp-client-last-used" />
            ) : (
              <span className="text-fg-subtle">未使用</span>
            )}
          </DataItem>
          {/* Phase 84: 接続 URL のヒント（`docs/mcp.md` §2 の既定値。トークンは絶対に出さない）。
              コピーしてクライアント側の設定にそのまま貼れるように `CopyButton` を添える。 */}
          <DataItem label="接続 URL のヒント" wide>
            <span className="flex flex-wrap items-center gap-2" data-testid="mcp-client-url-hint">
              <Mono className="break-all">{mcpConnectionUrlHint(client)}</Mono>
              <CopyButton value={mcpConnectionUrlHint(client)} label="URL" />
            </span>
          </DataItem>
        </dl>

        <McpClientCallsDisclosure clientId={client.id} fetchedAt={fetchedAt} />
      </CardBody>
    </Card>
  );
}

/**
 * 客 1 件の直近の呼び出し（`GET /mcp/clients/:id/calls` → `GET /mcp/calls?client=`。ADR-0056 D4）。
 * 開いたときだけ取りに行く（`ProgressBlockView` の「すべて見る」と同じ作り。`~/routes/mcp.clients.$id.calls.ts`）。
 */
function McpClientCallsDisclosure({ clientId, fetchedAt }: { clientId: string; fetchedAt: string }) {
  const fetcher = useFetcher<McpCallsView>();
  const loaded = fetcher.data != null;
  function onToggle(e: SyntheticEvent<HTMLDetailsElement>) {
    if (e.currentTarget.open && fetcher.state === "idle" && !loaded) {
      fetcher.load(`/mcp/clients/${encodeURIComponent(clientId)}/calls`);
    }
  }
  return (
    <details className="group border-t border-border pt-3" data-testid="mcp-client-calls" onToggle={onToggle}>
      <summary className="flex min-h-11 w-full cursor-pointer list-none items-center gap-1.5 text-sm font-medium text-fg-subtle hover:text-fg">
        <Icon name="chevronRight" className="size-3.5 shrink-0 transition-transform group-open:rotate-90" />
        直近の呼び出し
      </summary>
      <div className="mt-2">
        {fetcher.state !== "idle" ? (
          <McpCallListSkeleton />
        ) : !fetcher.data ? null : fetcher.data.items.length === 0 ? (
          <p className="text-sm text-fg-subtle">呼び出しはまだありません。</p>
        ) : (
          <ul className="space-y-1.5" data-testid="mcp-call-list">
            {fetcher.data.items.map((call) => (
              <McpCallRow key={call.id} call={call} fetchedAt={fetchedAt} />
            ))}
          </ul>
        )}
      </div>
    </details>
  );
}

/** 直近の呼び出しの読み込み中（Phase 84）。`useFetcher().load()` の応答を待つ間の骨組み。 */
function McpCallListSkeleton() {
  return (
    <ul className="space-y-1.5" aria-hidden="true" data-testid="mcp-call-list-skeleton">
      {[0, 1].map((i) => (
        <li key={i} className="rounded-lg border border-border bg-surface-2 px-3 py-2">
          <Skeleton className="h-4 w-2/3" />
        </li>
      ))}
    </ul>
  );
}

function McpCallRow({ call, fetchedAt }: { call: McpCall; fetchedAt: string }) {
  return (
    <li
      className="flex min-w-0 flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-surface-2 px-3 py-2 text-sm"
      data-testid="mcp-call-row"
    >
      <span className="flex min-w-0 items-center gap-2">
        <Mono className="break-all">{call.tool}</Mono>
        <Badge tone={call.ok ? "success" : "danger"} dot data-status-badge="mcp-call" data-testid="mcp-call-status">
          {call.ok ? "ok" : "error"}
        </Badge>
      </span>
      <span className="flex flex-wrap items-center gap-3 text-fg-muted">
        {/* Phase 84: エラーの種類も 1 語のバッジに揃える（ADR-0055 D1-3 と同じ規律。地の文にしない）。 */}
        {!call.ok && call.error_kind && (
          <Badge tone="danger" data-status-badge="mcp-call-error" data-testid="mcp-call-error-kind">
            {call.error_kind}
          </Badge>
        )}
        <span data-testid="mcp-call-latency">{call.latency_ms}ms</span>
        <LocalTime iso={call.at} fetchedAtIso={fetchedAt} />
      </span>
    </li>
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
      className="min-w-0 hover:shadow-md"
    >
      <CardHeader
        className="flex-wrap [&>div:last-child]:w-full sm:[&>div:last-child]:w-auto"
        icon="user"
        tone={tone}
        title={<Mono className="break-all text-sm font-semibold text-fg">{item.id}</Mono>}
        description={<span className="break-all">{item.dir}</span>}
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
          label="短期枠"
          testId="account-usage-five-hour"
          window={item.usage?.five_hour ?? null}
          fetchedAt={fetchedAt}
        />
        <UsageBar
          label="長期枠"
          testId="account-usage-seven-day"
          window={item.usage?.seven_day ?? null}
          fetchedAt={fetchedAt}
        />

        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm sm:grid-cols-3">
          <DataItem label="status">{item.usage?.status ?? "-"}</DataItem>
          <DataItem label="observed_at" wide>
            {item.usage ? (
              <>
                {item.usage.observed_at}（<LocalTime iso={item.usage.observed_at} fetchedAtIso={fetchedAt} />・
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
              コードは celeris も保存しないので、失ったら開始し直すしかない（前のコードは無効になる）。 */}
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
                  入力が終わると celeris が自動的に検知し、この画面も自動で更新されます（最大 15
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
        <div className="flex items-center justify-between text-sm text-fg-subtle lg:text-xs">
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
      <div className="flex items-center justify-between text-sm text-fg-muted lg:text-xs">
        <span>{label}</span>
        <span className="tabular-nums">
          使用 {pct}% / 残り {Math.max(0, 100 - pct)}%
        </span>
      </div>
      <div className="mt-1 h-2 w-full overflow-hidden rounded-full bg-surface-2">
        <div className={`h-full rounded-full ${TONE_SOLID_BG[tone]}`} style={{ width: `${Math.min(100, pct)}%` }} />
      </div>
      <p className="mt-1 text-sm text-fg-subtle lg:text-xs">
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
          config.toml に <Mono>[secrets]</Mono> セクションを足すと、GUI から API キー（Tavily / Exa 等）を預かれます。
          例:
          <pre className="mt-2 overflow-x-auto rounded-lg bg-surface-2 p-3 text-left font-mono text-xs">
            {'[secrets]\ndir = "secrets"'}
          </pre>
        </EmptyState>
      ) : (
        <>
          <Alert tone="warning" title="セキュリティ上の注意">
            値は celeris を動かしているホストに 0600 のファイルとして保存されます。
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
    <Card data-testid="secret-card" data-secret-id={item.id} className="min-w-0 hover:shadow-md">
      <CardHeader
        icon="lock"
        tone={isSet ? "success" : "warning"}
        title={<Mono className="break-all text-sm font-semibold text-fg">{item.id}</Mono>}
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
                {item.updated_at}（<LocalTime iso={item.updated_at} fetchedAtIso={fetchedAt} />）
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
              <span className="ml-1 text-sm text-fg-subtle lg:text-xs">
                （値の sha256 の先頭 8 桁。値そのものではありません）
              </span>
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
 * loader が `celerisErrorResponse` で投げた `Response` を判別する（`/providers` と同じ方針）。
 */
export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const errorData = error.data as CelerisRouteErrorData;
    if (errorData.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={errorData.baseUrl ?? ""} problem={null} />
          <RouteRecovery />
        </main>
      );
    }
    return (
      <main className="p-4">
        <h1 className="text-xl font-semibold">エラー {errorData.status}</h1>
        <p className="mt-2 text-sm text-fg-muted">{errorData.detail}</p>
        {isTransientStatus(errorData.status) && <RouteRecovery />}
      </main>
    );
  }

  return (
    <main className="p-4">
      <h1 className="text-xl font-semibold">エラー</h1>
      <p className="mt-2 text-sm text-fg-muted">予期しないエラーが起きました。</p>
      <RouteRecovery />
    </main>
  );
}
