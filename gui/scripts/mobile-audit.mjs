// gui/scripts/mobile-audit.mjs — ADR-0055 D1 の機械検査。
//
// 偽の celeris（node:http、`test/mock-celeris/fixtures.ts` の値を使う）と `pnpm build` した GUI の
// `server.js` を、どちらも 127.0.0.1 の空きポートで起動し、Playwright Chromium（393×851、
// `deviceScaleFactor 2.75`、Nothing Phone 2a の Chrome UA）で ADR-0055 D1 の一覧にある画面を開き、
// D1 の 1〜6 を検査する。1 件でも落ちたら非 0 で終わる（`gui/scripts/check-delivery.mjs` と同じ
// 「Build GUI first。実 celeris は起動しない。外部ネットワークに出ない」作り）。
//
// **`test/mock-celeris/server.ts` の `startMockCeleris` は使わない**: `./fixtures`（拡張子なし）を
// 相対 import しており、これは vite/vitest の TS 解決の下でしか解決できない（Node 24 の組み込み型剥がしは
// 拡張子の補完をしない）。`fixtures.ts` 自身は celeris の型を **type-only** import しているだけ
// （`gui/CLAUDE.md` の「GUI から celeris に入る依存は作らない」を保つ側の作り）なので、そちらは
// `scripts/check-delivery.mjs` と同じ手口でそのまま import できる。ルーティングは check-delivery.mjs に
// 合わせて素の `node:http` で書く。
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import { createRequire } from "node:module";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(path.join(GUI_DIR, "package.json"));
const { chromium } = require("@playwright/test");

const fx = await import(path.join(GUI_DIR, "test/mock-celeris/fixtures.ts"));

const VIEWPORT = { width: 393, height: 851 };
const DEVICE_SCALE_FACTOR = 2.75;
const USER_AGENT =
  "Mozilla/5.0 (Linux; Android 14; Nothing Phone 2a) AppleWebKit/537.36 (KHTML, like Gecko) " +
  "Chrome/128.0.0.0 Mobile Safari/537.36";

const OUT_DIR = path.join(GUI_DIR, "test/mobile-audit");
const REPORT_PATH = path.join(OUT_DIR, "report.json");
const TASK_ID = "01BOARDTASK00000000000001";
const PROJECT_ID = "p1";

fs.mkdirSync(OUT_DIR, { recursive: true });
for (const name of fs.readdirSync(OUT_DIR)) fs.rmSync(path.join(OUT_DIR, name), { force: true });

/** ADR-0055 D1 が挙げた画面。`mock` の id に合わせてある。 */
const ROUTES = [
  { route: "home", path: "/" },
  { route: "org", path: "/org" },
  { route: "org-node", path: "/org/coding-poc" },
  // ADR-0054 D3（Phase 68 追加。ADR-0055 D1 の元の一覧には無い）: 部門長ノードの詳細（継続セッション表示）。
  { route: "org-detail", path: "/org?selected=coding" },
  { route: "projects", path: "/projects" },
  { route: "project-detail", path: `/projects/${PROJECT_ID}` },
  { route: "project-docs", path: `/projects/${PROJECT_ID}/docs` },
  { route: "board", path: "/board" },
  { route: "approvals", path: "/approvals" },
  { route: "reports", path: "/reports" },
  { route: "releases", path: "/releases" },
  { route: "knowledge", path: "/knowledge" },
  { route: "knowledge-inbox", path: "/knowledge/inbox" },
  { route: "clusters", path: "/clusters" },
  { route: "accounts", path: "/accounts" },
  { route: "help", path: "/help" },
  { route: "task-overview", path: `/tasks/${TASK_ID}?tab=overview` },
  { route: "task-timeline", path: `/tasks/${TASK_ID}?tab=timeline` },
  { route: "task-changes", path: `/tasks/${TASK_ID}?tab=changes` },
  { route: "task-files", path: `/tasks/${TASK_ID}?tab=files` },
  { route: "task-artifacts", path: `/tasks/${TASK_ID}?tab=artifacts` },
];

function getFreePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

function sendSse(res) {
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store" });
  res.write(": mobile-audit fixture\n\n");
  // 閉じない（実際の SSE と同じ。Playwright は networkidle を待たないのでハングしない）。
}

function sendJson(res, status, body) {
  res.writeHead(status, { "content-type": "application/json; charset=utf-8", "cache-control": "no-store" });
  res.end(JSON.stringify(body));
}

/**
 * 素の celeris もどき（`node:http`、`gui/scripts/check-delivery.mjs` と同じ作り）。`on` は
 * 完全一致のパス（クエリ抜き）だけを見る。GET しか要らない（監査は読み取りだけ）。
 */
function createFakeCeleris() {
  const routes = new Map();
  const on = (method, routePath, handler) => routes.set(`${method} ${routePath}`, handler);
  const server = http.createServer((req, res) => {
    const pathname = new URL(req.url ?? "/", "http://fake-celeris.invalid").pathname;
    const handler = routes.get(`${req.method ?? "GET"} ${pathname}`);
    if (!handler) {
      sendJson(res, 404, { code: "not_found", detail: `no route for ${req.method} ${pathname}` });
      return;
    }
    handler(req, res);
  });
  return {
    on,
    listen: () =>
      new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(0, "127.0.0.1", () => {
          const { port } = server.address();
          resolve(`http://127.0.0.1:${port}`);
        });
      }),
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

async function setupMockCeleris() {
  const fake = createFakeCeleris();
  const baseUrl = await fake.listen();
  const mock = { on: fake.on, baseUrl, close: fake.close };

  mock.on("GET", "/api/v1/health", (_req, res) => sendJson(res, 200, fx.defaultHealth));
  mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [fx.project({ id: PROJECT_ID })] }));
  mock.on("GET", "/api/v1/tasks", (_req, res) =>
    sendJson(res, 200, {
      items: [fx.taskSummary({ assignee: "coding-poc", project_id: PROJECT_ID })],
      total: 1,
      counts_by_status: { running: 1 },
      next_cursor: null,
    }),
  );
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/timeline`, (_req, res) => sendJson(res, 200, fx.timeline([], TASK_ID)));
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/comments`, (_req, res) =>
    sendJson(res, 200, { items: [fx.taskComment({ task_id: TASK_ID })] }),
  );

  mock.on("GET", "/api/v1/knowledge/tree", (_req, res) => sendJson(res, 200, fx.knowledgeTree()));
  mock.on("GET", "/api/v1/knowledge/page", (_req, res) => sendJson(res, 200, fx.knowledgePage()));
  mock.on("GET", "/api/v1/knowledge/inbox", (_req, res) => sendJson(res, 200, fx.knowledgeInbox()));

  // フェーズ 73（ADR-0055 D2 ラウンド 5、U12）: `gui/test/mock-celeris/fixtures.ts` の `orgList()`
  // （`coding-poc` の下に 3 段のサブツリーを持つ）をそのまま使う。以前はここに 3 ノードだけの
  // その場限りの木を書いていたが、それでは `/org` の開閉トグル（フェーズ 72）が畳む中身がほぼ無く、
  // 「大きな組織で畳むと画面が短くなる」効果を監査のスクリーンショットで確認できなかった
  // （Phase G26 の未解決事項 U12）。部門長の継続セッション表示（`/org?selected=coding`）も
  // 引き続きこの固定データで確かめる。
  mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, fx.orgList()));

  mock.on("GET", "/api/v1/config", (_req, res) =>
    sendJson(res, 200, {
      api: { allowed_hosts: [], auth_required: false, bind: "127.0.0.1:0" },
      config_path: "/tmp/mobile-audit/config.toml",
      db: "/tmp/mobile-audit/celeris.sqlite3",
      error_cooldown_secs: 60,
      idle_timeout_secs: 900,
      kill_grace_secs: 10,
      lease_grace_secs: 30,
      max_concurrency: 4,
      max_requeues: 3,
      plan_auto_accept: false,
      providers: [],
      retry_backoff_base_secs: 5,
      retry_backoff_max_secs: 300,
      review_timeout_secs: 600,
      reviewer: { tier: "standard" },
      tick_ms: 1000,
      workspace_root: "/tmp/mobile-audit/workspace",
      genres: [{ id: "coding" }],
    }),
  );

  mock.on("GET", "/api/v1/clusters", (_req, res) =>
    sendJson(res, 200, {
      items: [
        {
          id: "gpu1",
          host: "gpu1.internal",
          concurrency: 1,
          delete_on_push: false,
          env_keys: [],
          has_setup: false,
          rsync_excludes: [],
          sync: "rsync",
        },
        // ADR-0053 D3（Phase 66）: トンネルを持つクラスタ。ログインが要る状態も一緒に検査する。
        {
          id: "pegasus",
          host: "pegasus",
          concurrency: 2,
          delete_on_push: false,
          env_keys: [],
          has_setup: false,
          rsync_excludes: [],
          sync: "rsync",
          auth: "totp",
          connected: false,
          tunnel_login_needed: true,
          tunnel_forwards: [{ listen: "127.0.0.1:18000", target: "bnode150:18000", up: false }],
        },
      ],
    }),
  );

  // ADR-0053 D4（Phase 66）: 「LLM source」節。到達する/しない・oauth プール・tier 解決の 3 パターン。
  mock.on("GET", "/api/v1/llm/sources", (_req, res) =>
    sendJson(res, 200, {
      sources: [
        {
          id: "claude-oauth",
          kind: "claude-oauth",
          enabled: true,
          accounts: [
            {
              id: "claude-a",
              logged_in: true,
              remaining: 0.62,
              remaining_short: 0.62,
              remaining_long: 0.81,
            },
          ],
          last_hour_requests: 12,
          last_hour_prompt_tokens: 3400,
          last_hour_completion_tokens: 900,
        },
        {
          id: "openai-compatible:qwen",
          kind: "openai-compatible",
          enabled: true,
          reachable: false,
          accounts: [],
          last_hour_requests: 40,
          last_hour_prompt_tokens: 9000,
          last_hour_completion_tokens: 5000,
        },
      ],
      celeris_tiers: [
        { tier: "frontier", resolves_to: "claude-oauth" },
        { tier: "standard", resolves_to: "claude-oauth" },
        { tier: "cheap", resolves_to: null },
      ],
    }),
  );

  mock.on(`GET`, `/api/v1/tasks/${TASK_ID}`, (_req, res) =>
    sendJson(res, 200, {
      actions: [],
      answers: [],
      approvals: [],
      children: [],
      criteria: [],
      delegated: [],
      dependencies: [],
      dependents: [],
      priority_label: "P2",
      prior_review: [],
      runs: [],
      task: fx.task({ id: TASK_ID, assignee: "coding-poc" }),
      timers: {
        consecutive_requeues: 0,
        consecutive_reviewer_requeues: 0,
        max_requeues: 2,
        now: "2026-09-21T00:00:00Z",
      },
    }),
  );
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/events`, (_req, res) => sendJson(res, 200, { has_more: false, items: [] }));
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/artifacts`, (_req, res) => sendJson(res, 200, { items: [] }));
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/tree`, (_req, res) => sendJson(res, 200, fx.treeView()));
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/changes`, (_req, res) =>
    sendJson(res, 200, fx.changesView({ task_id: TASK_ID })),
  );

  mock.on("GET", `/api/v1/projects/${PROJECT_ID}`, (_req, res) =>
    sendJson(res, 200, {
      project: fx.project({ id: PROJECT_ID }),
      milestones: [fx.milestone({ project_id: PROJECT_ID })],
      tasks: [
        {
          id: TASK_ID,
          conversation: false,
          depends_on: [],
          status: "running",
          title: "関連研究を調べる",
          assignee: "coding-poc",
        },
      ],
    }),
  );
  mock.on("GET", `/api/v1/projects/${PROJECT_ID}/docs`, (_req, res) =>
    sendJson(res, 200, fx.docsTree({ project_id: PROJECT_ID })),
  );
  mock.on("GET", `/api/v1/projects/${PROJECT_ID}/docs/page`, (_req, res) =>
    sendJson(res, 200, fx.docPage({ project_id: PROJECT_ID })),
  );

  mock.on("GET", "/api/v1/approvals", (_req, res) =>
    sendJson(res, 200, {
      items: [
        {
          id: "appr1",
          node_id: "coding-poc",
          project_id: PROJECT_ID,
          task_id: TASK_ID,
          question: "本番のクラスタに接続してよいですか",
          created_at: "2026-09-21T00:00:00Z",
        },
      ],
    }),
  );
  mock.on("GET", "/api/v1/standing-rules", (_req, res) => sendJson(res, 200, { items: [] }));

  mock.on("GET", "/api/v1/reports", (_req, res) =>
    sendJson(res, 200, {
      items: [
        {
          id: "r1",
          created_at: "2026-09-21T00:00:00Z",
          headline: "関連研究を 12 件集めた",
          kind: "result",
          level: 0,
          node_id: "cos",
          project_id: PROJECT_ID,
        },
      ],
    }),
  );
  mock.on("GET", "/api/v1/notify", (_req, res) =>
    sendJson(res, 200, { configured: false, recent: [], secret_id: "discord" }),
  );

  mock.on("GET", "/api/v1/releases", (_req, res) => sendJson(res, 200, fx.defaultReleases));

  mock.on("GET", "/api/v1/accounts", (_req, res) => sendJson(res, 200, { items: [], max_runs_per_account: 1 }));
  mock.on("GET", "/api/v1/secrets", (_req, res) => sendJson(res, 200, { items: [] }));

  mock.on("GET", "/api/v1/daemon", (_req, res) => sendJson(res, 200, { now: "2026-09-21T00:00:00Z", snapshot: null }));
  mock.on("GET", "/api/v1/inbox", (_req, res) =>
    sendJson(res, 200, { approvals: 0, attention: 0, by_status: {}, drafts: 0, questions: 0 }),
  );

  // フェーズ 73（ADR-0055 D2 ラウンド 5）: 育つ返事の積み上げ済みスナップショット
  // （`consoleGrowingReplySnapshot()`。ADR-0054 D2、`GET /console` は celeris 側で 1 回に組んで返す
  // ので GUI から見るとこの形で届く）を既定の流れに混ぜる。Phase 68 は `checkFixedOverlays`（D1-5）が
  // `console-stream`（内側で `overflow-y-auto` する箱）の中身を判定できず、実際にはスクロールで届く
  // 末尾要素を偽陽性で「固定バーに隠れている」と報告する既知の限界（U-G28-2 / P-G28-1）を理由に、
  // 意図的にここへ混ぜていなかった。今回 `checkFixedOverlays` に内側スクローラの追随を足した（下記）
  // ので、育つ返事の吹き出し（`ReplyStepRow` の折り畳み・トリム表示を含む）も実際に機械検査に通す。
  mock.on("GET", "/api/v1/console", (_req, res) =>
    sendJson(res, 200, fx.consolePage({ items: [...fx.consoleBlocks(), fx.consoleGrowingReplySnapshot()] })),
  );
  mock.on("GET", "/api/v1/stream", (_req, res) => sendSse(res));
  mock.on("GET", "/api/v1/console/stream", (_req, res) => sendSse(res));

  return mock;
}

async function waitForHealth(url, timeoutMs = 20_000) {
  const start = Date.now();
  for (;;) {
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      // まだ起動していない
    }
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${url}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

// ---------------------------------------------------------------------------
// D1 の検査本体。ページ内で評価する純関数は `page.evaluate` にそのまま渡す（DOM が要るので Node 側では書けない）。
// ---------------------------------------------------------------------------

function cssPathRef(el) {
  if (!(el instanceof Element)) return "";
  const parts = [];
  let node = el;
  let depth = 0;
  while (node && node.nodeType === 1 && depth < 6) {
    let part = node.tagName.toLowerCase();
    if (node.id) {
      part += `#${node.id}`;
      parts.unshift(part);
      break;
    }
    const testid = node.getAttribute?.("data-testid");
    if (testid) part += `[data-testid="${testid}"]`;
    const cls = typeof node.className === "string" ? node.className.trim().split(/\s+/).slice(0, 2).join(".") : "";
    if (cls) part += `.${cls}`;
    const parent = node.parentElement;
    if (parent) {
      const idx = Array.from(parent.children).indexOf(node);
      part += `:nth-child(${idx + 1})`;
    }
    parts.unshift(part);
    node = node.parentElement;
    depth += 1;
  }
  return parts.join(" > ");
}

/**
 * 要素自身だけでなく、祖先も含めて実際には描かれていない（`display:none` / `visibility:hidden`）かを見る。
 * `getComputedStyle` は `display`/`font-size` 等を要素自身の値で返す（`display` は継承しないので、
 * 祖先が `display:none` でも子要素自身の値は変わらない）。一方 `getBoundingClientRect()` は祖先が
 * 非表示ならボックスを持たず 0 になる。この非対称のせいで「自分の `style.display` だけ見る」判定は
 * デスクトップ専用の `<aside class="hidden lg:block">` の中身を見落とす。
 *
 * 加えて、閉じた `<details>` の中身（`<summary>` 以外の直接の子とその子孫）は Chromium では
 * `getComputedStyle` 上は `display:none` に**ならない**（実測で確認済み。ラウンド 2 の
 * `task-timeline` 監査で発見。D1-5 の固定要素チェックが誤検知した）。UA の既定の見せ方
 * （`details:not([open]) > *:not(summary)` は描かれない）を構造で判定する。
 */
function isNotVisible(el) {
  let node = el;
  while (node && node.nodeType === 1) {
    const style = getComputedStyle(node);
    if (style.display === "none" || style.visibility === "hidden") return true;
    const parent = node.parentElement;
    if (parent && parent.tagName === "DETAILS" && !parent.open && node.tagName !== "SUMMARY") return true;
    node = parent;
  }
  return false;
}

/** D1-1: 横はみ出し。overflow-x が auto/scroll なコンテナの中身は対象外（D1-6 と両立させるため）。 */
function checkOverflow() {
  const violations = [];
  const width = window.__MOBILE_AUDIT_WIDTH__;
  const docWidth = document.documentElement.scrollWidth;
  if (docWidth > width) {
    violations.push({
      rule: "overflow",
      selector: "html",
      box: { scrollWidth: docWidth },
      detail: `documentElement.scrollWidth=${docWidth} > ${width}`,
    });
  }
  const insideScroller = (el) => {
    let node = el.parentElement;
    while (node) {
      const style = getComputedStyle(node);
      if (style.overflowX === "auto" || style.overflowX === "scroll") return true;
      node = node.parentElement;
    }
    return false;
  };
  for (const el of document.querySelectorAll("body *")) {
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    if (rect.right > width + 0.5 && !insideScroller(el)) {
      violations.push({
        rule: "overflow",
        selector: cssPathRef(el),
        box: { top: rect.top, left: rect.left, right: rect.right, bottom: rect.bottom },
        detail: `right=${rect.right.toFixed(1)} > ${width}`,
      });
    }
  }
  return violations;
}

/** D1-2: タップ領域（44x44）。`data-touch-ok` を付けた要素・その子孫は対象外。 */
function checkTapTargets() {
  const violations = [];
  const selector = "button, a[href], input:not([type=hidden]), select";
  for (const el of document.querySelectorAll(selector)) {
    if (el.closest("[data-touch-ok]")) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    let rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    // checkbox/radio は `<label>` で包んで見た目より大きく押せるようにするのが通常の作り
    // （`~/components/ui/form.ts` の `chipLabelClass`）。実際に押せる範囲は包んでいる `<label>` の方なので、
    // そちらの大きさで判定する（要素自身が小さいこと自体は違反にしない）。
    if (el.tagName === "INPUT" && (el.type === "checkbox" || el.type === "radio")) {
      const label = el.closest("label");
      if (label) rect = label.getBoundingClientRect();
    }
    if (rect.width < 44 - 0.5 || rect.height < 44 - 0.5) {
      violations.push({
        rule: "tap-target",
        selector: cssPathRef(el),
        box: { width: rect.width, height: rect.height },
        detail: `${rect.width.toFixed(1)}x${rect.height.toFixed(1)} < 44x44`,
      });
    }
  }
  return violations;
}

/** D1-3: 状態バッジは 1 語。`data-status-badge` を付けた要素だけを対象にする。 */
function checkStatusBadges() {
  const violations = [];
  for (const el of document.querySelectorAll("[data-status-badge]")) {
    const text = (el.textContent ?? "").trim();
    if (text.length === 0) continue;
    if (/\s/.test(text) || text.length > 12) {
      violations.push({
        rule: "status-badge",
        selector: cssPathRef(el),
        box: {},
        detail: `badge text is not one word: ${JSON.stringify(text)}`,
      });
    }
  }
  return violations;
}

/** D1-4: 本文 14px 以上。`font-mono`（id / sha / パス）は対象外（D2）。 */
function checkFontSize() {
  const violations = [];
  const seen = new Set();
  for (const el of document.querySelectorAll("body *")) {
    if (el.children.length > 0) continue; // 直接テキストを持つ末端要素だけ
    const text = (el.textContent ?? "").trim();
    if (text.length === 0) continue;
    if (el.closest(".font-mono")) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    const size = Number.parseFloat(style.fontSize);
    if (!Number.isFinite(size)) continue;
    if (size < 14 - 0.1) {
      const key = cssPathRef(el);
      if (seen.has(key)) continue;
      seen.add(key);
      violations.push({
        rule: "font-size",
        selector: key,
        box: {},
        detail: `font-size=${size}px < 14px (text=${JSON.stringify(text.slice(0, 24))})`,
      });
    }
  }
  return violations;
}

/**
 * D1-5: 固定要素が内容を隠さない（一番下までスクロールしてから判定）。
 *
 * P-G28-1（Phase 68 の未解決事項 U-G28-2 の解消）: 文書全体の `window.scrollTo` だけでは、
 * `console-stream`（Console の `overflow-y-auto` な内側スクロール領域）のように**それ自身がスクロール
 * する箱**の中身までは末尾に送れない。そのため、育つ返事のような内側スクロール領域の中の要素が
 * 実際にはスクロールすれば読める位置にあるのに、「固定の入力欄より下＝隠れている」という偽陽性を生む
 * （Phase 68 で実際に踏んだので、育つ返事の既定モックへの混入を見送っていた）。ここでは判定の前に、
 * `overflow-y: auto/scroll` かつ実際にスクロールできる（`scrollHeight > clientHeight`）祖先をすべて
 * 一時的に末尾までスクロールし、判定が終わったら元の位置に戻す（スクリーンショットへの影響を避ける）。
 * ルールそのもの（「固定要素より下に来てはいけない」）は緩めていない。
 */
function checkFixedOverlays() {
  const violations = [];
  const height = window.__MOBILE_AUDIT_HEIGHT__;
  window.scrollTo(0, document.body.scrollHeight);
  const innerScrollers = [];
  for (const el of document.querySelectorAll("body *")) {
    const style = getComputedStyle(el);
    if (style.overflowY !== "auto" && style.overflowY !== "scroll") continue;
    if (el.scrollHeight <= el.clientHeight) continue;
    innerScrollers.push({ el, prevTop: el.scrollTop });
    el.scrollTop = el.scrollHeight;
  }
  function restoreInnerScrollers() {
    for (const { el, prevTop } of innerScrollers) el.scrollTop = prevTop;
  }
  const fixed = [];
  for (const el of document.querySelectorAll("body *")) {
    const style = getComputedStyle(el);
    if (style.position !== "fixed") continue;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    fixed.push({ el, rect });
  }
  const bottomBars = fixed.filter((f) => f.rect.top > height / 2);
  if (bottomBars.length === 0) {
    restoreInnerScrollers();
    return violations;
  }
  const minTop = Math.min(...bottomBars.map((f) => f.rect.top));
  let maxContentBottom = 0;
  let worst = null;
  for (const el of document.querySelectorAll("body *")) {
    if (fixed.some((f) => f.el === el || f.el.contains(el))) continue;
    const text = (el.textContent ?? "").trim();
    if (el.children.length > 0 && text.length > 0) continue; // 末端だけ見る（親の重複を避ける）
    if (text.length === 0) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    const rect = el.getBoundingClientRect();
    if (rect.bottom > maxContentBottom) {
      maxContentBottom = rect.bottom;
      worst = el;
    }
  }
  if (worst && maxContentBottom > minTop + 1) {
    violations.push({
      rule: "fixed-overlay",
      selector: cssPathRef(worst),
      box: { contentBottom: maxContentBottom, overlayTop: minTop },
      detail: `content bottom=${maxContentBottom.toFixed(1)} is below the fixed bar top=${minTop.toFixed(1)} even after scrolling to the end`,
    });
  }
  restoreInnerScrollers();
  return violations;
}

/** D1-6: 横スクロールが要る表は overflow-x-auto の箱に入っている。 */
function checkTables() {
  const violations = [];
  for (const table of document.querySelectorAll("table")) {
    let node = table.parentElement;
    let wrapped = false;
    while (node) {
      const style = getComputedStyle(node);
      if (style.overflowX === "auto" || style.overflowX === "scroll") {
        wrapped = true;
        break;
      }
      node = node.parentElement;
    }
    if (!wrapped) {
      violations.push({
        rule: "table-wrap",
        selector: cssPathRef(table),
        box: {},
        detail: "no overflow-x-auto ancestor",
      });
    }
  }
  return violations;
}

function runChecks() {
  return [
    ...checkOverflow(),
    ...checkTapTargets(),
    ...checkStatusBadges(),
    ...checkFontSize(),
    ...checkFixedOverlays(),
    ...checkTables(),
  ];
}

async function main() {
  const skipBuild = process.env.MOBILE_AUDIT_SKIP_BUILD === "1";
  if (!skipBuild) {
    const build = spawnSync("pnpm", ["build"], { cwd: GUI_DIR, stdio: "inherit" });
    if (build.status !== 0) {
      console.error("mobile-audit: pnpm build failed");
      process.exit(build.status ?? 1);
    }
  }

  const mock = await setupMockCeleris();
  const guiPort = await getFreePort();
  const guiBind = `127.0.0.1:${guiPort}`;
  const guiLog = fs.openSync(path.join(OUT_DIR, "gui.log"), "w");
  const gui = spawn(process.execPath, ["server.js"], {
    cwd: GUI_DIR,
    env: { ...process.env, NODE_ENV: "production", CELERIS_GUI_BIND: guiBind, CELERIS_API_URL: mock.baseUrl },
    stdio: ["ignore", guiLog, guiLog],
  });

  const allViolations = [];
  const routeReports = [];
  let browser;
  try {
    await waitForHealth(`http://${guiBind}/healthz`);
    browser = await chromium.launch({ headless: true });
    const context = await browser.newContext({
      viewport: VIEWPORT,
      deviceScaleFactor: DEVICE_SCALE_FACTOR,
      userAgent: USER_AGENT,
      isMobile: true,
      hasTouch: true,
    });
    // celeris へは fetch/EventSource で直接出ない構成だが、念のため GUI 以外への要求は塞ぐ（外部ネットワーク不使用）。
    await context.route("**/*", (route) => {
      const url = new URL(route.request().url());
      return url.hostname === "127.0.0.1" && url.port === String(guiPort) ? route.continue() : route.abort();
    });
    // 検査本体はブラウザ内で走る必要があるので、各関数の `toString()` を組み立てて `addInitScript` で
    // 毎ページに注入する（`page.evaluate(fn)` は `fn` 単体しか送れず、参照している他の関数までは
    // 持って行けないため）。
    const auditSource = [
      `window.__MOBILE_AUDIT_WIDTH__ = ${VIEWPORT.width};`,
      `window.__MOBILE_AUDIT_HEIGHT__ = ${VIEWPORT.height};`,
      cssPathRef.toString(),
      isNotVisible.toString(),
      checkOverflow.toString(),
      checkTapTargets.toString(),
      checkStatusBadges.toString(),
      checkFontSize.toString(),
      checkFixedOverlays.toString(),
      checkTables.toString(),
      runChecks.toString(),
      "window.__runMobileAudit = runChecks;",
    ].join("\n");
    await context.addInitScript(auditSource);

    for (const { route, path: routePath } of ROUTES) {
      const page = await context.newPage();
      const pageErrors = [];
      page.on("pageerror", (err) => pageErrors.push(err.message));
      const response = await page.goto(`http://${guiBind}${routePath}`, { waitUntil: "load" });
      const status = response?.status() ?? 0;
      if (status !== 200) {
        allViolations.push({
          route,
          rule: "http-status",
          selector: routePath,
          box: {},
          detail: `GET ${routePath} -> ${status}`,
        });
      } else {
        const violations = await page.evaluate(() => window.__runMobileAudit());
        for (const v of violations) allViolations.push({ route, ...v });
      }
      if (pageErrors.length > 0) {
        allViolations.push({ route, rule: "page-error", selector: routePath, box: {}, detail: pageErrors.join(" / ") });
      }
      const shotPath = path.join(OUT_DIR, `${route}.png`);
      await page.screenshot({ path: shotPath, fullPage: true }).catch(() => {});
      routeReports.push({ route, path: routePath, status });
      await page.close();
    }
  } finally {
    await browser?.close();
    gui.kill("SIGTERM");
    await mock.close();
  }

  fs.writeFileSync(
    REPORT_PATH,
    JSON.stringify(
      { generated_at: new Date().toISOString(), routes: routeReports, violations: allViolations },
      null,
      2,
    ),
  );

  const byRule = {};
  for (const v of allViolations) byRule[v.rule] = (byRule[v.rule] ?? 0) + 1;
  // `gui/biome.json` は `console.log` を禁止している（`error`/`warn` だけ許可）ので `console.error` で出す。
  console.error(
    JSON.stringify(
      { ok: allViolations.length === 0, total: allViolations.length, by_rule: byRule, report: REPORT_PATH },
      null,
      2,
    ),
  );
  process.exit(allViolations.length === 0 ? 0 : 1);
}

await main();
