import { useEffect, useRef, useState } from "react";
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
import { ConsoleComposer } from "~/components/ConsoleComposer";
import { ConsoleComposerProvider, useConsoleComposerContext } from "~/components/ConsoleComposerContext";
import { NotificationsWatcher } from "~/components/NotificationsWatcher";
import { RouteRecovery } from "~/components/RouteRecovery";
import { TimeZonePreference } from "~/components/TimeZonePreference";
import { Badge } from "~/components/ui/badge";
import { buttonClass } from "~/components/ui/button";
import { Icon, type IconName } from "~/components/ui/Icon";
import { Alert } from "~/components/ui/misc";
import { approvalsPendingCount } from "~/lib/approvals";
import { DEFAULT_MOBILE_COMPOSER_HEIGHT_PX, isConsoleComposerPathname } from "~/lib/console-composer";
import { reportsBadgeTone } from "~/lib/reports";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { cn } from "~/lib/utils";
import { version as guiVersion } from "../package.json";
import type { Route } from "./+types/root";
import "./app.css";
import { getCelerisClient } from "~/celeris/client.server";
import { CelerisError, type CelerisRouteErrorData } from "~/celeris/errors";
import { loadHealth } from "~/celeris/health.server";
import type { DaemonView, InboxCounts, ReportsLive } from "~/celeris/types";
import { useCelerisStream } from "~/hooks/useCelerisStream";
import { csrfCheck, hostCheck, securityHeaders } from "~/middleware/security.server";
import { useNonce } from "~/nonce";

// 全ルートに効くサーバ middleware（docs/DESIGN.md §8.2）。順序: Host 検査 → 認証（docs/adr/0008 D1）→ CSRF 検査（変更系のみ）→ nonce とヘッダ。
export const middleware: Route.MiddlewareFunction[] = [hostCheck, authCheck, csrfCheck, securityHeaders];

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request, context }: Route.LoaderArgs) {
  const session = context.get(sessionContext);
  const client = getCelerisClient();
  // 未認証（= /login を描画中）は celeris を呼ばない。ログイン前に celeris の版や接続先を出さない（docs/adr/0008 D5）
  if (!session.authenticated) {
    return {
      health: null,
      unavailable: false,
      problem: null,
      counts: null,
      reportsLive: null,
      approvalsPending: 0,
      session,
      // 接続先も出さない（hydration payload にも載せない）
      gui: { version: guiVersion, celerisApiUrl: "" },
    };
  }
  const state = await loadHealth(client, request.signal);
  // タイトルバーの承認待ちバッジ用（docs/DESIGN.md §4.1, §6.2, ADR-0004 D1）。
  // `/`（inbox ルート）が別途 `GET /inbox` を全項目のために呼ぶので、ここでは counts だけを使う。
  // celeris に届かない・エラーのときは badge を出さないだけにする（health のバナーが既に状況を伝える）。
  let counts: InboxCounts | null = null;
  // 「報告」ナビのバッジと、ブラウザ通知の判定（ADR-0033 D3、ADR-0034 D6）。`DaemonSnapshot.reports` は
  // API が応答を組むときに埋める唯一のフィールドなので、SSE の `daemon` イベントで root が再検証されるたびに
  // ここで拾い直す（`useCelerisStream` が `task.event`/`daemon`/`reset` のいずれでも root を revalidate する）。
  let reportsLive: ReportsLive | null = null;
  // 「認可」ナビのバッジ（ADR-0033 D5、docs/celeris-api-v1.md §3.20 の追加。`DaemonSnapshot.approvals_pending`
  // も `reports` と同じく API が応答を組むときに埋める）。celeris に届かないときは 0（バッジを出さない）。
  let approvalsPending = 0;
  if (!state.unavailable && state.health) {
    try {
      counts = (await client.get<{ counts: InboxCounts }>("/inbox", { signal: request.signal })).counts;
    } catch (e) {
      counts = null;
      // `GET /health` は celeris 側で無認証なので、トークンが無い・違うことに最初に気づくのはここ（docs/adr/0008 D6）。
      // 401 だけはバナーで知らせる（他は子ルートの ErrorBoundary が個別に出す）。
      if (e instanceof CelerisError && e.status === 401) state.problem = `${e.status} ${e.code}`;
    }
    try {
      const daemon = await client.get<DaemonView>("/daemon", { signal: request.signal });
      reportsLive = daemon.snapshot?.reports ?? null;
      approvalsPending = approvalsPendingCount(daemon);
    } catch {
      // バッジと通知が出ないだけにする（他の画面のバナー・ErrorBoundary が状況を伝える）。
      reportsLive = null;
      approvalsPending = 0;
    }
  }
  // トークンは含めない。baseUrl は接続先の表示用（loopback が既定）。
  return {
    ...state,
    counts,
    reportsLive,
    approvalsPending,
    session,
    gui: { version: guiVersion, celerisApiUrl: client.baseUrl },
  };
}

export function Layout({ children }: { children: React.ReactNode }) {
  const nonce = useNonce();
  return (
    <html lang="ja">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1, interactive-widget=resizes-content" />
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
  const { health, unavailable, problem, counts, reportsLive, approvalsPending, gui, session } = loaderData;
  const revalidator = useRevalidator();
  const disconnected = unavailable || health === null;
  const showBanner = disconnected || problem !== null;
  const { pathname } = useLocation();

  // celeris 停止中は 5 秒ごとに root だけ再検証し、復旧したらバナーを消す（§6.5）
  useEffect(() => {
    if (!disconnected) return;
    const id = setInterval(() => {
      if (revalidator.state === "idle") revalidator.revalidate();
    }, RECHECK_MS);
    return () => clearInterval(id);
  }, [disconnected, revalidator]);

  // SSE（`/events`）を root で 1 本だけ張り、`task.event` / `daemon` / `reset` を受けたらルートを再検証する（docs/DESIGN.md §6.3, ADR-0004 D2）。
  useCelerisStream({ enabled: session.authenticated });

  // 未認証（/login）: ナビゲーションもフッタも出さない（docs/adr/0008 D5）
  if (!session.authenticated) {
    return (
      <div className="flex min-h-screen flex-col px-4">
        <Outlet />
      </div>
    );
  }

  const connected = !disconnected && problem === null;
  return (
    <ConsoleComposerProvider>
      <div className="min-h-screen lg:grid lg:grid-cols-[16rem_1fr]">
        {/* Phase 76（ADR-0055 D1 拡張、フォーカスの可視性）: キーボード・スクリーンリーダー利用者が
          ナビ（サイドバー / モバイル上部の帯）を毎回たどらずに本文へ飛べるようにする。既定は視覚的に隠し、
          フォーカスが当たったときだけ見せる（`sr-only focus:not-sr-only`。`focus-visible:` ではなく
          `focus:` にしているのは、Tab で来た人にだけ見えれば十分で、かつスクリーンリーダーの仮想カーソルが
          フォーカスを当てる操作も拾いたいため）。 */}
        <a
          href="#main-content"
          // ADR-0055 D1-2 のタップ領域検査の対象外（`data-touch-ok`）: `sr-only` は未フォーカス時わざと
          // 1×1 に潰す（フォーカスが当たったときだけ `focus:not-sr-only` で 44×44 を超える大きさに戻る）。
          // タッチでは狙って押す対象ではなく、キーボード・スクリーンリーダー専用のリンクなので除外する。
          data-touch-ok
          className="sr-only focus:not-sr-only focus:fixed focus:top-2 focus:left-2 focus:z-50 focus:rounded-lg focus:bg-primary focus:px-4 focus:py-2 focus:text-sm focus:font-medium focus:text-primary-fg focus:shadow-lg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
        >
          本文へ
        </a>
        <NotificationsWatcher reportsLive={reportsLive} />
        <Sidebar
          approvals={counts?.approvals ?? 0}
          reportsLive={reportsLive}
          approvalsPending={approvalsPending}
          connected={connected}
          celerisVersion={health?.celeris_version ?? null}
          logoutEnabled={session.enabled}
        />
        {/* ADR-0055 D2（ラウンド 2）: モバイルはナビが下部固定タブなので、上には帯（ロゴ・接続状態）だけ置く。 */}
        <MobileTopBar connected={connected} logoutEnabled={session.enabled} />
        {/* 下部固定タブ（`h-16` + `env(safe-area-inset-bottom)`）に隠れないよう、本文側に余白を積む
          （ADR-0055 D1-5。数値は `pb-28`（112px） > タブ高さ 64px + 実機の safe-area の余裕）。 */}
        <div className="flex min-w-0 flex-col pb-28 lg:pb-0">
          <main
            id="main-content"
            tabIndex={-1}
            className="mx-auto w-full max-w-6xl flex-1 px-4 py-6 sm:px-6 lg:px-10 lg:py-10"
          >
            {showBanner && <CelerisBanner celerisApiUrl={gui.celerisApiUrl} problem={problem} />}
            <div className="animate-fade-in">
              <Outlet />
            </div>
          </main>
          <footer
            // ADR-0055 D1-4: 本文 14px 以上。デスクトップの見た目は変えず（`lg:` で元の `text-xs` に戻す）、
            // モバイルだけ `text-sm` に上げる。
            className="mx-auto w-full max-w-6xl border-t border-border px-4 py-5 text-sm text-fg-subtle sm:px-6 lg:px-10 lg:text-xs"
            data-testid="footer"
          >
            <p className="flex flex-wrap items-center gap-x-2 gap-y-1">
              <span className="font-medium text-fg-muted">Celeris {gui.version}</span>
              {health && (
                <>
                  <span aria-hidden="true">·</span>celeris {health.celeris_version} · api_version {health.api_version} ·
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
                  <dt className="text-fg-subtle">celeris_version</dt>
                  <dd className="font-mono text-fg-muted" data-testid="celeris_version">
                    {health.celeris_version}
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
          {/* ADR-0057（Phase 92、fixed-overlay 検査で見つけた既存の潜在バグの解消）: composer（モバイル、
              `position: fixed`）は `footer` より下の階層にいるビューポート基準の固定要素なので、
              footer 自身はこの spacer が無いと隠れうる（`pb-28`〈タブバー分だけの余白〉だけでは composer
              の高さぶんが足りない）。ページ全体の高さが小さい画面〈短い Console〉でページ末尾まで
              スクロールすると起きる。これまで検知できなかったのは、`.animate-fade-in` の containing-block
              バグ（同 ADR）で `load` 直後は composer がビューポート基準で測れず、`mobile-audit.mjs` の
              `fixed-overlay` 検査がたまたま素通りしていたため（今回の構造修正でこの見せかけの合格が消え、
              本当の状態が見えるようになった）。`ConsoleComposer` と同じ実測値（`mobileHeight`）を使う。 */}
          {isConsoleComposerPathname(pathname) && <FooterComposerSpacer />}
        </div>
        <MobileTabBar approvalsPending={approvalsPending} reportsLive={reportsLive} />
        {/* ADR-0057（Phase 92）: モバイル版 composer。`<Outlet/>`（`.animate-fade-in` の中）の外、
          `MobileTabBar` と同じ階層でレンダーする（ページ遷移アニメーションが `position: fixed` の
          containing block を差し替える Chromium の挙動の影響を構造的に受けない。ADR-0055 ラウンド 15 の
          `viewport-units` バグの根絶。詳細は ADR-0057）。`key={pathname}` は「ページを離れたら入力中の
          テキストを破棄する」ため（`root` 自体は画面遷移で unmount しないので、これが無いと `/` ↔
          `/org/:id` を行き来しても入力欄の下書きが残ってしまう。`?scope=` だけの変化では `pathname` は
          変わらないので下書きは保たれる）。Console 以外の画面では `ConsoleComposer` 自身が何も描かない。 */}
        {isConsoleComposerPathname(pathname) && <ConsoleComposer key={pathname} variant="mobile" />}
      </div>
    </ConsoleComposerProvider>
  );
}

/**
 * ADR-0057（Phase 92）: footer が composer（モバイル、`position: fixed`）の下に隠れないための余白。
 * `ConsoleComposerProvider` の内側（`App` の子）でなければ `useConsoleComposerContext` は使えないので、
 * 呼び出し側（`App`）とは別のコンポーネントに分けている。
 */
function FooterComposerSpacer() {
  const { mobileHeight } = useConsoleComposerContext();
  return (
    <div
      aria-hidden="true"
      data-testid="footer-composer-spacer"
      className="lg:hidden"
      style={{ height: mobileHeight ?? DEFAULT_MOBILE_COMPOSER_HEIGHT_PX }}
    />
  );
}

type NavItem = { href: string; label: string; icon: IconName; badge?: "approvals" | "reports" | "org_approvals" };

/**
 * ナビゲーションのグループ（docs/adr/0011 D3、Phase G13a で SPEC §4 の順に組み替え。ADR-0033 D8）。
 * 先頭は SPEC §4 の 6 画面の順（Console・組織・案件・報告・認可・成果物）。成果物は G13c、認可は G13d で実物になった。
 * Console（ADR-0048 D4、Phase G22）が `/` の入口になったので、ナビ先頭は「秘書」ではなく Console 自身を指す。
 * 既存のタスク・プロバイダ・アカウント・クラスタの画面は「裏方」区画にまとめて下げる
 * （人が見る単位は案件と組織になり、タスクは裏方に下がる）。
 */
const NAV_GROUPS: { label: string; items: NavItem[] }[] = [
  {
    label: "業務",
    items: [
      { href: "/", label: "Console", icon: "message" },
      { href: "/org", label: "組織", icon: "users" },
      { href: "/projects", label: "案件", icon: "folder" },
      // ADR-0044 D4（Phase 53）: 案件のタスクを 6 列で見るボード
      { href: "/board", label: "ボード", icon: "layers" },
      // ADR-0047 D5（Phase 61 / G21）: 組織が覚えていること（正本は `[knowledge] root` の Markdown）
      { href: "/knowledge", label: "知識", icon: "database" },
      { href: "/reports", label: "報告", icon: "send", badge: "reports" },
      { href: "/approvals", label: "認可", icon: "shield", badge: "org_approvals" },
      { href: "/artifacts", label: "成果物", icon: "file" },
    ],
  },
  {
    label: "裏方",
    items: [
      { href: "/inbox", label: "受信箱", icon: "inbox", badge: "approvals" },
      { href: "/tasks", label: "一覧", icon: "list" },
      { href: "/graph", label: "DAG", icon: "network" },
      { href: "/tasks/new", label: "新規タスク", icon: "plus" },
      { href: "/plans/new", label: "新規 Plan", icon: "sparkles" },
      { href: "/daemon", label: "デーモン", icon: "activity" },
      { href: "/providers", label: "プロバイダ", icon: "cpu" },
      { href: "/accounts", label: "アカウント", icon: "users" },
      { href: "/clusters", label: "クラスタ", icon: "server" },
      { href: "/releases", label: "リリース", icon: "layers" },
    ],
  },
  { label: "ヘルプ", items: [{ href: "/help", label: "使い方", icon: "book" }] },
];

/**
 * 下部固定タブ（ADR-0055 D2）: Console / ボード / 案件 / 認可 / その他。人が最も見る画面を
 * 4 本にしぼり、残りは「その他」のシートにまとめる（`MOBILE_OTHER`）。`NAV_GROUPS` とは別に持つ
 * （デスクトップの並びと優先順位が違うため）。
 */
const MOBILE_TABS: NavItem[] = [
  { href: "/", label: "Console", icon: "message" },
  { href: "/board", label: "ボード", icon: "layers" },
  { href: "/projects", label: "案件", icon: "folder" },
  { href: "/approvals", label: "認可", icon: "shield", badge: "org_approvals" },
];

/** 「その他」シートの一覧（ADR-0055 D2 の指定どおり）。 */
const MOBILE_OTHER: NavItem[] = [
  { href: "/org", label: "組織", icon: "users" },
  { href: "/reports", label: "報告", icon: "send", badge: "reports" },
  { href: "/releases", label: "リリース", icon: "layers" },
  { href: "/knowledge", label: "知識", icon: "database" },
  { href: "/clusters", label: "クラスタ", icon: "server" },
  { href: "/accounts", label: "アカウント", icon: "users" },
  { href: "/help", label: "使い方", icon: "book" },
];

function isActive(pathname: string, href: string): boolean {
  if (href === "/") return pathname === "/";
  if (href === "/inbox") return pathname === "/inbox";
  if (href === "/tasks") return pathname === "/tasks" || (pathname.startsWith("/tasks/") && pathname !== "/tasks/new");
  // `/org/cos`（旧 `/org/secretary`）は別のナビ項目（Console）ではなく組織の木の 1 ノードなので、
  // 「組織」ナビは `/org` そのものだけを active にする。
  if (href === "/org") return pathname === "/org";
  return pathname === href || pathname.startsWith(`${href}/`);
}

/**
 * デスクトップ専用のサイドバー（ADR-0055 D2 ラウンド 2: モバイルのナビは下部固定タブ
 * `MobileTabBar` に置き換えた。このコンポーネントは `lg:` 以上でしか出ない）。
 */
function Sidebar({
  approvals,
  reportsLive,
  connected,
  approvalsPending,
  celerisVersion,
  logoutEnabled,
}: {
  approvals: number;
  reportsLive: ReportsLive | null;
  approvalsPending: number;
  connected: boolean;
  celerisVersion: string | null;
  logoutEnabled: boolean;
}) {
  const { pathname } = useLocation();
  return (
    <aside className="hidden border-r border-border bg-surface/95 backdrop-blur-xl lg:sticky lg:top-0 lg:z-30 lg:block lg:h-screen">
      <div className="flex h-full flex-col items-stretch px-3 py-5">
        <div className="px-2">
          <a href="/" className="group flex items-center gap-2.5 rounded-lg no-underline">
            <span className="grid size-8 place-items-center rounded-lg bg-linear-to-br from-primary via-primary to-teal text-white shadow-md ring-1 ring-white/20 transition-transform group-hover:scale-105 dark:text-bg">
              <Icon name="zap" className="size-4" strokeWidth={2.2} />
            </span>
            <span className="text-[0.95rem] font-bold tracking-tight text-fg">Celeris</span>
          </a>
        </div>

        <nav aria-label="メイン" className="mt-7 flex flex-1 flex-col items-stretch gap-5 overflow-y-auto">
          {NAV_GROUPS.map((group) => (
            <div key={group.label} className="block">
              <p className="px-3 pb-1.5 text-[0.7rem] font-semibold uppercase tracking-wider text-fg-subtle">
                {group.label}
              </p>
              <ul className="flex flex-col gap-0.5">
                {group.items.map((item) => {
                  const active = isActive(pathname, item.href);
                  const unreadSecretary = reportsLive?.unread_secretary ?? 0;
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
                            className="absolute inset-y-1.5 -left-3 block w-1 rounded-r-full bg-primary"
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
                        {item.badge === "reports" && unreadSecretary > 0 && (
                          <span
                            data-testid="reports-unread-badge"
                            className={cn(
                              "ml-auto min-w-5 rounded-full px-1.5 py-0.5 text-center text-[0.7rem] leading-none font-bold tabular-nums shadow-sm",
                              reportsBadgeTone(reportsLive) === "danger"
                                ? "bg-danger text-white dark:text-bg"
                                : "bg-surface-2 text-fg-muted",
                            )}
                          >
                            {unreadSecretary}
                          </span>
                        )}
                        {item.badge === "org_approvals" && approvalsPending > 0 && (
                          <span
                            data-testid="approvals-pending-badge"
                            className="ml-auto min-w-5 rounded-full bg-danger px-1.5 py-0.5 text-center text-[0.7rem] leading-none font-bold text-white tabular-nums shadow-sm dark:text-bg"
                          >
                            {approvalsPending}
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

        {/* 接続状態とログアウト（モバイルの帯は `MobileTopBar` が別に持つので、data-testid は重複しない）。 */}
        <div className="mt-4 space-y-2 border-t border-border px-1 pt-4">
          <ConnectionPill connected={connected} celerisVersion={celerisVersion} />
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

/**
 * モバイルの上部の帯（ADR-0055 D2 ラウンド 2）。ナビは下部固定タブに移したので、ここはロゴと
 * 接続状態・ログアウトだけ（`lg:hidden`。デスクトップは `Sidebar` がこれらを持つので二重に描かない）。
 */
function MobileTopBar({ connected, logoutEnabled }: { connected: boolean; logoutEnabled: boolean }) {
  return (
    <header className="sticky top-0 z-30 flex items-center justify-between gap-2 border-b border-border bg-surface/95 px-4 py-2.5 backdrop-blur-xl lg:hidden">
      <a href="/" className="group flex min-h-11 items-center gap-2.5 rounded-lg no-underline">
        <span className="grid size-8 place-items-center rounded-lg bg-linear-to-br from-primary via-primary to-teal text-white shadow-md ring-1 ring-white/20 dark:text-bg">
          <Icon name="zap" className="size-4" strokeWidth={2.2} />
        </span>
        <span className="text-[0.95rem] font-bold tracking-tight text-fg">Celeris</span>
      </a>
      <div className="flex items-center gap-2">
        <ConnectionPill connected={connected} celerisVersion={null} className="px-2.5 py-1.5" />
        {logoutEnabled && (
          <Form method="post" action="/logout">
            <button
              type="submit"
              data-testid="logout-mobile"
              aria-label="ログアウト"
              className="grid min-h-11 min-w-11 place-items-center rounded-lg text-fg-muted hover:bg-surface-2 hover:text-fg"
            >
              <Icon name="logout" className="size-4.5" />
            </button>
          </Form>
        )}
      </div>
    </header>
  );
}

/**
 * 下部固定タブ（ADR-0055 D2）。`env(safe-area-inset-bottom)` を padding で確保し、実機のホーム
 * インジケータの上にタブが来るようにする（Playwright は safe-area を再現しない。P-G23-1 の続き）。
 * 「その他」はシート（下からせり出す一覧）を開く。開いていない間は DOM に無いので、機械検査
 * （D1-5 固定要素の検査等）には影響しない。
 */
function MobileTabBar({
  approvalsPending,
  reportsLive,
}: {
  approvalsPending: number;
  reportsLive: ReportsLive | null;
}) {
  const { pathname } = useLocation();
  const [sheetOpen, setSheetOpen] = useState(false);
  const closeButton = useRef<HTMLButtonElement>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: 画面が変わったらシートは閉じる。
  useEffect(() => setSheetOpen(false), [pathname]);
  useEffect(() => {
    if (sheetOpen) closeButton.current?.focus();
  }, [sheetOpen]);
  const unreadSecretary = reportsLive?.unread_secretary ?? 0;
  const otherActive = MOBILE_OTHER.some((item) => isActive(pathname, item.href));

  // Escape でシートを閉じる（背景はボタンにして a11y の静的要素の警告を避ける）。
  useEffect(() => {
    if (!sheetOpen) return;
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setSheetOpen(false);
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [sheetOpen]);

  return (
    <>
      {sheetOpen && (
        <div className="fixed inset-0 z-40 lg:hidden">
          <button
            type="button"
            aria-label="閉じる"
            onClick={() => setSheetOpen(false)}
            className="absolute inset-0 bg-black/40"
          />
          <div
            role="dialog"
            aria-modal="true"
            aria-label="その他"
            data-testid="mobile-more-sheet"
            className="absolute inset-x-0 bottom-16 max-h-[70dvh] overflow-y-auto rounded-t-2xl border-t border-border bg-surface p-4 shadow-lg"
            style={{ paddingBottom: "calc(1rem + env(safe-area-inset-bottom))" }}
          >
            <div className="mb-3 flex items-center justify-between">
              <p className="text-sm font-semibold text-fg">その他</p>
              <button
                ref={closeButton}
                type="button"
                onClick={() => setSheetOpen(false)}
                data-testid="mobile-more-close"
                aria-label="閉じる"
                className="grid min-h-11 min-w-11 place-items-center rounded-lg text-fg-muted hover:bg-surface-2 hover:text-fg"
              >
                <Icon name="x" className="size-4.5" />
              </button>
            </div>
            {/* ADR-0055 ラウンド 14（U-G37-1 の受け入れ条件 2）: 表示タイムゾーンの視聴者設定。
                celeris への問い合わせは無い（`localStorage` だけの見た目の好み）。 */}
            <TimeZonePreference className="mb-3 rounded-lg border border-border bg-surface-2/40 p-3" />
            <ul className="grid grid-cols-2 gap-2">
              {MOBILE_OTHER.map((item) => {
                const active = isActive(pathname, item.href);
                return (
                  <li key={item.href}>
                    <a
                      href={item.href}
                      aria-current={active ? "page" : undefined}
                      className={cn(
                        "flex min-h-11 items-center gap-2 rounded-lg border px-3 py-2 text-sm font-medium no-underline",
                        active
                          ? "border-primary-border bg-primary-soft text-primary-soft-fg"
                          : "border-border text-fg-muted hover:bg-surface-2 hover:text-fg",
                      )}
                    >
                      <Icon name={item.icon} className="size-4 shrink-0" />
                      <span className="min-w-0 truncate">{item.label}</span>
                      {item.badge === "reports" && unreadSecretary > 0 && (
                        <span
                          data-testid="mobile-reports-unread-badge"
                          className="ml-auto min-w-5 rounded-full bg-danger px-1.5 py-0.5 text-center text-xs leading-none font-bold text-white tabular-nums shadow-sm dark:text-bg"
                        >
                          {unreadSecretary}
                        </span>
                      )}
                    </a>
                  </li>
                );
              })}
            </ul>
          </div>
        </div>
      )}
      <nav
        aria-label="メイン（モバイル）"
        data-testid="mobile-tabbar"
        className="fixed inset-x-0 bottom-0 z-30 grid h-16 grid-cols-5 border-t border-border bg-surface/95 backdrop-blur-xl lg:hidden"
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        {MOBILE_TABS.map((item) => {
          const active = isActive(pathname, item.href);
          return (
            <a
              key={item.href}
              href={item.href}
              aria-current={active ? "page" : undefined}
              className={cn(
                "relative flex min-h-11 flex-col items-center justify-center gap-0.5 text-sm font-medium no-underline",
                active ? "text-primary" : "text-fg-muted",
              )}
            >
              <Icon name={item.icon} className="size-5" />
              <span>{item.label}</span>
              {item.badge === "org_approvals" && approvalsPending > 0 && (
                <span
                  data-testid="mobile-approvals-pending-badge"
                  aria-hidden="true"
                  className="absolute top-1.5 right-[calc(50%-1.4rem)] size-2 rounded-full bg-danger"
                />
              )}
            </a>
          );
        })}
        <button
          type="button"
          onClick={() => setSheetOpen((value) => !value)}
          aria-expanded={sheetOpen}
          data-testid="mobile-tabbar-more"
          className={cn(
            "relative flex min-h-11 flex-col items-center justify-center gap-0.5 text-sm font-medium",
            sheetOpen || otherActive ? "text-primary" : "text-fg-muted",
          )}
        >
          <Icon name="more" className="size-5" />
          その他
          {unreadSecretary > 0 && !otherActive && (
            <span
              aria-hidden="true"
              className="absolute top-1.5 right-[calc(50%-1.4rem)] size-2 rounded-full bg-danger"
            />
          )}
        </button>
      </nav>
    </>
  );
}

function ConnectionPill({
  connected,
  celerisVersion,
  className,
}: {
  connected: boolean;
  celerisVersion: string | null;
  className?: string;
}) {
  return (
    <div
      className={cn(
        // ADR-0055 D1-4: モバイルは text-sm、デスクトップは元の text-xs のまま。
        "flex items-center gap-2 rounded-lg border border-border bg-surface-2/60 px-3 py-2 text-sm text-fg-muted lg:text-xs",
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
      <span className="font-medium text-fg">{connected ? "celeris 接続中" : "celeris 未接続"}</span>
      {connected && celerisVersion && (
        <span className="ml-auto hidden font-mono text-fg-subtle sm:inline">v{celerisVersion}</span>
      )}
    </div>
  );
}

export function CelerisBanner({ celerisApiUrl, problem }: { celerisApiUrl: string; problem: string | null }) {
  return (
    <Alert
      role="alert"
      data-testid="celeris-banner"
      tone="danger"
      icon={problem ? "lock" : "wifiOff"}
      className="mb-6"
      title={
        problem ? `celeris が要求を拒否しました（${celerisApiUrl}）` : `celeris に接続できません（${celerisApiUrl}）`
      }
    >
      <p>
        {problem
          ? `celeris の応答: ${problem}。CELERIS_API_TOKEN_FILE が celeris の token_file と一致しているか確認してください。`
          : "celeris が起動しているか、CELERIS_API_URL を確認してください。5 秒ごとに再接続を試みます。"}
        操作はできません。celerisctl は従来どおり使えます。
      </p>
    </Alert>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  const rootData = useRouteLoaderData("root") as Route.ComponentProps["loaderData"] | undefined;
  // `/tasks` 等の子ルートが celeris のエラーを `Response` として投げてここまで来たとき（`celerisErrorResponse`、
  // docs/adr/0004 D6）、汎用のエラー画面ではなく `/` と同じバナー等を出す（`/` 自身は inbox.tsx が catch する
  // のでここには来ない）。本番ビルドは素の Error を渡す前に汎用 500 へサニタイズするため、`Response` 以外は
  // 判別できない（= 本当に予期しないエラーとして扱ってよい）。
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as CelerisRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="mx-auto max-w-3xl p-4 pt-16">
          <CelerisBanner celerisApiUrl={data.baseUrl ?? ""} problem={null} />
          <RouteRecovery />
        </main>
      );
    }
    // celeris の 401（トークン無し・不一致）は接続不可と同じ形のバナーで知らせる（docs/adr/0008 D6）
    if (data.status === 401) {
      return (
        <main className="mx-auto max-w-3xl p-4 pt-16">
          <CelerisBanner
            celerisApiUrl={rootData?.gui.celerisApiUrl ?? ""}
            problem={`${data.status} ${data.code ?? "unauthorized"}`}
          />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-3xl p-4 pt-16">
        <ErrorPanel title={data.status === 404 ? "404" : `エラー ${data.status}`} recover={data.status !== 404}>
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
      <ErrorPanel title={message} recover>
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

function ErrorPanel({
  title,
  children,
  recover = false,
}: {
  title: string;
  children: React.ReactNode;
  recover?: boolean;
}) {
  return (
    <div className="rounded-2xl border border-border bg-surface p-8 shadow-md">
      <span className="grid size-11 place-items-center rounded-xl bg-danger-soft text-danger-soft-fg ring-1 ring-danger-border">
        <Icon name="alert" className="size-5" />
      </span>
      <h1 className="mt-4 text-2xl font-bold tracking-tight text-fg">{title}</h1>
      <div className="mt-2 text-sm text-fg-muted">{children}</div>
      {recover && <RouteRecovery />}
      <a href="/" className={buttonClass({ variant: "secondary", className: "mt-6" })}>
        <Icon name="arrowLeft" />
        Console へ戻る
      </a>
    </div>
  );
}
