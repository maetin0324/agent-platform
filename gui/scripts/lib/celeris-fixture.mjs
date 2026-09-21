// gui/scripts/lib/celeris-fixture.mjs — mobile-audit.mjs と e2e-check.mjs（Phase 83 / G36）が共有する
// 「偽の celeris」（node:http、`test/mock-celeris/fixtures.ts` の値を使う）と、監査対象の画面一覧。
//
// **`test/mock-celeris/server.ts` の `startMockCeleris` は使わない**（mobile-audit.mjs のコメントと同じ理由:
// `./fixtures`（拡張子なし）の相対 import は vite/vitest の TS 解決の下でしか通らない。`fixtures.ts` 自身は
// celeris の型を type-only import しているだけなので、そちらは素の Node からそのまま import できる）。
//
// ここを変えると mobile-audit.mjs と e2e-check.mjs の両方に効く。1 か所にまとめたのは、同じ画面一覧・同じ
// 偽データを 2 度書くと片方だけ更新し忘れる事故が起きるため（Phase 83 で e2e-check.mjs を足すときに発見）。
import http from "node:http";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

const fx = await import(path.join(GUI_DIR, "test/mock-celeris/fixtures.ts"));

export const TASK_ID = "01BOARDTASK00000000000001";
export const PROJECT_ID = "p1";
/** `/org/<id>` の葉ノード（mount 先など）。 */
export const ORG_NODE_ID = "coding-poc";
/** `/org?selected=<id>` の部門長ノード（継続セッション表示。`ORG_NODE_ID` の親で id が違う）。 */
export const ORG_HEAD_ID = "coding";
export const SKILL_NAME = "rust-review";

/**
 * ADR-0055 D1 が挙げた画面（mobile-audit）。Phase 83 は同じ一覧を e2e-check.mjs のナビゲーション検査にも使うが、
 * staging（e2e:staging）は偽の celeris ではなく本番 DB のスナップショットに対して開くので、`taskId` /
 * `projectId` / `orgId` / `orgHeadId` / `skillName` を差し替えられるようにした（`e2e-check.mjs` が snapshot
 * から実在する id を読んで渡す。見つからなければ `undefined` のままにして、その id が要る画面はスキップする）。
 * 引数を省略すると mobile-audit の既定値（このモジュールの固定 fixture の id）になる。
 */
export function buildRoutes({
  taskId = TASK_ID,
  projectId = PROJECT_ID,
  orgId = ORG_NODE_ID,
  orgHeadId = ORG_HEAD_ID,
  skillName = SKILL_NAME,
} = {}) {
  const routes = [
    { route: "home", path: "/" },
    { route: "org", path: "/org" },
    { route: "projects", path: "/projects" },
    { route: "board", path: "/board" },
    { route: "approvals", path: "/approvals" },
    { route: "reports", path: "/reports" },
    { route: "releases", path: "/releases" },
    { route: "knowledge", path: "/knowledge" },
    { route: "knowledge-inbox", path: "/knowledge/inbox" },
    { route: "knowledge-skills", path: "/knowledge/skills" },
    { route: "clusters", path: "/clusters" },
    { route: "accounts", path: "/accounts" },
    { route: "help", path: "/help" },
  ];
  if (orgId) {
    routes.push({ route: "org-node", path: `/org/${orgId}` });
  }
  if (orgHeadId) {
    routes.push({ route: "org-detail", path: `/org?selected=${orgHeadId}` });
  }
  if (projectId) {
    routes.push(
      { route: "project-detail", path: `/projects/${projectId}` },
      { route: "project-docs", path: `/projects/${projectId}/docs` },
    );
  }
  if (skillName) {
    routes.push({ route: "knowledge-skill-detail", path: `/knowledge/skills?name=${skillName}` });
  }
  if (taskId) {
    for (const tab of ["overview", "timeline", "changes", "files", "artifacts"]) {
      routes.push({ route: `task-${tab}`, path: `/tasks/${taskId}?tab=${tab}` });
    }
  }
  return routes;
}

/** mobile-audit.mjs はこれまでどおり固定の fixture id（`TASK_ID` / `PROJECT_ID` / `ORG_NODE_ID`）を使う。 */
export const ROUTES = buildRoutes();

/** @returns {Promise<number>} */
export function getFreePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const address = /** @type {import("node:net").AddressInfo} */ (srv.address());
      srv.close(() => resolve(address.port));
    });
  });
}

/**
 * @param {string} url
 * @param {number} [timeoutMs]
 */
export async function waitForHealth(url, timeoutMs = 20_000) {
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

/** @typedef {(req: import("node:http").IncomingMessage, res: import("node:http").ServerResponse) => void} RouteHandler */

/** @param {import("node:http").ServerResponse} res */
function sendSse(res) {
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store" });
  res.write(": celeris-fixture\n\n");
  // 閉じない（実際の SSE と同じ。Playwright/chromium は networkidle を待たないのでハングしない）。
}

/**
 * @param {import("node:http").ServerResponse} res
 * @param {number} status
 * @param {unknown} body
 */
function sendJson(res, status, body) {
  res.writeHead(status, { "content-type": "application/json; charset=utf-8", "cache-control": "no-store" });
  res.end(JSON.stringify(body));
}

/**
 * 素の celeris もどき（`node:http`、`gui/scripts/mobile-audit.mjs` と同じ作り）。`on` は
 * 完全一致のパス（クエリ抜き）だけを見る。GET しか要らない（監査・e2e は読み取りだけ）。
 */
function createFakeCeleris() {
  /** @type {Map<string, RouteHandler>} */
  const routes = new Map();
  /** @type {(method: string, routePath: string, handler: RouteHandler) => void} */
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
    /** @returns {Promise<string>} */
    listen: () =>
      new Promise((resolve, reject) => {
        server.once("error", reject);
        server.listen(0, "127.0.0.1", () => {
          const address = /** @type {import("node:net").AddressInfo} */ (server.address());
          resolve(`http://127.0.0.1:${address.port}`);
        });
      }),
    /** @returns {Promise<void>} */
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

export async function setupMockCeleris() {
  const fake = createFakeCeleris();
  const baseUrl = await fake.listen();
  const mock = { on: fake.on, baseUrl, close: fake.close };

  mock.on("GET", "/api/v1/health", (_req, res) => sendJson(res, 200, fx.defaultHealth));
  mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [fx.project({ id: PROJECT_ID })] }));
  mock.on("GET", "/api/v1/tasks", (_req, res) =>
    sendJson(res, 200, {
      items: [
        fx.taskSummary({ assignee: "coding-poc", project_id: PROJECT_ID }),
        fx.taskSummary({
          id: "01BOARDTASK00000000000002",
          assignee: "coding-poc",
          project_id: PROJECT_ID,
          status: "ready",
          title:
            "長い題名のタスク: 関連研究のサーベイと実装方針の検討および " +
            "VeryLongUnbrokenIdentifierWithoutSpacesThatCouldOverflowTheCard-01BOARDTASK00000000000002",
          labels: ["survey", "impl"],
        }),
      ],
      total: 2,
      counts_by_status: { ready: 2 },
      next_cursor: null,
    }),
  );
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/timeline`, (_req, res) =>
    sendJson(res, 200, fx.timeline([...fx.timeline([], TASK_ID).items, ...fx.timelineWorkerProgressItems()], TASK_ID)),
  );
  mock.on("GET", `/api/v1/tasks/${TASK_ID}/comments`, (_req, res) =>
    sendJson(res, 200, { items: [fx.taskComment({ task_id: TASK_ID })] }),
  );

  mock.on("GET", "/api/v1/knowledge/tree", (_req, res) => sendJson(res, 200, fx.knowledgeTree()));
  mock.on("GET", "/api/v1/knowledge/page", (_req, res) => sendJson(res, 200, fx.knowledgePage()));
  mock.on("GET", "/api/v1/knowledge/inbox", (_req, res) => sendJson(res, 200, fx.knowledgeInbox()));

  mock.on("GET", "/api/v1/skills", (_req, res) => sendJson(res, 200, fx.skillList()));
  mock.on("GET", "/api/v1/skills/rust-review", (_req, res) => sendJson(res, 200, fx.skillDetail()));

  mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, fx.orgList()));

  mock.on("GET", "/api/v1/config", (_req, res) =>
    sendJson(res, 200, {
      api: { allowed_hosts: [], auth_required: false, bind: "127.0.0.1:0" },
      config_path: "/tmp/celeris-fixture/config.toml",
      db: "/tmp/celeris-fixture/celeris.sqlite3",
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
      workspace_root: "/tmp/celeris-fixture/workspace",
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

  mock.on("GET", "/api/v1/mcp/clients", (_req, res) =>
    sendJson(res, 200, {
      items: [
        {
          id: "chatgpt",
          name: "chatgpt",
          created_at: "2026-09-19T00:00:00Z",
          last_used_at: "2026-09-20T23:50:00Z",
          scopes: ["knowledge:read", "knowledge:propose", "tasks:read", "console:instruct"],
          token_hash: "a".repeat(64),
        },
        {
          id: "old-claude-code",
          name: "old-claude-code",
          created_at: "2026-08-01T00:00:00Z",
          revoked_at: "2026-09-10T00:00:00Z",
          scopes: ["org:read", "skills:read"],
          token_hash: null,
        },
      ],
    }),
  );
  mock.on("GET", "/api/v1/mcp/calls", (_req, res) =>
    sendJson(res, 200, {
      items: [
        {
          id: "call2",
          client_id: "chatgpt",
          tool: "knowledge_propose",
          ok: true,
          latency_ms: 120,
          at: "2026-09-20T23:50:00Z",
        },
        {
          id: "call1",
          client_id: "chatgpt",
          tool: "console_instruct",
          ok: false,
          error_kind: "rate_limited",
          latency_ms: 8,
          at: "2026-09-20T20:00:00Z",
        },
      ],
    }),
  );

  mock.on("GET", "/api/v1/daemon", (_req, res) => sendJson(res, 200, { now: "2026-09-21T00:00:00Z", snapshot: null }));
  mock.on("GET", "/api/v1/inbox", (_req, res) =>
    sendJson(res, 200, { approvals: 0, attention: 0, by_status: {}, drafts: 0, questions: 0 }),
  );

  const mcpHumanBlock = {
    kind: "human",
    at: "2026-09-20T00:59:50Z",
    cursor: "00001789000000000000.0.m01MCP",
    message_id: "01MCPMSG0000000000000001",
    node_id: "cos",
    text: "ChatGPT の Deep Research からの種: この方向で調べて",
    author: "mcp:chatgpt",
  };
  mock.on("GET", "/api/v1/console", (_req, res) =>
    sendJson(
      res,
      200,
      fx.consolePage({ items: [mcpHumanBlock, ...fx.consoleBlocks(), fx.consoleGrowingReplySnapshot()] }),
    ),
  );
  mock.on("GET", "/api/v1/stream", (_req, res) => sendSse(res));
  mock.on("GET", "/api/v1/console/stream", (_req, res) => sendSse(res));

  return mock;
}
