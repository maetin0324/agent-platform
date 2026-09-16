import { isRouteErrorResponse } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { Clusters } from "~/taskd/types";
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
      <h1 className="text-xl font-semibold">
        クラスタ
        <HelpLink anchor="screens" label="画面ごとの説明" />
      </h1>

      <section aria-labelledby="clusters-heading" data-testid="clusters-section">
        <h2 id="clusters-heading" className="text-lg font-semibold">
          クラスタ一覧
        </h2>
        {clusters.items.length === 0 ? (
          <p className="mt-2 text-sm text-gray-500">クラスタがありません</p>
        ) : (
          <ul className="mt-2 space-y-4">
            {clusters.items.map((item) => (
              <li
                key={item.id}
                data-testid="cluster-row"
                data-cluster-id={item.id}
                className="rounded border p-3 text-sm"
              >
                <dl className="grid grid-cols-2 gap-x-4 gap-y-1 sm:grid-cols-4">
                  <DlItem label="id" value={item.id} testId="cluster-id" />
                  <DlItem label="host" value={item.host} testId="cluster-host" />
                  <DlItem
                    label="connected"
                    value={item.connected === true ? "connected" : item.connected === false ? "disconnected" : "-"}
                    testId="cluster-connected"
                  />
                  <DlItem label="cooldown until" value={item.cooldown_until ?? "-"} testId="cluster-cooldown-until" />
                  <DlItem
                    label="in_use"
                    value={item.in_use == null ? "-" : String(item.in_use)}
                    testId="cluster-in-use"
                  />
                  <DlItem label="concurrency" value={String(item.concurrency)} testId="cluster-concurrency" />
                  <DlItem label="sync" value={item.sync} testId="cluster-sync" />
                  <DlItem label="delete_on_push" value={String(item.delete_on_push)} testId="cluster-delete-on-push" />
                </dl>

                {item.connected === false && (
                  <div
                    className="mt-2 rounded border border-red-300 bg-red-50 p-2 text-sm"
                    data-testid="cluster-login-hint"
                  >
                    手元で `scripts/cluster-login.sh {item.host}` を実行してください
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

function DlItem({ label, value, testId }: { label: string; value: string; testId?: string }) {
  return (
    <div>
      <dt className="text-xs text-gray-500">{label}</dt>
      <dd data-testid={testId}>{value}</dd>
    </div>
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
        <p className="mt-2 text-sm text-gray-600">{data.detail}</p>
      </main>
    );
  }

  return (
    <main className="p-4">
      <h1 className="text-xl font-semibold">エラー</h1>
      <p className="mt-2 text-sm text-gray-600">予期しないエラーが起きました。</p>
    </main>
  );
}
