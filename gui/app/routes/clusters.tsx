import { data, type FetcherWithComponents, isRouteErrorResponse, useFetcher } from "react-router";
import type { ClusterConnectOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import {
  cancelClusterConnect,
  readClusterConnectCode,
  readClusterId,
  startClusterConnect,
  submitClusterConnectCode,
} from "~/celeris/clusters-admin.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import type { ClusterForwardView, Clusters, ClusterView } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { forwardStatusWord } from "~/lib/llm-sources";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/clusters";

/**
 * `/clusters`（クラスタ画面、docs/DESIGN.md §10 Phase G7、接続は ADR-0032 / Phase 22）の loader が返すデータ。
 * `Clusters.items[]`（`ClusterView`）をそのまま表にする。cooldown の残り秒数、`auth`、`connect_pending` は
 * celeris がすでに計算済みなので、GUI 側で再計算しない。
 */
export interface ClustersData {
  clusters: Clusters;
}

/** `GET /clusters` を呼ぶ。応答はそのまま返す（派生の集計はしない）。 */
export async function loadClusters(client: CelerisClient, request: Request): Promise<ClustersData> {
  const clusters = await client.get<Clusters>("/clusters", { signal: request.signal });
  return { clusters };
}

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。接続の action（ADR-0032）も同じ規約に揃える。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ClustersData> {
  try {
    return await loadClusters(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "クラスタ - Celeris" }];
}

/**
 * クラスタへの接続の中継（ADR-0032 D5/D6）。`clusters-admin.server.ts` に判断ロジックは無く、
 * フォームの `intent` を対応する呼び出しに写すだけ。**`POST /reload` は呼ばない**（接続を張っても
 * `config.toml` の設定は変わらないので不要。プロバイダ・秘密の管理とはここが違う）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getCelerisClient();
  const id = readClusterId(form);

  let outcome: ClusterConnectOutcome;
  switch (intent) {
    case "cluster_connect":
      outcome = await startClusterConnect(client, id, request.signal);
      break;
    case "cluster_connect_code":
      outcome = await submitClusterConnectCode(client, id, readClusterConnectCode(form), request.signal);
      break;
    case "cluster_connect_cancel":
      outcome = await cancelClusterConnect(client, id, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export default function ClustersPage({ loaderData }: Route.ComponentProps) {
  const { clusters } = loaderData;
  const fetcher = useFetcher<ClusterConnectOutcome>();
  const submitting = fetcher.state !== "idle";

  return (
    <div className="space-y-8">
      <PageHeader
        icon="server"
        title={
          <>
            クラスタ
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="リモートクラスタの接続状態・並列度・cooldown をまとめて確認します。"
      />

      <section aria-labelledby="clusters-heading" data-testid="clusters-section" className="space-y-4">
        <SectionTitle icon="server" id="clusters-heading" count={clusters.items.length}>
          クラスタ一覧
        </SectionTitle>
        {clusters.items.length === 0 ? (
          <EmptyState icon="server" title="クラスタがありません">
            `[[clusters]]` を設定すると、接続状態や並列度がここに表示されます。
          </EmptyState>
        ) : (
          <div className="grid gap-4 xl:grid-cols-2">
            {clusters.items.map((item) => (
              <ClusterCard key={item.id} item={item} fetcher={fetcher} submitting={submitting} />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

/** `ClusterView.auth` を 3 通りに正規化する（未設定・未知の値は `"manual"`。既定と同じ、ADR-0032 D1）。 */
export type ClusterAuthKind = "manual" | "publickey" | "totp";

export function clusterAuthKind(auth: string | null | undefined): ClusterAuthKind {
  return auth === "publickey" || auth === "totp" ? auth : "manual";
}

/**
 * 接続パネルのどの部分を出すかを決める（ADR-0032 D6）。**描画から切り離して単体テストできるようにしてある**:
 * 一度「進行中だと入力欄に戻れなくなる」不具合を実機で出したため（`gui/test/unit/clusters.test.ts`）。
 */
export function clusterConnectPanelState(input: {
  connected: boolean | null | undefined;
  auth: ClusterAuthKind;
  connectPending: boolean;
  needsCode: boolean;
  hasCodeResult: boolean;
}): { showCodeForm: boolean; showPendingElsewhere: boolean; showConnectButton: boolean } {
  const notConnected = input.connected !== true;
  // このタブで「検証コードが要る」接続をちょうど開始し、まだコードの送信結果が付いていないときだけ
  // プロンプト・入力欄を出す。コード送信が済んだら閉じ、もう一度「接続」からやり直す（D4 手順 5）。
  const showCodeForm = input.auth === "totp" && notConnected && input.needsCode && !input.hasCodeResult;
  // `connect_pending` は celeris 側のセッションの有無（このタブに限らない）。このタブで開始したのでなければ
  // プロンプト文字列を持てない（D5: プロンプトは `POST` の応答にしか載らない）ので、通知だけ出す。
  const showPendingElsewhere = input.connectPending && !showCodeForm && notConnected;
  // **進行中でも接続ボタンは出す**。celeris は `connect` を受けると古いセッションを畳んでから張り直すので、
  // 押し直せば入力欄に戻れる（`/accounts` のログインと同じ扱い）。ここを `!showPendingElsewhere` にすると、
  // 画面を開き直しただけで「進行中」から抜け出せなくなる。
  const showConnectButton = notConnected && input.auth !== "manual" && !showCodeForm;
  return { showCodeForm, showPendingElsewhere, showConnectButton };
}

function ClusterCard({
  item,
  fetcher,
  submitting,
}: {
  item: ClusterView;
  fetcher: FetcherWithComponents<ClusterConnectOutcome>;
  submitting: boolean;
}) {
  const tone: Tone = item.connected === true ? "success" : item.connected === false ? "danger" : "neutral";
  const connectedLabel = item.connected === true ? "connected" : item.connected === false ? "disconnected" : "-";
  const auth = clusterAuthKind(item.auth);
  const pendingElsewhereCandidate = item.connect_pending === true;

  const actionData = fetcher.data;
  const own = actionData && actionData.id === item.id ? actionData : undefined;
  const startResult = own?.ok && own.op === "connect_start" ? own.start : undefined;
  const startError = own && !own.ok && own.op === "connect_start" ? own.error : undefined;
  const codeResult = own?.ok && own.op === "connect_code" ? own.result : undefined;
  const codeError = own && !own.ok && own.op === "connect_code" ? own.error : undefined;
  const cancelError = own && !own.ok && own.op === "connect_cancel" ? own.error : undefined;

  // このタブで「検証コードが要る」接続をちょうど開始し、まだコードの送信結果が付いていない状態のときだけ
  // プロンプト・入力欄を出す。コード送信が失敗しても celeris 側でセッションは終わる（ADR-0032 D4 手順 5）ので、
  // `codeResult` が付いたらこのパネルは閉じ、もう一度「接続」を押すところからやり直す。
  const { showCodeForm, showPendingElsewhere, showConnectButton } = clusterConnectPanelState({
    connected: item.connected,
    auth,
    connectPending: pendingElsewhereCandidate,
    needsCode: startResult?.kind === "needs_code",
    hasCodeResult: Boolean(codeResult),
  });

  return (
    <Card data-testid="cluster-row" data-cluster-id={item.id} className="hover:shadow-md">
      <CardHeader
        icon="server"
        tone={tone}
        title={
          <Mono className="text-sm font-semibold text-fg" data-testid="cluster-id">
            {item.id}
          </Mono>
        }
        description={
          <span data-testid="cluster-host" className="break-all">
            {item.host}
          </span>
        }
        actions={
          <>
            <Badge tone="neutral" data-testid="cluster-auth">
              {auth}
            </Badge>
            <Badge tone={tone} dot pulse={item.connected === true} data-testid="cluster-connected">
              {connectedLabel}
            </Badge>
          </>
        }
      />
      <CardBody className="space-y-4">
        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm sm:grid-cols-3">
          <DataItem label="sync">
            <span data-testid="cluster-sync">{item.sync}</span>
          </DataItem>
          <DataItem label="concurrency">
            <span data-testid="cluster-concurrency">{item.concurrency}</span>
          </DataItem>
          <DataItem label="in_use">
            <span data-testid="cluster-in-use">{item.in_use == null ? "-" : item.in_use}</span>
          </DataItem>
          <DataItem label="cooldown until">
            <span data-testid="cluster-cooldown-until" className="text-fg-subtle">
              {item.cooldown_until ?? "-"}
            </span>
          </DataItem>
          <DataItem label="delete_on_push">
            <span data-testid="cluster-delete-on-push">{String(item.delete_on_push)}</span>
          </DataItem>
        </dl>

        {item.tunnel_login_needed && (
          <Alert tone="danger" title="ログインが必要（TOTP）" data-testid="cluster-tunnel-login-needed">
            <p>
              このクラスタのトンネル（下の一覧）を維持するための ssh 接続が切れ、鍵認証だけでは繋がりませんでした
              （ADR-0053 D3）。下の「接続」から TOTP を入力してください。繋がれば celeris がトンネルを自動で
              張り直します。
            </p>
          </Alert>
        )}

        {item.tunnel_forwards && item.tunnel_forwards.length > 0 && (
          <div className="space-y-2" data-testid="cluster-tunnel-forwards">
            <p className="text-sm font-medium text-fg-muted">トンネル（port forward）</p>
            {item.tunnel_forwards.map((forward) => (
              <TunnelForwardRow key={forward.listen} forward={forward} />
            ))}
          </div>
        )}

        {item.connected === false && auth === "manual" && (
          <Alert tone="danger" title="未接続です" data-testid="cluster-login-hint">
            <p>手元で次のコマンドを実行してください（2 要素認証を通して多重接続を張ります）。</p>
            <pre className="mt-2 overflow-x-auto rounded-lg border border-danger-border bg-surface px-3 py-2 font-mono text-xs text-fg">
              scripts/cluster-login.sh {item.host}
            </pre>
          </Alert>
        )}

        {startError && <ErrorFlash error={startError} />}
        {cancelError && <ErrorFlash error={cancelError} />}
        {codeResult && (
          <Alert
            tone={codeResult.ok ? "success" : "danger"}
            title={codeResult.ok ? "接続しました" : "接続に失敗しました"}
          >
            {codeResult.detail && <p>{codeResult.detail}</p>}
          </Alert>
        )}
        {startResult?.kind === "connected" && (
          <Alert tone="success" title="接続しました">
            <p>画面はまもなく最新の状態に更新されます。</p>
          </Alert>
        )}

        {item.connected === true ? (
          <fetcher.Form method="post">
            <input type="hidden" name="intent" value="cluster_connect_cancel" />
            <input type="hidden" name="id" value={item.id} />
            <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="cluster-disconnect">
              <Icon name="xCircle" />
              切断
            </Button>
          </fetcher.Form>
        ) : (
          auth !== "manual" && (
            <div className="space-y-3 rounded-lg border border-primary-border bg-primary-soft/40 p-3">
              {showPendingElsewhere && (
                <Alert tone="warning" title="接続処理が進行中です" data-testid="cluster-connect-pending">
                  <p>
                    別の操作（別のタブ、または画面を開き直す前の操作）でこのクラスタへの接続が進行中です。
                    検証コードの入力欄はこの画面には出せないので、
                    <strong>もう一度「接続し直す」を押すと</strong>やり直せます。取り消しても構いません。
                  </p>
                </Alert>
              )}

              {showConnectButton && (
                <fetcher.Form method="post">
                  <input type="hidden" name="intent" value="cluster_connect" />
                  <input type="hidden" name="id" value={item.id} />
                  <Button type="submit" variant="primary" size="sm" disabled={submitting} data-testid="cluster-connect">
                    <Icon name="link" />
                    {showPendingElsewhere ? "接続し直す" : "接続"}
                  </Button>
                </fetcher.Form>
              )}

              {showCodeForm && (
                <>
                  <Alert tone="warning" title="セキュリティ上の注意">
                    検証コードは平文 HTTP を通ります（GUI は LAN で平文。ADR-0024 D7 / ADR-0030 D4 と同じ注意）。
                    信頼できるネットワークでだけ使ってください。
                  </Alert>
                  <p className="text-sm text-fg-muted" data-testid="cluster-connect-prompt">
                    {startResult?.prompt}
                  </p>
                  {codeError && <ErrorFlash error={codeError} />}
                  <fetcher.Form method="post" className="flex flex-wrap items-end gap-3">
                    <input type="hidden" name="intent" value="cluster_connect_code" />
                    <input type="hidden" name="id" value={item.id} />
                    <div>
                      <label htmlFor={`cluster-connect-code-${item.id}`} className={labelClass}>
                        検証コード
                      </label>
                      <input
                        id={`cluster-connect-code-${item.id}`}
                        name="code"
                        type="password"
                        autoComplete="off"
                        inputMode="numeric"
                        className={`${inputClass} mt-1.5`}
                        data-testid="cluster-connect-code"
                      />
                      <p className={hintClass}>コードはログにも応答にも残りません。</p>
                    </div>
                    <Button
                      type="submit"
                      variant="primary"
                      size="sm"
                      disabled={submitting}
                      data-testid="cluster-connect-submit"
                    >
                      <Icon name="check" />
                      送信
                    </Button>
                  </fetcher.Form>
                </>
              )}

              {(showCodeForm || showPendingElsewhere) && (
                <fetcher.Form method="post">
                  <input type="hidden" name="intent" value="cluster_connect_cancel" />
                  <input type="hidden" name="id" value={item.id} />
                  <Button
                    type="submit"
                    variant="ghost"
                    size="sm"
                    disabled={submitting}
                    data-testid="cluster-connect-cancel"
                  >
                    <Icon name="x" />
                    取り消し
                  </Button>
                </fetcher.Form>
              )}
            </div>
          )
        )}
      </CardBody>
    </Card>
  );
}

/**
 * 1 本の port forward（ADR-0053 D3、Phase 66）。`up` の一語バッジと `listen → target` を出す。
 * モバイル幅でも折り返せるよう `flex-wrap` にし、長い host:port は `break-all` にする。
 */
function TunnelForwardRow({ forward }: { forward: ClusterForwardView }) {
  const word = forwardStatusWord(forward);
  const tone: Tone = word === "up" ? "success" : word === "down" ? "danger" : "neutral";
  return (
    <div
      className="flex min-w-0 flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-surface-2 px-3 py-2 text-xs"
      data-testid="cluster-tunnel-forward-row"
    >
      <Mono className="min-w-0 break-all">
        {forward.listen} → {forward.target}
      </Mono>
      <Badge tone={tone} dot data-status-badge="tunnel" data-testid="cluster-tunnel-forward-status">
        {word}
      </Badge>
    </div>
  );
}

/**
 * loader が `celerisErrorResponse` で投げた `Response` を判別する（docs/adr/0004-g1-decisions.md D6、
 * `app/routes/providers.tsx` と同じ方針）。celeris 停止中はバナー、それ以外は status と detail を出す。
 */
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
