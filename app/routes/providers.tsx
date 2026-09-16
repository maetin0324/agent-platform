import { isRouteErrorResponse } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { formatDuration, secondsBetween } from "~/lib/time-delta";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { Providers } from "~/taskd/types";
import type { Route } from "./+types/providers";

/**
 * `/providers`（プロバイダ画面、docs/DESIGN.md §4.5）の loader が返すデータ。
 * `Providers.items[]`（`ProviderView`）をそのまま表にする（docs/adr/0007 D7）。
 * cooldown の残り時間だけは taskd が値を返さないので、BFF 自身がリクエスト前後に取った
 * `fetchedAt`（ISO 文字列）を基準に画面側で減算する（docs/adr/0007 D3）。
 */
export interface ProvidersData {
  providers: Providers;
  fetchedAt: string;
}

/** `GET /providers` を呼ぶ。応答はそのまま返す（派生の集計はしない）。 */
export async function loadProviders(client: TaskdClient, request: Request): Promise<ProvidersData> {
  const providers = await client.get<Providers>("/providers", { signal: request.signal });
  const fetchedAt = new Date().toISOString();
  return { providers, fetchedAt };
}

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。/providers に action は無いが、
// 他ルートと同じ規約に揃える（daemon.tsx と同じ）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ProvidersData> {
  try {
    return await loadProviders(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "プロバイダ - taskd-gui" }];
}

export default function ProvidersPage({ loaderData }: Route.ComponentProps) {
  const { providers, fetchedAt } = loaderData;

  return (
    <div className="space-y-8">
      <h1 className="text-xl font-semibold">
        プロバイダ
        <HelpLink anchor="screens" label="画面ごとの説明" />
      </h1>

      <section aria-labelledby="providers-heading" data-testid="providers-section">
        <h2 id="providers-heading" className="text-lg font-semibold">
          プロバイダ一覧
        </h2>
        {providers.items.length === 0 ? (
          <p className="mt-2 text-sm text-gray-500">プロバイダがありません</p>
        ) : (
          <ul className="mt-2 space-y-4">
            {providers.items.map((item) => (
              <li
                key={item.id}
                data-testid="provider-row"
                data-provider-id={item.id}
                className="rounded border p-3 text-sm"
              >
                <dl className="grid grid-cols-2 gap-x-4 gap-y-1 sm:grid-cols-4">
                  <DlItem label="id" value={item.id} />
                  <DlItem label="adapter" value={item.adapter} />
                  <DlItem label="tiers" value={item.tiers.join(", ")} />
                  <DlItem label="concurrency" value={String(item.concurrency)} />
                  <DlItem label="model" value={item.model ?? "-"} />
                  <DlItem label="in_use" value={item.in_use == null ? "-" : String(item.in_use)} />
                  <DlItem
                    label="env_keys（キー名のみ）"
                    value={item.env_keys.length > 0 ? item.env_keys.join(", ") : "-"}
                  />
                </dl>

                <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 sm:grid-cols-4">
                  <DlItem label="runs" value={String(item.stats.runs)} />
                  <DlItem label="done" value={String(item.stats.done)} testId="provider-done" />
                  <DlItem label="question" value={String(item.stats.question)} />
                  <DlItem label="error" value={String(item.stats.error)} />
                  <DlItem label="requeue" value={String(item.stats.requeue)} testId="provider-requeue" />
                  <DlItem label="lease_expired" value={String(item.stats.lease_expired)} />
                  <DlItem
                    label="tokens (input+output)"
                    value={String(item.stats.input_tokens + item.stats.output_tokens)}
                    testId="provider-tokens"
                  />
                </dl>

                {item.cooldown && (
                  <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 sm:grid-cols-4">
                    <DlItem label="cooldown reason" value={item.cooldown.reason} testId="provider-cooldown-reason" />
                    <DlItem
                      label="cooldown remaining"
                      value={formatDuration(secondsBetween(fetchedAt, item.cooldown.until))}
                      testId="provider-cooldown-remaining"
                    />
                    <DlItem label="cooldown until" value={item.cooldown.until} />
                  </dl>
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
 * `app/routes/daemon.tsx` と同じ方針）。taskd 停止中はバナー、それ以外は status と detail を出す。
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
