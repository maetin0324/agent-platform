import { isRouteErrorResponse } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import { Badge } from "~/components/ui/badge";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { Alert, DataItem, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { Clusters, ClusterView } from "~/taskd/types";
import type { Route } from "./+types/clusters";

/**
 * `/clusters`（クラスタ画面、docs/DESIGN.md §10 Phase G7）の loader が返すデータ。
 * `Clusters.items[]`（`ClusterView`）をそのまま表にする。cooldown の残り秒数などは
 * taskd がすでに計算済みなので、GUI 側で再計算しない。
 */
export interface ClustersData {
  clusters: Clusters;
}

/** `GET /clusters` を呼ぶ。応答はそのまま返す（派生の集計はしない）。 */
export async function loadClusters(client: TaskdClient, request: Request): Promise<ClustersData> {
  const clusters = await client.get<Clusters>("/clusters", { signal: request.signal });
  return { clusters };
}

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。/clusters に action は無いが、
// 他ルートと同じ規約に揃える（providers.tsx と同じ）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ClustersData> {
  try {
    return await loadClusters(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "クラスタ - taskd-gui" }];
}

export default function ClustersPage({ loaderData }: Route.ComponentProps) {
  const { clusters } = loaderData;

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
              <ClusterCard key={item.id} item={item} />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function ClusterCard({ item }: { item: ClusterView }) {
  const tone: Tone = item.connected === true ? "success" : item.connected === false ? "danger" : "neutral";
  const connectedLabel = item.connected === true ? "connected" : item.connected === false ? "disconnected" : "-";

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
          <Badge tone={tone} dot pulse={item.connected === true} data-testid="cluster-connected">
            {connectedLabel}
          </Badge>
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

        {item.connected === false && (
          <Alert tone="danger" title="未接続です" data-testid="cluster-login-hint">
            <p>手元で次のコマンドを実行してください（2 要素認証を通して多重接続を張ります）。</p>
            <pre className="mt-2 overflow-x-auto rounded-lg border border-danger-border bg-surface px-3 py-2 font-mono text-xs text-fg">
              scripts/cluster-login.sh {item.host}
            </pre>
          </Alert>
        )}
      </CardBody>
    </Card>
  );
}

/**
 * loader が `taskdErrorResponse` で投げた `Response` を判別する（docs/adr/0004-g1-decisions.md D6、
 * `app/routes/providers.tsx` と同じ方針）。taskd 停止中はバナー、それ以外は status と detail を出す。
 */
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
