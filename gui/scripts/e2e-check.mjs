// gui/scripts/e2e-check.mjs — Phase 83 / G36（ADR-0041 verify.sh の検査 4b、ADR-0055 D3「GUI の e2e には
// 実 celeris が要る」というギャップの解消）。
//
// `gui/e2e/*.spec.ts`（`pnpm e2e`）は `scripts/celeris.sh` で使い捨ての celeris を起こし、タスクを作って
// 承認して……と**書き込みを伴う**結合テストなので、そのまま `verify.sh`（staging = 本番 DB のスナップショット）
// には使えない（ADR-0041 の staging は「検査 6 の煙試験だけが書き込む」という前提で件数一致検査（検査 2/5）を
// 組んでいる）。この検査 4b は別物: **読み取りだけ**（ナビゲーションと `?tab=` の切り替えのみ。POST は一切しない）
// で、次の 2 通りに対して同じチェックを走らせる:
//
//   - `pnpm e2e:mock`（オフライン、外部ネットワーク不使用）: `scripts/lib/celeris-fixture.mjs` の偽の celeris
//     （`test/mock-celeris/fixtures.ts` の値）と `pnpm build` 済みの GUI を自分で起こす。CI・オフラインでの
//     動作確認用（このコマンドが 0 で終わることが Phase 83 の受け入れ条件の 1 つ）。
//   - `pnpm e2e:staging`（`verify.sh` から呼ぶ）: 既に起きている staging の GUI / celeris（環境変数
//     `E2E_GUI_URL` / `E2E_API_URL` / `E2E_TOKEN_FILE`）に対して**何も起動せず**接続するだけ。存在するタスク・
//     プロジェクト・組織ノード・skill を `E2E_API_URL` から Node 側で読んで（ブラウザは直接 celeris を叩かない。
//     gui/CLAUDE.md の境界どおり）、`/tasks/<id>` のタブ切り替えなど「実データでの見た目」を確かめる。
//     見つからなければ id が要る画面はスキップする（タスクを作らない。ADR-0041 の検査 6 煙試験より**前**に
//     `verify.sh` の 4b として置くので、e2e の時点ではスナップショットに元からある行しか無い）。
//
// 検査する内容（Phase 83 の受け入れ条件）:
//   1. mobile-audit と同じ画面一覧（`buildRoutes`）を 393×851 と 1280×800 の両方で開き、200 で応答し、
//      コンソールエラー・失敗した要求（401 は許容）が無いこと。
//   2. `/`（Console）が `[data-testid="console-screen"]` を描画すること（ブロック数は参考情報として出す）。
//   3. `/accounts` が「LLM source」「MCP クライアント」節（`llm-sources-section` / `mcp-clients-section`）を
//      描画すること。
//   4. `/knowledge/skills` が描画すること（`knowledge-skills`）。
//   5. 実在するタスク（`/tasks/<id>`）でタブ（概要・タイムライン・変更・ファイル・成果物）を切り替え、
//      対応する節（`info-section` 等）が出ること。`?tab=` を差し替える `<Link>` のクリックだけで、POST は無い。
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  buildRoutes,
  getFreePort,
  ORG_HEAD_ID,
  ORG_NODE_ID,
  PROJECT_ID,
  SKILL_NAME,
  setupMockCeleris,
  TASK_ID,
  waitForHealth,
} from "./lib/celeris-fixture.mjs";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = path.join(GUI_DIR, "test/e2e-check");

const VIEWPORTS = [
  { name: "mobile", width: 393, height: 851 },
  { name: "desktop", width: 1280, height: 800 },
];

// タブ → そのタブで出るはずの節の data-testid（`app/routes/tasks.$id.tsx` の `TASK_TABS` と同じ 5 つ）。
const TASK_TAB_SECTIONS = {
  overview: "info-section",
  timeline: "timeline-section",
  changes: "changes-section",
  files: "files-section",
  artifacts: "artifacts-section",
};

fs.mkdirSync(OUT_DIR, { recursive: true });
for (const name of fs.readdirSync(OUT_DIR)) fs.rmSync(path.join(OUT_DIR, name), { force: true, recursive: true });

/** @param {string} [file] */
function readTrimmed(file) {
  if (!file) return "";
  try {
    return fs.readFileSync(file, "utf8").trim();
  } catch {
    return "";
  }
}

/**
 * 応答の形は `docs/celeris-api-v1.md` の各エンドポイントで決まっているが、ここは実在の id を 1 つ拾うだけの
 * 使い捨ての読み取りなので `app/celeris/types.ts` の型は引かず `any` のまま扱う（gui/CLAUDE.md が禁じる
 * のは「文書に無い挙動に頼る」ことで、ここでは無い）。
 * @param {string} base
 * @param {string} token
 * @param {string} pathAndQuery
 * @returns {Promise<any>}
 */
async function apiFetch(base, token, pathAndQuery) {
  const headers = token ? { Authorization: `Bearer ${token}` } : {};
  const res = await fetch(`${base.replace(/\/$/, "")}${pathAndQuery}`, { headers });
  if (!res.ok) throw new Error(`GET ${pathAndQuery} -> ${res.status}`);
  return res.json();
}

/**
 * staging のスナップショットから、e2e のナビゲーションに使える実在の id を読む（Node 側の fetch のみ。
 * ブラウザから celeris を直接呼ぶコードは書かない。gui/CLAUDE.md の境界）。何も無ければ `undefined` のままにし、
 * その id が要る画面（`buildRoutes` がスキップする）とタブ切り替え検査は行わない。
 * @param {string} apiBase
 * @param {string} token
 */
async function discoverIds(apiBase, token) {
  /** @type {{taskId?: string, projectId?: string, orgId?: string, orgHeadId?: string, skillName?: string}} */
  const ids = {
    taskId: undefined,
    projectId: undefined,
    orgId: undefined,
    orgHeadId: undefined,
    skillName: undefined,
  };
  /** @type {string[]} */
  const notes = [];
  if (!apiBase) {
    notes.push(
      "E2E_API_URL is not set: skipped discovering task/project/org/skill ids from the snapshot " +
        "(the routes that need one, and the /tasks/<id> tab-switch check, were skipped too)",
    );
    return { ids, notes };
  }
  try {
    const projects = await apiFetch(apiBase, token, "/api/v1/projects");
    ids.projectId = (projects.items ?? [])[0]?.id;
    if (!ids.projectId) notes.push("GET /projects returned no items: skipped project-detail/project-docs");
  } catch (e) {
    notes.push(`GET /projects failed: ${/** @type {Error} */ (e).message}`);
  }
  try {
    const org = await apiFetch(apiBase, token, "/api/v1/org");
    /** @type {any[]} */
    const items = org.items ?? [];
    ids.orgId = items[0]?.id;
    // 部門長（他ノードの parent_id になっている）を探す。無ければ最初のノードで代用する。
    const parents = new Set(items.map((n) => n.parent_id).filter(Boolean));
    ids.orgHeadId = items.find((n) => parents.has(n.id))?.id ?? ids.orgId;
    if (!ids.orgId) notes.push("GET /org returned no items: skipped org-node/org-detail");
  } catch (e) {
    notes.push(`GET /org failed: ${/** @type {Error} */ (e).message}`);
  }
  try {
    const skills = await apiFetch(apiBase, token, "/api/v1/skills");
    ids.skillName = (skills.items ?? [])[0]?.name;
  } catch (e) {
    // skills が空/未対応でも致命的ではない（knowledge-skill-detail をスキップするだけ）。
    notes.push(`GET /skills failed or empty: ${/** @type {Error} */ (e).message}`);
  }
  try {
    const tasks = await apiFetch(apiBase, token, "/api/v1/tasks?limit=1");
    ids.taskId = (tasks.items ?? [])[0]?.id;
    if (!ids.taskId) notes.push("GET /tasks returned no items: skipped task-* routes and the tab-switch check");
  } catch (e) {
    notes.push(`GET /tasks failed: ${/** @type {Error} */ (e).message}`);
  }
  return { ids, notes };
}

async function loadChromium() {
  const require = createRequire(path.join(GUI_DIR, "package.json"));
  const { chromium } = require("@playwright/test");
  return chromium;
}

async function main() {
  const requireStaging = process.env.E2E_REQUIRE_STAGING === "1";
  const guiUrlEnv = process.env.E2E_GUI_URL;
  if (requireStaging && !guiUrlEnv) {
    console.error(
      "e2e-check: pnpm e2e:staging needs E2E_GUI_URL (and usually E2E_API_URL / E2E_TOKEN_FILE) pointing at " +
        "an already-running staging GUI/celeris. verify.sh sets these; for a manual run see docs/selfdeploy.md.",
    );
    process.exit(2);
  }
  const mode = guiUrlEnv ? "staging" : "mock";

  let chromium;
  try {
    chromium = await loadChromium();
  } catch (e) {
    const report = {
      ok: false,
      mode,
      detail: `not installed — @playwright/test not found in ${GUI_DIR}: ${/** @type {Error} */ (e).message}`,
    };
    fs.writeFileSync(path.join(OUT_DIR, "report.json"), JSON.stringify(report, null, 2));
    console.error(JSON.stringify(report, null, 2));
    process.exit(3);
  }

  const failures = [];
  const notes = [];
  let mock;
  let guiProc;
  let guiBase;
  let apiBase = process.env.E2E_API_URL ?? "";
  const token = readTrimmed(process.env.E2E_TOKEN_FILE);
  let ids;
  let browser;

  try {
    if (mode === "mock") {
      const skipBuild = process.env.E2E_SKIP_BUILD === "1";
      if (!skipBuild) {
        const build = spawnSync("pnpm", ["build"], { cwd: GUI_DIR, stdio: "inherit" });
        if (build.status !== 0) {
          console.error("e2e-check: pnpm build failed");
          process.exit(1);
        }
      }
      mock = await setupMockCeleris();
      apiBase = mock.baseUrl;
      const guiPort = await getFreePort();
      guiBase = `http://127.0.0.1:${guiPort}`;
      const guiLog = fs.openSync(path.join(OUT_DIR, "gui.log"), "w");
      guiProc = spawn(process.execPath, ["server.js"], {
        cwd: GUI_DIR,
        env: {
          ...process.env,
          NODE_ENV: "production",
          CELERIS_GUI_BIND: `127.0.0.1:${guiPort}`,
          CELERIS_API_URL: apiBase,
        },
        stdio: ["ignore", guiLog, guiLog],
      });
      await waitForHealth(`${guiBase}/healthz`);
      ids = {
        taskId: TASK_ID,
        projectId: PROJECT_ID,
        orgId: ORG_NODE_ID,
        orgHeadId: ORG_HEAD_ID,
        skillName: SKILL_NAME,
      };
    } else {
      guiBase = /** @type {string} */ (guiUrlEnv).replace(/\/$/, "");
      await waitForHealth(`${guiBase}/healthz`, 30_000);
      const discovered = await discoverIds(apiBase, token);
      ids = discovered.ids;
      notes.push(...discovered.notes);
    }

    const routes = buildRoutes(ids);
    const guiOrigin = new URL(guiBase).origin;

    browser = await chromium.launch({ headless: true });

    for (const viewport of VIEWPORTS) {
      const context = await browser.newContext({ viewport: { width: viewport.width, height: viewport.height } });
      // 外部ネットワーク不使用・GUI 以外を直接叩かない（gui/CLAUDE.md の境界の機械的な後押し）。
      await context.route("**/*", (route) => {
        const url = new URL(route.request().url());
        return url.origin === guiOrigin ? route.continue() : route.abort();
      });
      for (const { route, path: routePath } of routes) {
        const page = await context.newPage();
        /** @type {string[]} */
        const consoleErrors = [];
        /** @type {string[]} */
        const failedRequests = [];
        page.on("console", (msg) => {
          if (msg.type() === "error") consoleErrors.push(msg.text());
        });
        page.on("pageerror", (err) => consoleErrors.push(err.message));
        page.on("response", (res) => {
          if (new URL(res.url()).origin !== guiOrigin) return; // 遮った要求（route.abort）はここに来ない
          const status = res.status();
          if (status >= 400 && status !== 401)
            failedRequests.push(`${res.request().method()} ${res.url()} -> ${status}`);
        });

        let status = 0;
        try {
          const resp = await page.goto(`${guiBase}${routePath}`, { waitUntil: "load", timeout: 30_000 });
          status = resp?.status() ?? 0;
        } catch (e) {
          failures.push(`${route}@${viewport.name} (${routePath}): goto failed: ${/** @type {Error} */ (e).message}`);
          await page.close();
          continue;
        }
        if (status !== 200) failures.push(`${route}@${viewport.name} (${routePath}): GET -> ${status}`);

        // 構造チェック（受け入れ条件 2〜4）は desktop viewport のときだけ（画面幅で有無は変わらないため）。
        if (viewport.name === "desktop" && status === 200) {
          if (route === "home") {
            const consoleScreen = await page.locator('[data-testid="console-screen"]').count();
            if (consoleScreen === 0) failures.push("home: [data-testid=console-screen] is not rendered");
            const blocks = await page.locator("[data-console-block]").count();
            notes.push(`home: rendered ${blocks} console block(s)`);
          }
          if (route === "accounts") {
            const llm = await page.locator('[data-testid="llm-sources-section"]').count();
            const mcp = await page.locator('[data-testid="mcp-clients-section"]').count();
            if (llm === 0) failures.push("accounts: [data-testid=llm-sources-section] is not rendered");
            if (mcp === 0) failures.push("accounts: [data-testid=mcp-clients-section] is not rendered");
          }
          if (route === "knowledge-skills") {
            const skillsPage = await page.locator('[data-testid="knowledge-skills"]').count();
            if (skillsPage === 0) failures.push("knowledge-skills: [data-testid=knowledge-skills] is not rendered");
          }
        }

        if (consoleErrors.length > 0) {
          failures.push(`${route}@${viewport.name}: console error(s): ${consoleErrors.join(" | ").slice(0, 300)}`);
        }
        if (failedRequests.length > 0) {
          failures.push(`${route}@${viewport.name}: failed request(s): ${failedRequests.join(" | ").slice(0, 300)}`);
        }
        await page.close();
      }
      await context.close();
    }

    // 受け入れ条件 5: 実在するタスクでタブを切り替える（読み取りのみ。`?tab=` を差し替える <Link> のクリック）。
    if (ids.taskId) {
      const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
      const page = await context.newPage();
      /** @type {string[]} */
      const consoleErrors = [];
      page.on("console", (msg) => {
        if (msg.type() === "error") consoleErrors.push(msg.text());
      });
      page.on("pageerror", (err) => consoleErrors.push(err.message));
      try {
        await page.goto(`${guiBase}/tasks/${ids.taskId}?tab=overview`, { waitUntil: "load", timeout: 30_000 });
        for (const [tab, sectionTestId] of Object.entries(TASK_TAB_SECTIONS)) {
          await page.click(`[data-testid="task-tab-${tab}"]`);
          try {
            await page.waitForSelector(`[data-testid="${sectionTestId}"]`, { timeout: 10_000 });
          } catch {
            failures.push(`task tab switch ${tab}: [data-testid=${sectionTestId}] did not appear`);
          }
        }
      } catch (e) {
        failures.push(`task tab switching (task ${ids.taskId}): ${/** @type {Error} */ (e).message}`);
      }
      if (consoleErrors.length > 0) {
        failures.push(`task tab switching: console error(s): ${consoleErrors.join(" | ").slice(0, 300)}`);
      }
      await page.close();
      await context.close();
    } else {
      notes.push("no task id available: skipped the /tasks/<id> tab-switch check (read-only — never creates a task)");
    }
  } finally {
    await browser?.close().catch(() => {});
    guiProc?.kill("SIGTERM");
    await mock?.close();
  }

  const report = {
    ok: failures.length === 0,
    mode,
    gui_url: guiBase,
    api_url: apiBase || null,
    ids,
    failures,
    notes,
  };
  fs.writeFileSync(path.join(OUT_DIR, "report.json"), JSON.stringify(report, null, 2));
  console.error(JSON.stringify(report, null, 2));
  if (failures.length > 0) {
    console.error(`e2e-check: ${failures.length} failure(s). First: route ${failures[0]}`);
  }
  process.exit(failures.length === 0 ? 0 : 1);
}

await main();
