import { isRouteErrorResponse } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import { Badge } from "~/components/ui/badge";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { Alert, DataItem, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { formatDuration, secondsBetween } from "~/lib/time-delta";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { Providers, ProviderView } from "~/taskd/types";
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
      <PageHeader
        icon="cpu"
        title={
          <>
            プロバイダ
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="接続先プロバイダの稼働状況・累積 usage・直近の疎通確認をまとめて確認します。"
      />

      <section aria-labelledby="providers-heading" data-testid="providers-section" className="space-y-4">
        <SectionTitle icon="cpu" id="providers-heading" count={providers.items.length}>
          プロバイダ一覧
        </SectionTitle>
        {providers.items.length === 0 ? (
          <EmptyState icon="cpu" title="プロバイダがありません" />
        ) : (
          <div className="grid gap-4 xl:grid-cols-2">
            {providers.items.map((item) => (
              <ProviderCard key={item.id} item={item} fetchedAt={fetchedAt} />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function ProviderCard({ item, fetchedAt }: { item: ProviderView; fetchedAt: string }) {
  const tone: Tone = item.cooldown ? "warning" : "success";

  return (
    <Card data-testid="provider-row" data-provider-id={item.id} className="hover:shadow-md">
      <CardHeader
        icon="cpu"
        tone={tone}
        title={<Mono className="text-sm font-semibold text-fg">{item.id}</Mono>}
        description={item.adapter}
        actions={
          <Badge tone={tone} dot pulse={!!item.cooldown}>
            {item.cooldown ? "cooldown" : "利用可"}
          </Badge>
        }
      />
      <CardBody className="space-y-4">
        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm sm:grid-cols-3">
          <DataItem label="tiers">
            <div className="flex flex-wrap gap-1">
              {item.tiers.map((tier) => (
                <Badge key={tier} tone="neutral">
                  {tier}
                </Badge>
              ))}
            </div>
          </DataItem>
          <DataItem label="concurrency">{item.concurrency}</DataItem>
          <DataItem label="model">{item.model ?? "-"}</DataItem>
          <DataItem label="in_use">{item.in_use == null ? "-" : item.in_use}</DataItem>
          <DataItem label="env_keys（キー名のみ）" wide>
            {item.env_keys.length > 0 ? (
              <div className="flex flex-wrap gap-1">
                {item.env_keys.map((key) => (
                  <Mono key={key} className="rounded bg-surface-2 px-1.5 py-0.5">
                    {key}
                  </Mono>
                ))}
              </div>
            ) : (
              "-"
            )}
          </DataItem>
        </dl>

        <dl className="grid grid-cols-3 gap-x-4 gap-y-3 text-sm sm:grid-cols-4">
          <DataItem label="runs">{item.stats.runs}</DataItem>
          <DataItem label="done">
            <span data-testid="provider-done">{item.stats.done}</span>
          </DataItem>
          <DataItem label="question">{item.stats.question}</DataItem>
          <DataItem label="error">{item.stats.error}</DataItem>
          <DataItem label="requeue">
            <span data-testid="provider-requeue">{item.stats.requeue}</span>
          </DataItem>
          <DataItem label="lease_expired">{item.stats.lease_expired}</DataItem>
          <DataItem label="tokens (input+output)" wide>
            <span data-testid="provider-tokens" className="tabular-nums">
              {item.stats.input_tokens + item.stats.output_tokens}
            </span>
          </DataItem>
        </dl>

        {/* ADR-0022 D2: 直近の疎通確認。手動で `POST /providers/{id}/check` を叩いたときだけ入り、
            taskd を再起動すると消える（メモリ上の観測値）。 */}
        <Alert
          tone={item.last_check ? (item.last_check.result === "ok" ? "success" : "danger") : "neutral"}
          title="最後の疎通確認"
        >
          <span data-testid="provider-last-check">
            {item.last_check ? `${item.last_check.result}（${item.last_check.at}）` : "未確認"}
          </span>
        </Alert>

        {item.cooldown && (
          <Alert tone="warning" title="cooldown">
            <dl className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3">
              <DataItem label="reason">
                <span data-testid="provider-cooldown-reason">{item.cooldown.reason}</span>
              </DataItem>
              <DataItem label="remaining">
                <span data-testid="provider-cooldown-remaining" className="tabular-nums">
                  {formatDuration(secondsBetween(fetchedAt, item.cooldown.until))}
                </span>
              </DataItem>
              <DataItem label="until">
                <span className="text-fg-subtle">{item.cooldown.until}</span>
              </DataItem>
            </dl>
          </Alert>
        )}
      </CardBody>
    </Card>
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
