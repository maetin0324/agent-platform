import type { ReactNode } from "react";
import { data, isRouteErrorResponse, Link, useFetcher } from "react-router";
import type { ReplayOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { runReplay } from "~/celeris/route-actions.server";
import type { ConfigView, DaemonView } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { RouteRecovery } from "~/components/RouteRecovery";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, DataList, EmptyState, Mono, PageHeader, SectionTitle, StatCard } from "~/components/ui/misc";
import { isTransientStatus } from "~/lib/recovery";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { formatDuration, secondsBetween } from "~/lib/time-delta";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/daemon";

/**
 * `/daemon`（デーモン画面、docs/DESIGN.md §4.6）の loader が返すデータ。
 * G2 の範囲は受け入れ条件 8（replay ボタン）だけなので、`DaemonView` / `ConfigView` を
 * そのまま描く最小限の画面にとどめる（in_flight の経過時間・遅延判定・awaiting_human /
 * unroutable の照合等の本格版は Phase G4）。
 */
export interface DaemonData {
  daemon: DaemonView;
  config: ConfigView;
}

/** `GET /daemon` と `GET /config` を並列に呼ぶ。応答はそのまま返す（派生値は計算しない）。 */
export async function loadDaemon(client: CelerisClient, request: Request): Promise<DaemonData> {
  const [daemon, config] = await Promise.all([
    client.get<DaemonView>("/daemon", { signal: request.signal }),
    client.get<ConfigView>("/config", { signal: request.signal }),
  ]);
  return { daemon, config };
}

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<DaemonData> {
  try {
    return await loadDaemon(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "デーモン - Celeris" }];
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  if (intent !== "replay") {
    throw data({ error: "unknown intent" }, { status: 400 });
  }
  const outcome = await runReplay(getCelerisClient(), request.signal);
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export default function DaemonPage({ loaderData }: Route.ComponentProps) {
  const { daemon, config } = loaderData;
  const { snapshot } = daemon;
  // replay の結果が SSE の再検証で消えないよう fetcher に載せる（監査 H1）。
  const fetcher = useFetcher<ReplayOutcome>();
  const submitting = fetcher.state !== "idle";
  const replay = fetcher.data;

  return (
    <div className="space-y-8">
      <PageHeader
        icon="activity"
        title={
          <>
            デーモン
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="ディスパッチャの稼働状況・設定の要約・replay をここでまとめて確認します。"
      />

      <section aria-labelledby="daemon-heading" data-testid="daemon-section" className="space-y-4">
        <SectionTitle icon="activity" id="daemon-heading">
          デーモンの状態
        </SectionTitle>
        {snapshot ? (
          <>
            {secondsBetween(snapshot.last_tick_at, daemon.now) * 1000 >= 3 * snapshot.tick_ms && (
              <Alert tone="danger" data-testid="daemon-delayed">
                ディスパッチャが遅延しています（last_tick_at が {3 * snapshot.tick_ms}ms 以上前です）。
              </Alert>
            )}

            <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
              <StatCard
                icon="hash"
                label="ticks"
                value={<span data-testid="daemon-ticks">{snapshot.ticks}</span>}
                hint={`tick_ms ${snapshot.tick_ms}`}
              />
              <StatCard
                icon="activity"
                label="in_flight"
                value={snapshot.in_flight.length}
                tone={snapshot.in_flight.length > 0 ? "primary" : "neutral"}
              />
              <StatCard
                icon="clock"
                label="cooldowns"
                value={snapshot.cooldowns.length}
                tone={snapshot.cooldowns.length > 0 ? "warning" : "neutral"}
              />
              <StatCard
                icon="user"
                label="awaiting_human"
                value={snapshot.awaiting_human.length}
                tone={snapshot.awaiting_human.length > 0 ? "teal" : "neutral"}
              />
              <StatCard
                icon="users"
                label="部下待ち（awaiting_children）"
                value={
                  <span data-testid="daemon-awaiting-children-count">{snapshot.awaiting_children?.length ?? 0}</span>
                }
                tone={(snapshot.awaiting_children?.length ?? 0) > 0 ? "teal" : "neutral"}
              />
              <StatCard
                icon="alert"
                label="unroutable"
                value={snapshot.unroutable.length}
                tone={snapshot.unroutable.length > 0 ? "danger" : "neutral"}
              />
            </div>

            <Card>
              <CardHeader icon="server" title="インスタンス" description="このデーモンプロセスの識別情報" />
              <CardBody>
                <DataList>
                  <DataItem label="pid">
                    <span data-testid="daemon-pid">{snapshot.pid}</span>
                  </DataItem>
                  <DataItem label="hostname">
                    <span data-testid="daemon-hostname">{snapshot.hostname}</span>
                  </DataItem>
                  <DataItem label="instance_id">
                    <Mono className="break-all">{snapshot.instance_id}</Mono>
                  </DataItem>
                  <DataItem label="started_at">
                    <span className="text-fg-subtle">{snapshot.started_at}</span>
                  </DataItem>
                  <DataItem label="last_tick_at">
                    <span data-testid="daemon-last-tick-at" className="text-fg-subtle">
                      {snapshot.last_tick_at}
                    </span>
                  </DataItem>
                </DataList>
              </CardBody>
            </Card>

            <SubSection icon="activity" tone="primary" heading="in_flight">
              {snapshot.in_flight.length === 0 ? (
                <EmptyState icon="checkCircle" title="実行中の run はありません。" />
              ) : (
                <div className="overflow-x-auto">
                  <table className="w-full min-w-[36rem] text-left text-sm">
                    <thead>
                      <tr className="text-xs font-medium text-fg-subtle">
                        <th className="pr-2 pb-2">task</th>
                        <th className="pr-2 pb-2">run_id</th>
                        <th className="pr-2 pb-2">provider</th>
                        <th className="pr-2 pb-2">経過</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-border">
                      {snapshot.in_flight.map((item) => (
                        <tr key={item.run_id} data-testid="in-flight-row" data-task-id={item.task_id}>
                          <td className="py-2 pr-2">
                            <Link
                              to={`/tasks/${item.task_id}`}
                              data-testid="in-flight-task-link"
                              className="font-medium text-primary hover:underline"
                            >
                              {item.task_id}
                            </Link>
                          </td>
                          <td className="py-2 pr-2" data-testid="in-flight-run-id">
                            <Mono>{item.run_id}</Mono>
                          </td>
                          <td className="py-2 pr-2" data-testid="in-flight-provider">
                            <Badge tone="primary">{item.provider}</Badge>
                          </td>
                          <td className="py-2 pr-2 tabular-nums text-fg-muted" data-testid="in-flight-elapsed">
                            {formatDuration(secondsBetween(item.since, daemon.now))}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </SubSection>

            <SubSection icon="clock" tone="warning" heading="cooldowns">
              {snapshot.cooldowns.length === 0 ? (
                <EmptyState icon="checkCircle" title="cooldown 中のプロバイダはありません。" />
              ) : (
                <div className="overflow-x-auto">
                  <table className="w-full min-w-[28rem] text-left text-sm">
                    <thead>
                      <tr className="text-xs font-medium text-fg-subtle">
                        <th className="pr-2 pb-2">provider</th>
                        <th className="pr-2 pb-2">reason</th>
                        <th className="pr-2 pb-2">残り</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-border">
                      {snapshot.cooldowns.map((cooldown) => (
                        <tr
                          key={cooldown.provider}
                          data-testid="daemon-cooldown-row"
                          data-provider-id={cooldown.provider}
                        >
                          <td className="py-2 pr-2 font-medium text-fg">{cooldown.provider}</td>
                          <td className="py-2 pr-2 text-fg-muted">{cooldown.reason}</td>
                          <td className="py-2 pr-2 tabular-nums" data-testid="daemon-cooldown-remaining">
                            {formatDuration(secondsBetween(daemon.now, cooldown.until))}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </SubSection>

            <SubSection icon="user" tone="teal" heading="awaiting_human">
              {snapshot.awaiting_human.length === 0 ? (
                <EmptyState icon="checkCircle" title="人間の承認待ちはありません。" />
              ) : (
                <ul className="space-y-1.5 text-sm">
                  {snapshot.awaiting_human.map((id) => (
                    <li key={id} data-testid="awaiting-human-item">
                      <Link
                        to={`/tasks/${id}`}
                        className="inline-flex items-center gap-1.5 font-medium text-primary hover:underline"
                      >
                        {id}
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
            </SubSection>

            {/* ADR-0023 D3: 委譲した子を待っている親。`reviewing` でも「自分の判定待ち」とは別物。 */}
            <SubSection icon="users" tone="teal" heading="部下待ち（awaiting_children）">
              {(snapshot.awaiting_children?.length ?? 0) === 0 ? (
                <EmptyState icon="checkCircle" title="委譲した子を待っているタスクはありません。" />
              ) : (
                <ul className="space-y-1.5 text-sm">
                  {(snapshot.awaiting_children ?? []).map((id) => (
                    <li key={id} data-testid="awaiting-children-item">
                      <Link
                        to={`/tasks/${id}`}
                        className="inline-flex items-center gap-1.5 font-medium text-primary hover:underline"
                      >
                        {id}
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
            </SubSection>

            <SubSection icon="alert" tone="danger" heading="unroutable">
              {snapshot.unroutable.length === 0 ? (
                <EmptyState icon="checkCircle" title="経路の無いタスクはありません。" />
              ) : (
                <ul className="space-y-1.5 text-sm">
                  {snapshot.unroutable.map((id) => (
                    <li key={id} data-testid="unroutable-item">
                      <Link
                        to={`/tasks/${id}`}
                        className="inline-flex items-center gap-1.5 font-medium text-primary hover:underline"
                      >
                        {id}
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
            </SubSection>
          </>
        ) : (
          <EmptyState icon="clock" title="最初の tick を待っています" />
        )}
      </section>

      <section aria-labelledby="config-heading" data-testid="config-section" className="space-y-4">
        <SectionTitle icon="settings" id="config-heading">
          設定の要約
        </SectionTitle>
        <Card>
          <CardBody>
            <DataList>
              <DataItem label="config_path" wide>
                <span className="break-all" title={config.config_path}>
                  {config.config_path}
                </span>
              </DataItem>
              <DataItem label="db" wide>
                <span className="break-all" title={config.db}>
                  {config.db}
                </span>
              </DataItem>
              <DataItem label="workspace_root" wide>
                <span className="break-all" title={config.workspace_root}>
                  {config.workspace_root}
                </span>
              </DataItem>
              <DataItem label="tick_ms">{config.tick_ms}</DataItem>
              <DataItem label="max_concurrency">{config.max_concurrency}</DataItem>
              <DataItem label="plan_auto_accept">
                <Badge tone={config.plan_auto_accept ? "success" : "neutral"}>{String(config.plan_auto_accept)}</Badge>
              </DataItem>
              <DataItem label="api.bind">
                <Mono>{config.api.bind}</Mono>
              </DataItem>
              <DataItem label="api.auth_required">
                <Badge tone={config.api.auth_required ? "success" : "neutral"}>
                  {String(config.api.auth_required)}
                </Badge>
              </DataItem>
            </DataList>
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="replay-heading" data-testid="replay-section" className="space-y-4">
        <SectionTitle icon="rotate" id="replay-heading">
          replay
        </SectionTitle>
        <Card>
          <CardBody className="space-y-4">
            <p className="text-sm text-fg-muted">
              全タスクをイベントから再構築して `tasks` との差分を検査します（DB は変更しません）。
            </p>
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="replay" />
              <Button type="submit" variant="primary" disabled={submitting} data-testid="replay-button">
                <Icon name="rotate" />
                replay
              </Button>
            </fetcher.Form>
            {replay &&
              (replay.ok ? (
                <Alert tone={replay.report.mismatches.length > 0 ? "warning" : "success"} data-testid="replay-result">
                  {replay.report.mismatches.length} mismatches across {replay.report.tasks} tasks
                  {replay.report.mismatches.length > 0 && (
                    <ul className="mt-2 space-y-1">
                      {replay.report.mismatches.map((mismatch) => (
                        <li
                          key={`${mismatch.task_id}-${mismatch.field}`}
                          data-testid="replay-mismatch"
                          className="rounded-md border border-border bg-surface px-2 py-1 font-mono text-xs text-fg"
                        >
                          {`{"id":"${mismatch.task_id}","field":"${mismatch.field}","replayed":"${mismatch.replayed}","stored":"${mismatch.stored}"}`}
                        </li>
                      ))}
                    </ul>
                  )}
                </Alert>
              ) : (
                <ErrorFlash error={replay.error} />
              ))}
          </CardBody>
        </Card>
      </section>
    </div>
  );
}

/** デーモン画面のサブセクション。見出しは h3 のまま Card に収める（見出しレベルは変えない）。 */
function SubSection({
  icon,
  tone,
  heading,
  children,
}: {
  icon: Parameters<typeof CardHeader>[0]["icon"];
  tone: Parameters<typeof CardHeader>[0]["tone"];
  heading: string;
  children: ReactNode;
}) {
  return (
    <Card>
      <CardHeader icon={icon} tone={tone} title={<h3 className="text-[0.95rem] font-semibold text-fg">{heading}</h3>} />
      <CardBody>{children}</CardBody>
    </Card>
  );
}

/**
 * loader が `celerisErrorResponse` で投げた `Response` を判別する（docs/adr/0004-g1-decisions.md D6、
 * `app/routes/tasks.$id.tsx` と同じ方針）。celeris 停止中はバナー、それ以外は status と detail を出す。
 */
export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as CelerisRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={data.baseUrl ?? ""} problem={null} />
          <RouteRecovery />
        </main>
      );
    }
    return (
      <main className="p-4">
        <h1 className="text-xl font-semibold">エラー {data.status}</h1>
        <p className="mt-2 text-sm text-fg-muted">{data.detail}</p>
        {isTransientStatus(data.status) && <RouteRecovery />}
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
