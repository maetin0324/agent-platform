import { useEffect } from "react";
import {
  Form,
  isRouteErrorResponse,
  Links,
  Meta,
  Outlet,
  Scripts,
  ScrollRestoration,
  useLocation,
  useRevalidator,
  useRouteLoaderData,
} from "react-router";
import { authCheck, sessionContext } from "~/auth.server";
import { Badge } from "~/components/ui/badge";
import { buttonClass } from "~/components/ui/button";
import { Icon, type IconName } from "~/components/ui/Icon";
import { Alert } from "~/components/ui/misc";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { cn } from "~/lib/utils";
import { version as guiVersion } from "../package.json";
import type { Route } from "./+types/root";
import "./app.css";
import { useTaskdStream } from "~/hooks/useTaskdStream";
import { csrfCheck, hostCheck, securityHeaders } from "~/middleware/security.server";
import { useNonce } from "~/nonce";
import { getTaskdClient } from "~/taskd/client.server";
import { TaskdError, type TaskdRouteErrorData } from "~/taskd/errors";
import { loadHealth } from "~/taskd/health.server";
import type { InboxCounts } from "~/taskd/types";

// 全ルートに効くサーバ middleware（docs/DESIGN.md §8.2）。順序: Host 検査 → 認証（docs/adr/0008 D1）→ CSRF 検査（変更系のみ）→ nonce とヘッダ。
export const middleware: Route.MiddlewareFunction[] = [hostCheck, authCheck, csrfCheck, securityHeaders];

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request, context }: Route.LoaderArgs) {
  const session = context.get(sessionContext);
  const client = getTaskdClient();
  // 未認証（= /login を描画中）は taskd を呼ばない。ログイン前に taskd の版や接続先を出さない（docs/adr/0008 D5）
  if (!session.authenticated) {
    return {
      health: null,
      unavailable: false,
      problem: null,
      counts: null,
      session,
      // 接続先も出さない（hydration payload にも載せない）
      gui: { version: guiVersion, taskdApiUrl: "" },
    };
  }
  const state = await loadHealth(client, request.signal);
  // タイトルバーの承認待ちバッジ用（docs/DESIGN.md §4.1, §6.2, ADR-0004 D1）。
  // `/`（inbox ルート）が別途 `GET /inbox` を全項目のために呼ぶので、ここでは counts だけを使う。
  // taskd に届かない・エラーのときは badge を出さないだけにする（health のバナーが既に状況を伝える）。
  let counts: InboxCounts | null = null;
  if (!state.unavailable && state.health) {
    try {
      counts = (await client.get<{ counts: InboxCounts }>("/inbox", { signal: request.signal })).counts;
    } catch (e) {
      counts = null;
      // `GET /health` は taskd 側で無認証なので、トークンが無い・違うことに最初に気づくのはここ（docs/adr/0008 D6）。
      // 401 だけはバナーで知らせる（他は子ルートの ErrorBoundary が個別に出す）。
      if (e instanceof TaskdError && e.status === 401) state.problem = `${e.status} ${e.code}`;
    }
  }
  // トークンは含めない。baseUrl は接続先の表示用（loopback が既定）。
  return { ...state, counts, session, gui: { version: guiVersion, taskdApiUrl: client.baseUrl } };
}

export function Layout({ children }: { children: React.ReactNode }) {
  const nonce = useNonce();
  return (
    <html lang="ja">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <meta name="color-scheme" content="light dark" />
        <Meta />
        <Links />
      </head>
      <body className="min-h-screen bg-bg font-sans text-fg antialiased">
        {children}
        <ScrollRestoration nonce={nonce} />
        <Scripts nonce={nonce} />
      </body>
    </html>
  );
}

const RECHECK_MS = 5_000;

export default function App({ loaderData }: Route.ComponentProps) {
  const { health, unavailable, problem, counts, gui, session } = loaderData;
  const revalidator = useRevalidator();
  const disconnected = unavailable || health === null;
  const showBanner = disconnected || problem !== null;

  // taskd 停止中は 5 秒ごとに root だけ再検証し、復旧したらバナーを消す（§6.5）
  useEffect(() => {
    if (!disconnected) return;
    const id = setInterval(() => {
      if (revalidator.state === "idle") revalidator.revalidate();
    }, RECHECK_MS);
    return () => clearInterval(id);
  }, [disconnected, revalidator]);

  // SSE（`/events`）を root で 1 本だけ張り、`task.event` / `daemon` / `reset` を受けたらルートを再検証する（docs/DESIGN.md §6.3, ADR-0004 D2）。
  useTaskdStream({ enabled: session.authenticated });

  // 未認証（/login）: ナビゲーションもフッタも出さない（docs/adr/0008 D5）
  if (!session.authenticated) {
    return (
      <div className="flex min-h-screen flex-col px-4">
        <Outlet />
      </div>
    );
  }

  return (
    <div className="min-h-screen lg:grid lg:grid-cols-[16rem_1fr]">
      <Sidebar
        approvals={counts?.approvals ?? 0}
        connected={!disconnected && problem === null}
        taskdVersion={health?.taskd_version ?? null}
        logoutEnabled={session.enabled}
      />
      <div className="flex min-w-0 flex-col">
        <main className="mx-auto w-full max-w-6xl flex-1 px-4 py-6 sm:px-6 lg:px-10 lg:py-10">
          {showBanner && <TaskdBanner taskdApiUrl={gui.taskdApiUrl} problem={problem} />}
          <div className="animate-fade-in">
            <Outlet />
          </div>
        </main>
        <footer
          className="mx-auto w-full max-w-6xl border-t border-border px-4 py-5 text-xs text-fg-subtle sm:px-6 lg:px-10"
          data-testid="footer"
        >
          <p className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className="font-medium text-fg-muted">taskd-gui {gui.version}</span>
            {health && (
              <>
                <span aria-hidden="true">·</span>taskd {health.taskd_version} · api_version {health.api_version} ·
                schema_version {health.schema_version}
              </>
            )}
          </p>
          {health && (
            <dl
              className="mt-3 grid grid-cols-2 gap-x-6 gap-y-2 rounded-lg border border-border bg-surface/60 px-4 py-3 sm:grid-cols-4"
              data-testid="health"
            >
              <div>
                <dt className="text-fg-subtle">taskd_version</dt>
                <dd className="font-mono text-fg-muted" data-testid="taskd_version">
                  {health.taskd_version}
                </dd>
              </div>
              <div>
                <dt className="text-fg-subtle">api_version</dt>
                <dd className="font-mono text-fg-muted" data-testid="api_version">
                  {health.api_version}
                </dd>
              </div>
              <div>
                <dt className="text-fg-subtle">schema_version</dt>
                <dd className="font-mono text-fg-muted" data-testid="schema_version">
                  {health.schema_version}
                </dd>
              </div>
              <div>
                <dt className="text-fg-subtle">db.journal_mode</dt>
                <dd className="font-mono text-fg-muted" data-testid="journal_mode">
                  {health.db.journal_mode}
                  {health.db.journal_mode !== "wal" && (
                    <Badge tone="warning" className="ml-2 font-sans">
                      wal ではありません（設定不備）
                    </Badge>
                  )}
                </dd>
              </div>
            </dl>
          )}
        </footer>
      </div>
    </div>
  );
}

type NavItem = { href: string; label: string; icon: IconName; badge?: "approvals" };

/**
 * ナビゲーションのグループ（docs/adr/0011 D3、Phase G13a で SPEC §4 の順に組み替え。ADR-0033 D8）。
 * 先頭は SPEC §4 の 6 画面の順（秘書・組織・案件・報告・認可・成果物）。秘書・報告・認可・成果物は
 * G13b までプレースホルダ（`~/components/Placeholder.tsx`）。既存のタスク・プロバイダ・アカウント・
 * クラスタの画面は「裏方」区画にまとめて下げる（人が見る単位は案件と組織になり、タスクは裏方に下がる）。
 */
const NAV_GROUPS: { label: string; items: NavItem[] }[] = [
  {
    label: "業務",
    items: [
      { href: "/org/secretary", label: "秘書", icon: "message" },
      { href: "/org", label: "組織", icon: "users" },
      { href: "/projects", label: "案件", icon: "folder" },
      { href: "/reports", label: "報告", icon: "send" },
      { href: "/approvals", label: "認可", icon: "shield" },
      { href: "/artifacts", label: "成果物", icon: "file" },
    ],
  },
  {
    label: "裏方",
    items: [
      { href: "/", label: "受信箱", icon: "inbox", badge: "approvals" },
      { href: "/tasks", label: "一覧", icon: "list" },
      { href: "/graph", label: "DAG", icon: "network" },
      { href: "/tasks/new", label: "新規タスク", icon: "plus" },
      { href: "/plans/new", label: "新規 Plan", icon: "sparkles" },
      { href: "/daemon", label: "デーモン", icon: "activity" },
      { href: "/providers", label: "プロバイダ", icon: "cpu" },
      { href: "/accounts", label: "アカウント", icon: "users" },
      { href: "/clusters", label: "クラスタ", icon: "server" },
    ],
  },
  { label: "ヘルプ", items: [{ href: "/help", label: "使い方", icon: "book" }] },
];

function isActive(pathname: string, href: string): boolean {
  if (href === "/") return pathname === "/";
  if (href === "/tasks") return pathname === "/tasks" || (pathname.startsWith("/tasks/") && pathname !== "/tasks/new");
  // `/org/secretary` は別のナビ項目（秘書）なので、「組織」は `/org` そのものだけを active にする。
  if (href === "/org") return pathname === "/org";
  return pathname === href || pathname.startsWith(`${href}/`);
}

function Sidebar({
  approvals,
  connected,
  taskdVersion,
  logoutEnabled,
}: {
  approvals: number;
  connected: boolean;
  taskdVersion: string | null;
  logoutEnabled: boolean;
}) {
  const { pathname } = useLocation();
  return (
    <aside className="sticky top-0 z-30 border-b border-border bg-surface/80 backdrop-blur-xl lg:h-screen lg:border-r lg:border-b-0">
      <div className="flex h-full flex-wrap items-center lg:flex-col lg:flex-nowrap lg:items-stretch lg:px-3 lg:py-5">
        <div className="order-1 px-4 pt-3 lg:order-none lg:px-2 lg:pt-0">
          <a href="/" className="group flex items-center gap-2.5 rounded-lg no-underline">
            <span className="grid size-8 place-items-center rounded-lg bg-linear-to-br from-primary via-primary to-teal text-white shadow-md ring-1 ring-white/20 transition-transform group-hover:scale-105 dark:text-bg">
              <Icon name="zap" className="size-4" strokeWidth={2.2} />
            </span>
            <span className="text-[0.95rem] font-bold tracking-tight text-fg">taskd-gui</span>
          </a>
        </div>

        <nav
          aria-label="メイン"
          className="order-3 mt-2 flex w-full items-center gap-1 overflow-x-auto px-3 pb-2 lg:order-none lg:mt-7 lg:flex-1 lg:flex-col lg:items-stretch lg:gap-5 lg:overflow-visible lg:px-0 lg:pb-0"
        >
          {NAV_GROUPS.map((group) => (
            <div key={group.label} className="contents lg:block">
              <p className="hidden px-3 pb-1.5 text-[0.7rem] font-semibold uppercase tracking-wider text-fg-subtle lg:block">
                {group.label}
              </p>
              <ul className="contents lg:flex lg:flex-col lg:gap-0.5">
                {group.items.map((item) => {
                  const active = isActive(pathname, item.href);
                  return (
                    <li key={item.href} className="shrink-0">
                      <a
                        href={item.href}
                        aria-current={active ? "page" : undefined}
                        className={cn(
                          "group relative flex items-center gap-2.5 rounded-lg px-3 py-1.5 text-sm font-medium no-underline transition-colors",
                          active
                            ? "bg-primary-soft text-primary-soft-fg"
                            : "text-fg-muted hover:bg-surface-2 hover:text-fg",
                        )}
                      >
                        {active && (
                          <span
                            aria-hidden="true"
                            className="absolute inset-y-1.5 -left-3 hidden w-1 rounded-r-full bg-primary lg:block"
                          />
                        )}
                        <Icon
                          name={item.icon}
                          className={cn("size-4", active ? "text-primary" : "text-fg-subtle group-hover:text-fg-muted")}
                        />
                        {item.label}
                        {item.badge === "approvals" && approvals > 0 && (
                          <span
                            data-testid="approvals-badge"
                            className="ml-auto min-w-5 rounded-full bg-danger px-1.5 py-0.5 text-center text-[0.7rem] leading-none font-bold text-white tabular-nums shadow-sm dark:text-bg"
                          >
                            {approvals}
                          </span>
                        )}
                      </a>
                    </li>
                  );
                })}
              </ul>
            </div>
          ))}
        </nav>

        {/* 接続状態とログアウト。同じ要素を 2 つ描かない（data-testid の重複を避ける。docs/adr/0011 D3）ので、狭い画面では order で右上へ寄せる */}
        <div className="order-2 ml-auto flex items-center gap-2 px-4 pt-3 lg:order-none lg:ml-0 lg:mt-4 lg:block lg:space-y-2 lg:border-t lg:border-border lg:px-1 lg:pt-4">
          <ConnectionPill connected={connected} taskdVersion={taskdVersion} />
          {logoutEnabled && (
            <Form method="post" action="/logout">
              <button
                type="submit"
                data-testid="logout"
                className="flex w-full items-center gap-2.5 rounded-lg px-3 py-1.5 text-sm font-medium text-fg-muted transition-colors hover:bg-surface-2 hover:text-fg"
              >
                <Icon name="logout" className="size-4 text-fg-subtle" />
                ログアウト
              </button>
            </Form>
          )}
        </div>
      </div>
    </aside>
  );
}

function ConnectionPill({
  connected,
  taskdVersion,
  className,
}: {
  connected: boolean;
  taskdVersion: string | null;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex items-center gap-2 rounded-lg border border-border bg-surface-2/60 px-3 py-2 text-xs text-fg-muted",
        className,
      )}
    >
      <span
        aria-hidden="true"
        className={cn(
          "size-2 rounded-full",
          connected ? "bg-success text-success animate-pulse-dot" : "bg-danger text-danger",
        )}
      />
      <span className="font-medium text-fg">{connected ? "taskd 接続中" : "taskd 未接続"}</span>
      {connected && taskdVersion && (
        <span className="ml-auto hidden font-mono text-fg-subtle sm:inline">v{taskdVersion}</span>
      )}
    </div>
  );
}

export function TaskdBanner({ taskdApiUrl, problem }: { taskdApiUrl: string; problem: string | null }) {
  return (
    <Alert
      role="alert"
      data-testid="taskd-banner"
      tone="danger"
      icon={problem ? "lock" : "wifiOff"}
      className="mb-6"
      title={problem ? `taskd が要求を拒否しました（${taskdApiUrl}）` : `taskd に接続できません（${taskdApiUrl}）`}
    >
      <p>
        {problem
          ? `taskd の応答: ${problem}。TASKD_API_TOKEN_FILE が taskd の token_file と一致しているか確認してください。`
          : "taskd が起動しているか、TASKD_API_URL を確認してください。5 秒ごとに再接続を試みます。"}
        操作はできません。taskctl は従来どおり使えます。
      </p>
    </Alert>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  const rootData = useRouteLoaderData("root") as Route.ComponentProps["loaderData"] | undefined;
  // `/tasks` 等の子ルートが taskd のエラーを `Response` として投げてここまで来たとき（`taskdErrorResponse`、
  // docs/adr/0004 D6）、汎用のエラー画面ではなく `/` と同じバナー等を出す（`/` 自身は inbox.tsx が catch する
  // のでここには来ない）。本番ビルドは素の Error を渡す前に汎用 500 へサニタイズするため、`Response` 以外は
  // 判別できない（= 本当に予期しないエラーとして扱ってよい）。
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as TaskdRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="mx-auto max-w-3xl p-4 pt-16">
          <TaskdBanner taskdApiUrl={data.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    // taskd の 401（トークン無し・不一致）は接続不可と同じ形のバナーで知らせる（docs/adr/0008 D6）
    if (data.status === 401) {
      return (
        <main className="mx-auto max-w-3xl p-4 pt-16">
          <TaskdBanner
            taskdApiUrl={rootData?.gui.taskdApiUrl ?? ""}
            problem={`${data.status} ${data.code ?? "unauthorized"}`}
          />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-3xl p-4 pt-16">
        <ErrorPanel title={data.status === 404 ? "404" : `エラー ${data.status}`}>
          <p>{data.detail}</p>
        </ErrorPanel>
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
    <main className="mx-auto max-w-3xl p-4 pt-16">
      <ErrorPanel title={message}>
        <p>{details}</p>
        {stack && (
          <pre className="mt-3 w-full overflow-x-auto rounded-lg border border-border bg-surface-2 p-4 text-xs">
            <code>{stack}</code>
          </pre>
        )}
      </ErrorPanel>
    </main>
  );
}

function ErrorPanel({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-2xl border border-border bg-surface p-8 shadow-md">
      <span className="grid size-11 place-items-center rounded-xl bg-danger-soft text-danger-soft-fg ring-1 ring-danger-border">
        <Icon name="alert" className="size-5" />
      </span>
      <h1 className="mt-4 text-2xl font-bold tracking-tight text-fg">{title}</h1>
      <div className="mt-2 text-sm text-fg-muted">{children}</div>
      <a href="/" className={buttonClass({ variant: "secondary", className: "mt-6" })}>
        <Icon name="arrowLeft" />
        受信箱へ戻る
      </a>
    </div>
  );
}
