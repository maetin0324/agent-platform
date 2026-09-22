import { data, type FetcherWithComponents, isRouteErrorResponse, useFetcher } from "react-router";
import type { ClusterActionOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import {
  cancelClusterConnect,
  putClusterSettings,
  readClusterConnectCode,
  readClusterId,
  readClusterWorkDir,
  startClusterConnect,
  submitClusterConnectCode,
} from "~/celeris/clusters-admin.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import type { ClusterForwardView, Clusters, ClusterView } from "~/celeris/types";
import { ErrorFlash, FieldErrors } from "~/components/Flash";
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
 * クラスタへの接続の中継（ADR-0032 D5/D6）と作業ディレクトリの登録・変更（ADR-0059 D6 §3.107）。
 * `clusters-admin.server.ts` に判断ロジックは無く、フォームの `intent` を対応する呼び出しに写すだけ。
 * **`POST /reload` は呼ばない**（接続を張っても・`work_dir` を変えても `config.toml` の設定は変わらないので
 * 不要。プロバイダ・秘密の管理とはここが違う）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getCelerisClient();
  const id = readClusterId(form);

  let outcome: ClusterActionOutcome;
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
    // ADR-0059 D6: 「保存」は入力欄の値をそのまま送る（空欄なら celeris の 422 `validation` をそのまま
    // 出す）。「上書きを消す」は入力欄を見ず、明示的に `work_dir: null` を送る（設定ファイルの値に戻る）。
    case "cluster_work_dir_save":
      outcome = await putClusterSettings(client, id, readClusterWorkDir(form), request.signal);
      break;
    case "cluster_work_dir_clear":
      outcome = await putClusterSettings(client, id, null, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export default function ClustersPage({ loaderData }: Route.ComponentProps) {
  const { clusters } = loaderData;
  const fetcher = useFetcher<ClusterActionOutcome>();
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
 * クラスタ全体の状態を 1 語に（ADR-0055 D1-3、Phase 86）。`tunnel_login_needed` を「down」より優先する:
 * ssh master が鍵認証でも繋がらず人の TOTP が要る状態は、ただの切断より先に伝えたい（下の
 * 「接続（TOTP）」の全幅ボタンと対になる）。観測が無ければ `"unknown"`（値を捏造しない）。
 */
export type ClusterStatusWord = "connected" | "login-needed" | "down" | "unknown";

export function clusterStatusWord(item: Pick<ClusterView, "connected" | "tunnel_login_needed">): ClusterStatusWord {
  if (item.tunnel_login_needed) return "login-needed";
  if (item.connected === true) return "connected";
  if (item.connected === false) return "down";
  return "unknown";
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

/**
 * ADR-0059 D6: 実効の作業ディレクトリの出どころを 1 語のバッジ語にする。`/clusters` は裏方の画面
 * （`app/lib/labels.ts` の対象外）なので、celeris の値（`"settings"` / `"config"`）をそのまま出す
 * （`auth` バッジと同じ流儀）。どちらでも無ければ `"unregistered"`（値を捏造しない。`clusterStatusWord`
 * の `"unknown"` と同じ考え方）。
 */
export type ClusterWorkDirWord = "settings" | "config" | "unregistered";

export function clusterWorkDirWord(source: string | null | undefined): ClusterWorkDirWord {
  if (source === "settings") return "settings";
  if (source === "config") return "config";
  return "unregistered";
}

/**
 * 「上書きを消す」ボタンは DB の上書き（`work_dir_source === "settings"`）があるときだけ出す
 * （設定ファイルの値・未登録には「消すもの」が無いため）。
 */
export function showClusterWorkDirClear(source: string | null | undefined): boolean {
  return source === "settings";
}

function ClusterCard({
  item,
  fetcher,
  submitting,
}: {
  item: ClusterView;
  fetcher: FetcherWithComponents<ClusterActionOutcome>;
  submitting: boolean;
}) {
  const statusWord = clusterStatusWord(item);
  const tone: Tone = statusWord === "connected" ? "success" : statusWord === "unknown" ? "neutral" : "danger";
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
            <Badge
              tone={tone}
              dot
              pulse={statusWord === "connected"}
              data-status-badge="cluster"
              data-testid="cluster-connected"
            >
              {statusWord}
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

        <ClusterWorkDirSection item={item} fetcher={fetcher} submitting={submitting} />

        {item.tunnel_login_needed && (
          <Alert tone="danger" title="ログインが必要（TOTP）" data-testid="cluster-tunnel-login-needed">
            <p>
              このクラスタのトンネル（下の一覧）を維持するための ssh 接続が切れ、鍵認証だけでは繋がりませんでした
              （ADR-0053 D3）。下の「接続（TOTP）」から TOTP を入力してください。繋がれば celeris がトンネルを
              自動で張り直します。
            </p>
          </Alert>
        )}

        {item.tunnel_forwards && item.tunnel_forwards.length > 0 && (
          <div className="space-y-2" data-testid="cluster-tunnel-forwards">
            <p className="text-sm font-medium text-fg-muted">トンネル（port forward）</p>
            {item.tunnel_forwards.map((forward) => (
              <TunnelForwardRow key={forward.listen} forward={forward} />
            ))}
            <details className="rounded-lg border border-border bg-surface-2/40 px-3 py-2 text-sm text-fg-muted">
              <summary className="cursor-pointer select-none font-medium text-fg" data-testid="tunnel-explainer-toggle">
                これは何を意味しますか？
              </summary>
              <p className="mt-2">
                このトンネルは Qwen（無料の LLM source、ADR-0053）への接続です。転送先（target）が応答しない間は、
                <Mono className="text-xs">celeris/&lt;tier&gt;</Mono> は自動で Claude / GPT のアカウントに倒れます （
                <Mono className="text-xs">/accounts</Mono> の「LLM source」節で今の解決先を確認できます）。
                タスクは止まりません。
              </p>
            </details>
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
                  {/* Phase 86（ADR-0055 D1）: TOTP の再ログインが要る状態は、スマホでも見つけやすい
                      全幅の主操作にする。それ以外（初回の接続など）は従来どおり小さいボタン。 */}
                  <Button
                    type="submit"
                    variant="primary"
                    size="sm"
                    disabled={submitting}
                    className={item.tunnel_login_needed ? "w-full sm:w-auto" : undefined}
                    data-testid="cluster-connect"
                  >
                    <Icon name="link" />
                    {item.tunnel_login_needed ? "接続（TOTP）" : showPendingElsewhere ? "接続し直す" : "接続"}
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
 * 作業ディレクトリ節（ADR-0059 D6、ADR-0055 ラウンド 21）: 実効値 + 出どころの 1 語バッジ + 編集フォーム。
 * `<details>` で折りたたむのは「接続」節の流儀（ADR-0032）ではなくトンネルの「これは何を意味しますか？」
 * （Phase 66）に倣った。**編集フォームは常時開いていない**（作業ディレクトリは滅多に変えないので、
 * 通常は実効値の表示だけで十分。ADR-0055 D2「一度に 1 画面ずつ直し」の精神で画面を騒がしくしない）。
 * 「上書きを消す」は `work_dir_source === "settings"` のときだけ出す（消すものが無ければボタン自体が無い方が
 * 状態を素直に表す。無効化したボタンより「無い」を選ぶ）。
 */
function ClusterWorkDirSection({
  item,
  fetcher,
  submitting,
}: {
  item: ClusterView;
  fetcher: FetcherWithComponents<ClusterActionOutcome>;
  submitting: boolean;
}) {
  const actionData = fetcher.data;
  const own = actionData && actionData.op === "cluster_settings" && actionData.id === item.id ? actionData : undefined;
  const error = own && !own.ok ? own.error : undefined;
  const saved = own?.ok ? own.settings : undefined;

  const word = clusterWorkDirWord(item.work_dir_source);
  const tone: Tone = word === "settings" ? "success" : word === "config" ? "neutral" : "warning";
  const showClear = showClusterWorkDirClear(item.work_dir_source);

  return (
    <div
      className="space-y-2 rounded-lg border border-border bg-surface-2/40 px-3 py-2.5"
      data-testid="cluster-work-dir"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-sm font-medium text-fg-muted">作業ディレクトリ</p>
        <Badge tone={tone} data-testid="cluster-work-dir-source">
          {word}
        </Badge>
      </div>

      {item.work_dir ? (
        <Mono className="block break-all text-sm text-fg" data-testid="cluster-work-dir-value">
          {item.work_dir}
        </Mono>
      ) : (
        <p className="text-sm text-fg-subtle" data-testid="cluster-work-dir-empty">
          未登録。コマンド実行だけのタスクはここで動きます
        </p>
      )}

      {saved && (
        <Alert
          tone="success"
          title={saved.work_dir ? "保存しました" : "上書きを消しました"}
          data-testid="cluster-work-dir-saved"
        >
          {saved.work_dir ? <p className="break-all">{saved.work_dir}</p> : <p>設定ファイルの値に戻ります。</p>}
        </Alert>
      )}
      {error && <ErrorFlash error={error} />}

      <details className="text-sm">
        <summary className="cursor-pointer select-none font-medium text-fg" data-testid="cluster-work-dir-toggle">
          変更する
        </summary>
        <div className="mt-2 space-y-3">
          <fetcher.Form method="post" className="space-y-1.5">
            <input type="hidden" name="intent" value="cluster_work_dir_save" />
            <input type="hidden" name="id" value={item.id} />
            <label htmlFor={`cluster-work-dir-input-${item.id}`} className={labelClass}>
              パス
            </label>
            <input
              id={`cluster-work-dir-input-${item.id}`}
              name="work_dir"
              type="text"
              defaultValue={item.work_dir ?? ""}
              placeholder="/work/NBB/rmaeda"
              autoComplete="off"
              className={`${inputClass} mt-1.5`}
              data-testid="cluster-work-dir-input"
            />
            <p className={hintClass}>絶対パスか ~ で始めてください。</p>
            <FieldErrors error={error} field="work_dir" />
            <Button type="submit" variant="primary" size="sm" disabled={submitting} data-testid="cluster-work-dir-save">
              <Icon name="check" />
              保存
            </Button>
          </fetcher.Form>

          {showClear && (
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="cluster_work_dir_clear" />
              <input type="hidden" name="id" value={item.id} />
              <Button
                type="submit"
                variant="ghost"
                size="sm"
                disabled={submitting}
                data-testid="cluster-work-dir-clear"
              >
                <Icon name="x" />
                上書きを消す
              </Button>
            </fetcher.Form>
          )}
        </div>
      </details>
    </div>
  );
}

/**
 * 1 本の port forward（ADR-0053 D3、Phase 66。listener/target の分離は Phase 85）。状態バッジと
 * `listen → target` を出す。`unreachable`（転送はあるが先方が応答しない）のときは、バッジの下に
 * 「転送あり・先方応答なし」の理由を添える（celeris はこの状態では転送を再発行しない）。
 * モバイル幅でも折り返せるよう `flex-wrap` にし、長い host:port は `break-all` にする。
 */
function TunnelForwardRow({ forward }: { forward: ClusterForwardView }) {
  const word = forwardStatusWord(forward);
  const tone: Tone =
    word === "up" ? "success" : word === "unreachable" ? "warning" : word === "down" ? "danger" : "neutral";
  return (
    <div
      className="flex min-w-0 flex-col gap-1 rounded-lg border border-border bg-surface-2 px-3 py-2 text-sm"
      data-testid="cluster-tunnel-forward-row"
    >
      <div className="flex min-w-0 flex-wrap items-center justify-between gap-2">
        <Mono className="min-w-0 break-all">
          {forward.listen} → {forward.target}
        </Mono>
        <Badge tone={tone} dot data-status-badge="tunnel" data-testid="cluster-tunnel-forward-status">
          {word}
        </Badge>
      </div>
      {word === "unreachable" && (
        <p className="text-fg-muted" data-testid="cluster-tunnel-forward-reason">
          転送あり・先方応答なし
        </p>
      )}
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
