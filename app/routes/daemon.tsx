import { data, Form, isRouteErrorResponse, Link, useNavigation } from "react-router";
import { ErrorFlash } from "~/components/Flash";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { formatDuration, secondsBetween } from "~/lib/time-delta";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { runReplay } from "~/taskd/route-actions.server";
import type { ConfigView, DaemonView } from "~/taskd/types";
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
export async function loadDaemon(client: TaskdClient, request: Request): Promise<DaemonData> {
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
    return await loadDaemon(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "デーモン - taskd-gui" }];
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  if (intent !== "replay") {
    throw data({ error: "unknown intent" }, { status: 400 });
  }
  const outcome = await runReplay(getTaskdClient(), request.signal);
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export default function DaemonPage({ loaderData, actionData }: Route.ComponentProps) {
  const { daemon, config } = loaderData;
  const { snapshot } = daemon;
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";

  return (
    <div className="space-y-8">
      <h1 className="text-xl font-semibold">デーモン</h1>

      <section aria-labelledby="daemon-heading" data-testid="daemon-section">
        <h2 id="daemon-heading" className="text-lg font-semibold">
          デーモンの状態
        </h2>
        {snapshot ? (
          <>
            {secondsBetween(snapshot.last_tick_at, daemon.now) * 1000 >= 3 * snapshot.tick_ms && (
              <p
                data-testid="daemon-delayed"
                className="mt-2 rounded border border-red-300 bg-red-50 p-2 text-sm text-red-700"
              >
                ディスパッチャが遅延しています（last_tick_at が {3 * snapshot.tick_ms}ms 以上前です）。
              </p>
            )}
            <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm sm:grid-cols-4">
              <DlItem label="pid" value={String(snapshot.pid)} testId="daemon-pid" />
              <DlItem label="hostname" value={snapshot.hostname} testId="daemon-hostname" />
              <DlItem label="instance_id" value={snapshot.instance_id} />
              <DlItem label="started_at" value={snapshot.started_at} />
              <DlItem label="ticks" value={String(snapshot.ticks)} testId="daemon-ticks" />
              <DlItem label="last_tick_at" value={snapshot.last_tick_at} testId="daemon-last-tick-at" />
              <DlItem label="tick_ms" value={String(snapshot.tick_ms)} />
              <DlItem label="in_flight" value={String(snapshot.in_flight.length)} />
              <DlItem label="awaiting_human" value={String(snapshot.awaiting_human.length)} />
              <DlItem label="unroutable" value={String(snapshot.unroutable.length)} />
            </dl>

            <div className="mt-4">
              <h3 className="text-sm font-semibold">in_flight</h3>
              {snapshot.in_flight.length === 0 ? (
                <p className="mt-1 text-sm text-gray-500">実行中の run はありません。</p>
              ) : (
                <table className="mt-1 w-full text-left text-sm">
                  <thead>
                    <tr className="text-xs text-gray-500">
                      <th className="pr-2">task</th>
                      <th className="pr-2">run_id</th>
                      <th className="pr-2">provider</th>
                      <th className="pr-2">経過</th>
                    </tr>
                  </thead>
                  <tbody>
                    {snapshot.in_flight.map((item) => (
                      <tr key={item.run_id} data-testid="in-flight-row" data-task-id={item.task_id}>
                        <td className="pr-2">
                          <Link
                            to={`/tasks/${item.task_id}`}
                            data-testid="in-flight-task-link"
                            className="hover:underline"
                          >
                            {item.task_id}
                          </Link>
                        </td>
                        <td className="pr-2" data-testid="in-flight-run-id">
                          {item.run_id}
                        </td>
                        <td className="pr-2" data-testid="in-flight-provider">
                          {item.provider}
                        </td>
                        <td className="pr-2" data-testid="in-flight-elapsed">
                          {formatDuration(secondsBetween(item.since, daemon.now))}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div className="mt-4">
              <h3 className="text-sm font-semibold">cooldowns</h3>
              {snapshot.cooldowns.length === 0 ? (
                <p className="mt-1 text-sm text-gray-500">cooldown 中のプロバイダはありません。</p>
              ) : (
                <table className="mt-1 w-full text-left text-sm">
                  <thead>
                    <tr className="text-xs text-gray-500">
                      <th className="pr-2">provider</th>
                      <th className="pr-2">reason</th>
                      <th className="pr-2">残り</th>
                    </tr>
                  </thead>
                  <tbody>
                    {snapshot.cooldowns.map((cooldown) => (
                      <tr
                        key={cooldown.provider}
                        data-testid="daemon-cooldown-row"
                        data-provider-id={cooldown.provider}
                      >
                        <td className="pr-2">{cooldown.provider}</td>
                        <td className="pr-2">{cooldown.reason}</td>
                        <td className="pr-2" data-testid="daemon-cooldown-remaining">
                          {formatDuration(secondsBetween(daemon.now, cooldown.until))}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div className="mt-4">
              <h3 className="text-sm font-semibold">awaiting_human</h3>
              {snapshot.awaiting_human.length === 0 ? (
                <p className="mt-1 text-sm text-gray-500">人間の承認待ちはありません。</p>
              ) : (
                <ul className="mt-1 space-y-1 text-sm">
                  {snapshot.awaiting_human.map((id) => (
                    <li key={id} data-testid="awaiting-human-item">
                      <Link to={`/tasks/${id}`} className="hover:underline">
                        {id}
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
            </div>

            <div className="mt-4">
              <h3 className="text-sm font-semibold">unroutable</h3>
              {snapshot.unroutable.length === 0 ? (
                <p className="mt-1 text-sm text-gray-500">経路の無いタスクはありません。</p>
              ) : (
                <ul className="mt-1 space-y-1 text-sm">
                  {snapshot.unroutable.map((id) => (
                    <li key={id} data-testid="unroutable-item">
                      <Link to={`/tasks/${id}`} className="hover:underline">
                        {id}
                      </Link>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </>
        ) : (
          <p className="mt-2 text-sm text-gray-500">最初の tick を待っています</p>
        )}
      </section>

      <section aria-labelledby="config-heading" data-testid="config-section">
        <h2 id="config-heading" className="text-lg font-semibold">
          設定の要約
        </h2>
        <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm sm:grid-cols-4">
          <DlItem label="config_path" value={config.config_path} />
          <DlItem label="db" value={config.db} />
          <DlItem label="workspace_root" value={config.workspace_root} />
          <DlItem label="tick_ms" value={String(config.tick_ms)} />
          <DlItem label="max_concurrency" value={String(config.max_concurrency)} />
          <DlItem label="plan_auto_accept" value={String(config.plan_auto_accept)} />
          <DlItem label="api.bind" value={config.api.bind} />
          <DlItem label="api.auth_required" value={String(config.api.auth_required)} />
        </dl>
      </section>

      <section aria-labelledby="replay-heading" data-testid="replay-section">
        <h2 id="replay-heading" className="text-lg font-semibold">
          replay
        </h2>
        <p className="mt-2 text-sm text-gray-600">
          全タスクをイベントから再構築して `tasks` との差分を検査します（DB は変更しません）。
        </p>
        <Form method="post" className="mt-2">
          <input type="hidden" name="intent" value="replay" />
          <button
            type="submit"
            disabled={submitting}
            data-testid="replay-button"
            className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
          >
            replay
          </button>
        </Form>
        {actionData &&
          (actionData.ok ? (
            <div className="mt-2" data-testid="replay-result">
              {actionData.report.mismatches.length} mismatches across {actionData.report.tasks} tasks
              {actionData.report.mismatches.length > 0 && (
                <ul className="mt-2 space-y-1 text-sm">
                  {actionData.report.mismatches.map((mismatch) => (
                    <li
                      key={`${mismatch.task_id}-${mismatch.field}`}
                      data-testid="replay-mismatch"
                      className="rounded border p-1 font-mono text-xs"
                    >
                      {`{"id":"${mismatch.task_id}","field":"${mismatch.field}","replayed":"${mismatch.replayed}","stored":"${mismatch.stored}"}`}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          ) : (
            <ErrorFlash error={actionData.error} />
          ))}
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
 * `app/routes/tasks.$id.tsx` と同じ方針）。taskd 停止中はバナー、それ以外は status と detail を出す。
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
