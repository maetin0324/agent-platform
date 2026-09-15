import { useEffect } from "react";
import { isRouteErrorResponse, Links, Meta, Outlet, Scripts, ScrollRestoration, useRevalidator } from "react-router";
import { version as guiVersion } from "../package.json";
import type { Route } from "./+types/root";
import "./app.css";
import { useTaskdStream } from "~/hooks/useTaskdStream";
import { hostCheck, securityHeaders } from "~/middleware/security.server";
import { useNonce } from "~/nonce";
import { getTaskdClient } from "~/taskd/client.server";
import type { TaskdRouteErrorData } from "~/taskd/errors";
import { loadHealth } from "~/taskd/health.server";
import type { InboxCounts } from "~/taskd/types";

// 全ルートに効くサーバ middleware（docs/DESIGN.md §8.2）。順序: Host 検査 → nonce とヘッダ。
export const middleware: Route.MiddlewareFunction[] = [hostCheck, securityHeaders];

export async function loader({ request }: Route.LoaderArgs) {
  const client = getTaskdClient();
  const state = await loadHealth(client, request.signal);
  // タイトルバーの承認待ちバッジ用（docs/DESIGN.md §4.1, §6.2, ADR-0004 D1）。
  // `/`（inbox ルート）が別途 `GET /inbox` を全項目のために呼ぶので、ここでは counts だけを使う。
  // taskd に届かない・エラーのときは badge を出さないだけにする（health のバナーが既に状況を伝える）。
  let counts: InboxCounts | null = null;
  if (!state.unavailable && state.health) {
    try {
      counts = (await client.get<{ counts: InboxCounts }>("/inbox", { signal: request.signal })).counts;
    } catch {
      counts = null;
    }
  }
  // トークンは含めない。baseUrl は接続先の表示用（loopback が既定）。
  return { ...state, counts, gui: { version: guiVersion, taskdApiUrl: client.baseUrl } };
}

export function Layout({ children }: { children: React.ReactNode }) {
  const nonce = useNonce();
  return (
    <html lang="ja">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <Meta />
        <Links />
      </head>
      <body className="min-h-screen bg-white text-gray-900">
        {children}
        <ScrollRestoration nonce={nonce} />
        <Scripts nonce={nonce} />
      </body>
    </html>
  );
}

const RECHECK_MS = 5_000;

export default function App({ loaderData }: Route.ComponentProps) {
  const { health, unavailable, problem, counts, gui } = loaderData;
  const revalidator = useRevalidator();
  const disconnected = unavailable || health === null;

  // taskd 停止中は 5 秒ごとに root だけ再検証し、復旧したらバナーを消す（§6.5）
  useEffect(() => {
    if (!disconnected) return;
    const id = setInterval(() => {
      if (revalidator.state === "idle") revalidator.revalidate();
    }, RECHECK_MS);
    return () => clearInterval(id);
  }, [disconnected, revalidator]);

  // SSE（`/events`）を root で 1 本だけ張り、`task.event` / `daemon` / `reset` を受けたらルートを再検証する（docs/DESIGN.md §6.3, ADR-0004 D2）。
  useTaskdStream();

  return (
    <div className="mx-auto flex min-h-screen max-w-5xl flex-col px-4">
      <header className="flex items-center justify-between border-b py-3">
        <a href="/" className="text-lg font-semibold">
          taskd-gui
        </a>
        <nav className="flex items-center gap-4 text-sm text-gray-600">
          <a href="/" className="hover:underline">
            受信箱
            {counts && counts.approvals > 0 && (
              <span
                data-testid="approvals-badge"
                className="ml-1 rounded-full bg-red-600 px-1.5 py-0.5 text-xs font-semibold text-white"
              >
                {counts.approvals}
              </span>
            )}
          </a>
          <a href="/tasks" className="hover:underline">
            一覧
          </a>
        </nav>
      </header>
      {disconnected && <TaskdBanner taskdApiUrl={gui.taskdApiUrl} problem={problem} />}
      <main className="flex-1 py-4">
        <Outlet />
      </main>
      <footer className="border-t py-2 text-xs text-gray-500" data-testid="footer">
        taskd-gui {gui.version}
        {health && (
          <>
            {" · "}taskd {health.taskd_version} · api_version {health.api_version} · schema_version{" "}
            {health.schema_version}
          </>
        )}
        {health && (
          <dl className="mt-1 grid grid-cols-[max-content_1fr] gap-x-4 gap-y-0.5" data-testid="health">
            <dt>taskd_version</dt>
            <dd data-testid="taskd_version">{health.taskd_version}</dd>
            <dt>api_version</dt>
            <dd data-testid="api_version">{health.api_version}</dd>
            <dt>schema_version</dt>
            <dd data-testid="schema_version">{health.schema_version}</dd>
            <dt>db.journal_mode</dt>
            <dd data-testid="journal_mode">
              {health.db.journal_mode}
              {health.db.journal_mode !== "wal" && (
                <span className="ml-2 rounded bg-amber-100 px-1 text-amber-900">wal ではありません（設定不備）</span>
              )}
            </dd>
          </dl>
        )}
      </footer>
    </div>
  );
}

export function TaskdBanner({ taskdApiUrl, problem }: { taskdApiUrl: string; problem: string | null }) {
  return (
    <div
      role="alert"
      data-testid="taskd-banner"
      className="my-3 rounded border border-red-300 bg-red-50 px-3 py-2 text-sm text-red-900"
    >
      <p className="font-semibold">taskd に接続できません（{taskdApiUrl}）</p>
      <p>
        {problem ? `taskd の応答: ${problem}。` : "taskd が起動しているか、TASKD_API_URL を確認してください。"}
        操作はできません。taskctl は従来どおり使えます。5 秒ごとに再接続を試みます。
      </p>
    </div>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  // `/tasks` 等の子ルートが taskd のエラーを `Response` として投げてここまで来たとき（`taskdErrorResponse`、
  // docs/adr/0004 D6）、汎用のエラー画面ではなく `/` と同じバナー等を出す（`/` 自身は inbox.tsx が catch する
  // のでここには来ない）。本番ビルドは素の Error を渡す前に汎用 500 へサニタイズするため、`Response` 以外は
  // 判別できない（= 本当に予期しないエラーとして扱ってよい）。
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as TaskdRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="container mx-auto p-4 pt-16">
          <TaskdBanner taskdApiUrl={data.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="container mx-auto p-4 pt-16">
        <h1 className="text-xl font-semibold">{data.status === 404 ? "404" : `エラー ${data.status}`}</h1>
        <p>{data.detail}</p>
      </main>
    );
  }
  let message = "エラー";
  let details = "予期しないエラーが起きました。";
  let stack: string | undefined;
  if (isRouteErrorResponse(error)) {
    message = error.status === 404 ? "404" : `${error.status}`;
    details =
      error.status === 404
        ? "ページが見つかりません。"
        : typeof error.data === "string" && error.data
          ? error.data
          : error.statusText || details;
  } else if (import.meta.env.DEV && error instanceof Error) {
    details = error.message;
    stack = error.stack;
  }
  return (
    <main className="container mx-auto p-4 pt-16">
      <h1 className="text-xl font-semibold">{message}</h1>
      <p>{details}</p>
      {stack && (
        <pre className="w-full overflow-x-auto p-4 text-xs">
          <code>{stack}</code>
        </pre>
      )}
    </main>
  );
}
