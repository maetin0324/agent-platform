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
import { createRequire } from "node:module";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

const fx = await import(path.join(GUI_DIR, "test/mock-celeris/fixtures.ts"));

// Phase 91（ADR-0055 ラウンド 15、D1 の「本物のモバイル・エミュレーション」）: mobile-audit.mjs と
// e2e-check.mjs の両方が、モバイルの Playwright コンテキストをここから作る（1 か所にまとめる理由は
// このファイルの他のものと同じ: 2 か所に同じ寸法・UA を書くと片方だけ更新し忘れる事故が起きるため）。
// Playwright の組み込みデバイス記述子 `devices["Pixel 7"]`（`isMobile`/`hasTouch`/`defaultBrowserType`
// 等、実機の Chrome-on-Android に近い挙動一式が揃っている）を土台にし、ADR-0055 D1 が指定する寸法
// （393×851）・`deviceScaleFactor`（2.75）・UA（Nothing Phone 2a）だけを上書きする。以前（Phase 69〜90）は
// これらの値を手で列挙していた（`isMobile: true`/`hasTouch: true` 自体は Phase 69 の最初のコミットから
// 既に指定していたので機能的な差は無いが、`devices[...]` を土台にすることで、Pixel 7 記述子が将来
// 増やすかもしれない他のモバイル固有のコンテキストオプションも自動的に追随する）。
const require = createRequire(path.join(GUI_DIR, "package.json"));
const { devices } = require("@playwright/test");

export const MOBILE_DEVICE = {
  ...devices["Pixel 7"],
  viewport: { width: 393, height: 851 },
  deviceScaleFactor: 2.75,
  userAgent:
    "Mozilla/5.0 (Linux; Android 14; Nothing Phone 2a) AppleWebKit/537.36 (KHTML, like Gecko) " +
    "Chrome/128.0.0.0 Mobile Safari/537.36",
};

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
    // Phase 87（P-G38-3）: `/inbox`（裏方の受信箱）を ADR-0055 D1 の機械検査対象に加えた（Phase 86 の
    // 提案 P-G38-3 を受けての人の指示。`docs/PROGRESS.md` Phase 87 参照）。
    { route: "inbox", path: "/inbox" },
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
  // Phase 84: 作成・編集フォーム（`SkillEditor`）自体も監査対象にする（GET だけなので e2e:staging でも安全。
  // 一覧・詳細の閲覧だけでは、files 入力・雛形ボタン・インラインの検証エラー表示の画面が一度も機械検査を
  // 通らないまま「監査 0 件」を名乗ってしまう事故を防ぐ）。
  routes.push({ route: "knowledge-skill-create", path: "/knowledge/skills?create=1" });
  if (skillName) {
    routes.push(
      { route: "knowledge-skill-detail", path: `/knowledge/skills?name=${skillName}` },
      { route: "knowledge-skill-edit", path: `/knowledge/skills?name=${skillName}&edit=1` },
    );
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
  /** 検査用: 指定パス接頭辞の API を 503（problem+json）にする。null で解除。 */
  /** @type {string | null} */
  let failPrefix = null;
  const server = http.createServer((req, res) => {
    const pathname = new URL(req.url ?? "/", "http://fake-celeris.invalid").pathname;
    if (failPrefix && pathname.startsWith(failPrefix)) {
      res.writeHead(503, { "content-type": "application/problem+json" });
      res.end(
        JSON.stringify({ code: "unavailable", title: "Service Unavailable", status: 503, detail: "temporarily down" }),
      );
      return;
    }
    const handler = routes.get(`${req.method ?? "GET"} ${pathname}`);
    if (!handler) {
      sendJson(res, 404, { code: "not_found", detail: `no route for ${req.method} ${pathname}` });
      return;
    }
    handler(req, res);
  });
  return {
    on,
    /** @param {string | null} prefix */
    failWith503: (prefix) => {
      failPrefix = prefix;
    },
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
  const mock = { on: fake.on, failWith503: fake.failWith503, baseUrl, close: fake.close };

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
          // ADR-0059 D6（Phase 99/100）: 作業ディレクトリが未登録のクラスタ（work_dir/work_dir_source とも
          // null）。mobile-audit / e2e:mock がこの状態の「未登録」案内を検査対象にする。
          work_dir: null,
          work_dir_source: null,
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
          // ADR-0059 D6（Phase 99/100）: 設定ファイル（`[[clusters]] work_dir`）由来の実効値。
          work_dir: "/work/NBB/rmaeda",
          work_dir_source: "config",
          // Phase 85（ADR-0053 追記）の実機と同じ形: listener はあるが target が応答しない
          // （"unreachable" の 1 語バッジ + 理由の文を mobile-audit / e2e:mock に描画させる）。
          tunnel_forwards: [
            {
              listen: "127.0.0.1:18000",
              target: "bnode150:18000",
              up: false,
              listener: true,
              target_healthy: false,
              last_error: "target 127.0.0.1:18000 did not answer /v1/models within 2s",
            },
          ],
        },
        // Phase 86（ADR-0055 ラウンド 11）: tunnel_login_needed を伴わない、ただの切断
        // （"down" の 1 語バッジ）も監査対象にする。
        // Phase 87（P-G38-2）: `auth: "manual"` にしていた回避（"pegasus" と同じ `auth != "manual"` にすると
        // `focus-order` の検査が「2 つの接続フォームの CSS パス署名が衝突した」と誤検知していた）を元に戻した。
        // 誤検知の原因は `mobile-audit.mjs::cssPathRef` が要素識別に `node.id`（IDL 属性。named-form-control の
        // shadowing で `<input type="hidden" name="id">` を持つ `<form>` では文字列ではなく要素自身を返す）を
        // 使っていたことで、`node.getAttribute("id")` に直したことで直った（このファイルの `auth: "publickey"`
        // が、構造が同じ 2 つの接続フォーム（"pegasus" と "gpu2"）を意図的に並べる回帰検査の役目を果たす）。
        {
          id: "gpu2",
          host: "gpu2.internal",
          concurrency: 1,
          delete_on_push: false,
          env_keys: [],
          has_setup: false,
          rsync_excludes: [],
          sync: "rsync",
          auth: "publickey",
          connected: false,
          tunnel_login_needed: false,
          // ADR-0059 D6（Phase 99/100）: 画面から登録した DB の上書き（「上書きを消す」ボタンの
          // 監査対象。gpu1=未登録・pegasus=config の 2 通りと合わせ、settings/config/null の 3 通りが揃う）。
          work_dir: "/work/NBB/rmaeda-gui",
          work_dir_source: "settings",
        },
      ],
    }),
  );

  // ADR-0059 D6（Phase 100）: `PUT /clusters/{id}/settings`（§3.107）。この偽 celeris はパスの完全一致で
  // ルーティングするので（`createFakeCeleris` の `on`）、動的な `:id` は持てず、フィクスチャが知っている
  // 3 つの id ごとに登録する。バリデーションは celeris 本体（`crates/task-api/src/handlers.rs::put_cluster_settings`）
  // と同じ規則（絶対パスか `~`/`~/…`。空文字・それ以外は 422）にして、e2e:mock / mobile-audit が本番と
  // 同じ挙動を確認できるようにする。
  for (const clusterId of ["gpu1", "pegasus", "gpu2"]) {
    mock.on("PUT", `/api/v1/clusters/${clusterId}/settings`, (req, res) => {
      let raw = "";
      req.on("data", (chunk) => {
        raw += chunk;
      });
      req.on("end", () => {
        /** @type {{work_dir?: string | null}} */
        let body;
        try {
          body = raw ? JSON.parse(raw) : {};
        } catch {
          sendJson(res, 400, { code: "bad_request", detail: "invalid JSON" });
          return;
        }
        const workDir = body.work_dir ?? null;
        if (workDir !== null) {
          const trimmed = String(workDir).trim();
          const ok = trimmed !== "" && (trimmed.startsWith("/") || trimmed === "~" || trimmed.startsWith("~/"));
          if (!ok) {
            sendJson(res, 422, {
              type: "urn:celeris:problem:validation",
              title: "validation",
              status: 422,
              code: "validation",
              detail: "work_dir must be an absolute path or ~ / ~/…",
              errors: [{ field: "work_dir", message: "work_dir must be an absolute path or ~ / ~/…" }],
            });
            return;
          }
        }
        sendJson(res, 200, {
          cluster_id: clusterId,
          work_dir: workDir,
          updated_at: "2026-09-22T12:00:00Z",
        });
      });
    });
  }

  mock.on("GET", "/api/v1/llm/sources", (_req, res) =>
    sendJson(res, 200, {
      sources: [
        {
          id: "claude-oauth",
          kind: "claude-oauth",
          enabled: true,
          // Phase 86（ADR-0055 ラウンド 11）: 2 件とも cooldown 中にして、絶対 title 付きの相対時間の
          // バッジと「Claude のアカウントが全て cooldown 中です」の警告カードの両方を監査対象にする
          // （celeris/<tier> の「why」が cooldown になる経路もこれで確かめられる）。
          accounts: [
            {
              id: "claude-a",
              logged_in: true,
              remaining: 0.1,
              remaining_short: 0.1,
              remaining_long: 0.3,
              cooldown_until: 1_893_456_000, // 2030-01-01T00:00:00Z 相当（常に未来）
              cooldown_reason: "exhausted",
            },
            {
              id: "claude-b",
              logged_in: true,
              remaining: 0.05,
              remaining_short: 0.05,
              remaining_long: 0.2,
              cooldown_until: 1_893_456_000,
              cooldown_reason: "throttled",
            },
          ],
          last_hour_requests: 12,
          last_hour_prompt_tokens: 3400,
          last_hour_completion_tokens: 900,
        },
        {
          // Phase 86（ADR-0055 ラウンド 11）: reachable にして、`cheap` の解決先にする（"free-first"
          // の理由を監査対象にする）。frontier/standard は claude-oauth に解決したままにし（無料源は
          // 別 tier で使われている、という設定）、その claude-oauth のアカウントは 2 件とも cooldown 中
          // なので、こちらは「cooldown」の理由を監査対象にする。
          id: "openai-compatible:qwen",
          kind: "openai-compatible",
          enabled: true,
          reachable: true,
          accounts: [],
          last_hour_requests: 40,
          last_hour_prompt_tokens: 9000,
          last_hour_completion_tokens: 5000,
        },
      ],
      celeris_tiers: [
        { tier: "frontier", resolves_to: "claude-oauth" },
        { tier: "standard", resolves_to: "claude-oauth" },
        { tier: "cheap", resolves_to: "openai-compatible:qwen" },
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
  // Phase 87（P-G38-3）: 以前はここが `Inbox`（`docs/celeris-api-v1.md` §3.2）の形と違う平らなオブジェクトを
  // 返していた（`counts` が無く、`approvals`/`questions`/`drafts`/`attention` は配列ではなく数だった）。
  // `/inbox` が ADR-0055 D1 の監査対象に無かったため気づかれずに残っていた（`app/routes/inbox.tsx` の
  // `isEmpty` が `inbox.counts.approvals` を読むので、実際にこの画面を開くと `inbox.counts` が `undefined`
  // になり例外で落ちていたはずのバグ）。承認待ち・質問を 1 件ずつ持つ、型どおりの `Inbox` にした
  // （受け入れ条件「カードが描画される状態を fixture に作る」）。
  //
  // Phase 88（P-G39-1）: Phase 87 の未解決事項（`draft-group`・`attention-item`・`approval-parent-title`・
  // `question-approval-link` が fixture に無く、機械検査を通っていなかった）を解消するため、承認 1 件に
  // 親タスクを付け、質問 1 件に `approval_id` を付け、draft グループ 1 件（draft タスク 2 件）と
  // attention 1 件（`failed`）を足した。`counts` はそれぞれの配列の実件数と一致させる
  // （`docs/celeris-api-v1.md` §5.1: `drafts` は draft タスクの件数でグループ数ではない）。
  mock.on("GET", "/api/v1/inbox", (_req, res) =>
    sendJson(res, 200, {
      approvals: [
        {
          approval: {
            actions: ["approve", "reject"],
            id: "01INBOXAPPROVAL000000001",
            kind: "approval",
            status: "reviewing",
            title: "本番のクラスタに接続してよいですか",
          },
          artifacts: [],
          criterion_idx: 0,
          criterion_text: "pegasus への接続を許可する",
          evidence: [],
          last_run: null,
          other_verdicts: [],
          // Phase 88（P-G39-1）: `approval-parent-title` を機械検査対象にする（承認待ちの元になった親タスク）。
          parent: {
            actions: [],
            id: "01INBOXAPPROVALPARENT0001",
            kind: "execute",
            status: "running",
            title: "本番運用への移行 Plan",
          },
          previous_decisions: [],
          requested_at: "2026-09-20T23:40:00Z",
        },
      ],
      // Phase 88（P-G39-1）: `attention-item`（`AttentionRow`。`cluster_unavailable` 以外の分岐）を
      // 機械検査対象にする。
      attention: [
        {
          at: "2026-09-20T22:00:00Z",
          reason: "run failed: exit 1",
          task: {
            actions: ["retry", "cancel"],
            id: "01INBOXATTENTIONTASK00001",
            kind: "execute",
            status: "failed",
            title: "依存パッケージの更新確認",
          },
          type: "failed",
        },
      ],
      counts: { approvals: 1, attention: 1, by_status: {}, drafts: 2, questions: 1 },
      // Phase 88（P-G39-1）: `draft-group`（`DraftGroupRow`、`draft-item` 2 件）を機械検査対象にする。
      drafts: [
        {
          parent: {
            actions: [],
            id: "01INBOXDRAFTPARENT0000001",
            kind: "plan",
            status: "ready",
            title: "関連研究サーベイの Plan",
          },
          plan_summary: "3 本の論文を読んで比較する",
          drafts: [
            {
              actions: ["cancel"],
              adapter: null,
              assignee: null,
              attempts: 0,
              backoff_until: null,
              category: "research",
              children: 0,
              conversation: false,
              created_at: "2026-09-20T21:00:00Z",
              depends_on: [],
              genre: null,
              id: "01INBOXDRAFTTASK000000001",
              kind: "execute",
              labels: [],
              lease_expires_at: null,
              max_retries: 2,
              milestone_id: null,
              parent_id: "01INBOXDRAFTPARENT0000001",
              pending_children: 0,
              priority: 10,
              priority_label: "P2",
              project_id: null,
              role: null,
              status: "draft",
              support: null,
              tier: "standard",
              title: "論文 A を読む",
              updated_at: "2026-09-20T21:00:01Z",
            },
            {
              actions: ["cancel"],
              adapter: null,
              assignee: null,
              attempts: 0,
              backoff_until: null,
              category: "research",
              children: 0,
              conversation: false,
              created_at: "2026-09-20T21:05:00Z",
              depends_on: [],
              genre: null,
              id: "01INBOXDRAFTTASK000000002",
              kind: "execute",
              labels: [],
              lease_expires_at: null,
              max_retries: 2,
              milestone_id: null,
              parent_id: "01INBOXDRAFTPARENT0000001",
              pending_children: 0,
              priority: 10,
              priority_label: "P2",
              project_id: null,
              role: null,
              status: "draft",
              support: null,
              tier: "standard",
              title: "論文 B を読む",
              updated_at: "2026-09-20T21:05:01Z",
            },
          ],
        },
      ],
      questions: [
        {
          // Phase 88（P-G39-1）: `approval_id` を付け、`question-approval-link`（「認可」の画面で答える）を
          // 機械検査対象にする（`approval_id` が無い分岐は Phase 87 から既に監査対象）。
          approval_id: "01INBOXAPPROVAL000000002",
          asked_at: "2026-09-20T23:30:00Z",
          previous: [],
          question: "この案件のスコープに含めてよいですか",
          run_id: null,
          task: {
            actions: ["answer"],
            id: "01INBOXQUESTION00000001",
            kind: "execute",
            status: "blocked",
            title: "関連研究のサーベイ範囲を決める",
          },
        },
      ],
    }),
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

  // Phase 102（本番でホームからの送信が 405 になった回帰の修正）: e2e-check.mjs が実際に composer から
  // 送信して「エラー画面が出ないこと」を確かめられるように、`POST /console/instruct`（§3.107、
  // **管理系**、202 `ConsoleInstructAccepted`）を足す。本文は読み捨て、固定の応答を返すだけでよい
  // （このフィクスチャは読み取りだけを前提にしてきたが、この 1 本だけは書き込み検査に要る）。
  mock.on("POST", "/api/v1/console/instruct", (req, res) => {
    req.on("data", () => {});
    req.on("end", () => {
      sendJson(res, 202, {
        message_id: "01E2ECHECKMSG00000000001",
        node_id: "cos",
        task_id: "01E2ECHECKTASK0000000001",
      });
    });
  });

  return mock;
}
