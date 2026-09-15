import { useRouteLoaderData } from "react-router";
import type { loader as rootLoader } from "~/root";
import type { Route } from "./+types/inbox";

export function meta(_: Route.MetaArgs) {
  return [{ title: "taskd-gui" }];
}

/** `/`。G0 では taskd の `GET /health` の内容を出す。受信箱（`GET /inbox`）は G1。 */
export default function Inbox() {
  const root = useRouteLoaderData<typeof rootLoader>("root");
  const health = root?.health ?? null;
  return (
    <section className="space-y-4">
      <h1 className="text-xl font-semibold">taskd の状態</h1>
      {health ? (
        <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm" data-testid="health">
          <dt className="text-gray-500">taskd_version</dt>
          <dd data-testid="taskd_version">{health.taskd_version}</dd>
          <dt className="text-gray-500">api_version</dt>
          <dd data-testid="api_version">{health.api_version}</dd>
          <dt className="text-gray-500">schema_version</dt>
          <dd data-testid="schema_version">{health.schema_version}</dd>
          <dt className="text-gray-500">instance_id</dt>
          <dd className="font-mono">{health.instance_id}</dd>
          <dt className="text-gray-500">started_at</dt>
          <dd className="font-mono">{health.started_at}</dd>
          <dt className="text-gray-500">db.journal_mode</dt>
          <dd data-testid="journal_mode">
            {health.db.journal_mode}
            {health.db.journal_mode !== "wal" && (
              <span className="ml-2 rounded bg-amber-100 px-1 text-amber-900">wal ではありません（設定不備）</span>
            )}
          </dd>
          <dt className="text-gray-500">db.busy_timeout_ms</dt>
          <dd>{health.db.busy_timeout_ms}</dd>
        </dl>
      ) : (
        <p className="text-sm text-gray-600" data-testid="health-missing">
          taskd の状態を取得できません。
        </p>
      )}
      <p className="text-sm text-gray-500">受信箱（承認待ち・質問・draft・注意）は Phase G1 で実装します。</p>
    </section>
  );
}
