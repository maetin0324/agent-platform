import { useEffect } from "react";
import { isRouteErrorResponse, Links, Meta, Outlet, Scripts, ScrollRestoration, useRevalidator } from "react-router";
import { version as guiVersion } from "../package.json";
import type { Route } from "./+types/root";
import "./app.css";
import { hostCheck, securityHeaders } from "~/middleware/security.server";
import { useNonce } from "~/nonce";
import { getTaskdClient } from "~/taskd/client.server";
import { loadHealth } from "~/taskd/health.server";

// 全ルートに効くサーバ middleware（docs/DESIGN.md §8.2）。順序: Host 検査 → nonce とヘッダ。
export const middleware: Route.MiddlewareFunction[] = [hostCheck, securityHeaders];

export async function loader({ request }: Route.LoaderArgs) {
  const client = getTaskdClient();
  const state = await loadHealth(client, request.signal);
  // トークンは含めない。baseUrl は接続先の表示用（loopback が既定）。
  return { ...state, gui: { version: guiVersion, taskdApiUrl: client.baseUrl } };
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
  const { health, unavailable, problem, gui } = loaderData;
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

  return (
    <div className="mx-auto flex min-h-screen max-w-5xl flex-col px-4">
      <header className="flex items-center justify-between border-b py-3">
        <a href="/" className="text-lg font-semibold">
          taskd-gui
        </a>
        <nav className="text-sm text-gray-600">
          <a href="/" className="hover:underline">
            受信箱
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
