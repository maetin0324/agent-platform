// Regression: build GUI first, then node scripts/check-mobile.mjs. Isolated loopback fixtures only.
import fs from "node:fs";
import http from "node:http";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import assert from "node:assert/strict";
import os from "node:os";
import { fileURLToPath } from "node:url";
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const out = process.env.MOBILE_CHECK_OUT ?? fs.mkdtempSync(path.join(os.tmpdir(), "celeris-mobile-"));
fs.mkdirSync(out, { recursive: true });
console.log("Evidence: " + out);
const require = createRequire(repo + "/gui/package.json");
const { chromium } = require("@playwright/test");
const f = await import(repo + "/gui/test/mock-celeris/fixtures.ts");
const project = f.project({ title: "スマホGUI改善調査・長い案件名の表示確認" });
const tasks = Array.from({ length: 12 }, (_, i) =>
  f.taskSummary({
    id: `fixture-task-${i}`,
    title: `スマホGUI改善調査 ${i + 1}：長いタイトルの表示確認`,
    status: ["ready", "running", "blocked", "done", "failed", "cancelled"][i % 6],
  }),
);
const blocks = Array.from({ length: 4 }, (_, i) =>
  f.consoleBlocks().map((b, j) => ({ ...b, cursor: `fixture-${i}-${j}` })),
).flat();
const report = f.consoleBlocks().find((b) => b.kind === "report").report;
const requests = [];
const api = http.createServer((req, res) => {
  const u = new URL(req.url, "http://localhost");
  const p = u.pathname.replace("/api/v1", "");
  requests.push({ method: req.method, path: u.pathname, query: u.search });
  if (req.method !== "GET") {
    res.writeHead(405);
    res.end();
    return;
  }
  if (p === "/stream" || p === "/console/stream") {
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write(": fixture\n\n");
    return;
  }
  const values = {
    "/health": f.defaultHealth,
    "/console": { items: blocks, next_cursor: null },
    "/org": { items: [] },
    "/projects": { items: [project] },
    "/projects/p1": { project, milestones: [], tasks: [] },
    "/clusters": { items: [] },
    "/tasks": {
      items: tasks,
      total: 12,
      next_cursor: null,
      counts_by_status: { ready: 2, running: 2, blocked: 2, done: 2, failed: 2, cancelled: 2 },
    },
    "/config": { genres: [] },
    "/inbox": {
      counts: { approvals: 0, questions: 0, drafts: 0, attention: 0 },
      approvals: [],
      questions: [],
      drafts: [],
      attention: [],
    },
    "/daemon": { snapshot: { approvals_pending: 1, reports: { unread_secretary: 1, unread_total: 1 } } },
    "/approvals": {
      items:
        u.searchParams.get("pending") === "true"
          ? [
              {
                id: "fixture-approval",
                node_id: "cos",
                project_id: "p1",
                question: "長い認可対象の確認：スマホGUIの表示を読み取り専用で検証します。",
                created_at: "2026-09-20T00:00:00Z",
                decision: null,
              },
            ]
          : [],
    },
    "/standing-rules": { items: [] },
    "/reports": { items: [report], next_cursor: null },
    "/knowledge/tree": f.knowledgeTree(),
    "/knowledge/page": f.knowledgePage(),
  };
  res.writeHead(p in values ? 200 : 404, { "content-type": "application/json" });
  res.end(JSON.stringify(values[p] ?? { code: "not_found", detail: "fixture endpoint unavailable" }));
});
let gui, browser;
try {
  await new Promise((resolve, reject) => {
    api.once("error", reject);
    api.listen(17971, "127.0.0.1", resolve);
  });
  const log = fs.openSync(out + "/server.log", "w");
  gui = spawn(process.execPath, ["server.js"], {
    cwd: repo + "/gui",
    env: {
      PATH: process.env.PATH,
      HOME: process.env.HOME,
      NODE_ENV: "production",
      CELERIS_GUI_BIND: "127.0.0.1:17901",
      CELERIS_API_URL: "http://127.0.0.1:17971",
    },
    stdio: ["ignore", log, log],
  });
  let ready = false;
  for (let i = 0; i < 100; i++) {
    try {
      const r = await fetch("http://127.0.0.1:17901/healthz");
      if (r.ok) {
        ready = true;
        break;
      }
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  if (!ready) throw Error("GUI not ready");
  browser = await chromium.launch({ headless: true });
  const results = [];
  for (const [width, height] of [
    [360, 800],
    [393, 852],
    [412, 915],
    [1023, 768],
    [1024, 768],
    [1440, 900],
  ]) {
    const context = await browser.newContext({
      viewport: { width, height },
      isMobile: width < 500,
      hasTouch: width < 500,
      deviceScaleFactor: 1,
      colorScheme: "light",
    });
    await context.route("**/*", (route) => {
      const u = new URL(route.request().url());
      return u.hostname === "127.0.0.1" && u.port === "17901" && route.request().method() === "GET"
        ? route.continue()
        : route.abort();
    });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    for (const route of ["/", "/tasks", "/projects", "/board", "/approvals", "/knowledge", "/reports"]) {
      const name = route === "/" ? "console" : route.slice(1);
      const response = await page.goto("http://127.0.0.1:17901" + route);
      await page.waitForTimeout(500);
      const measurement = await page.evaluate(() => {
        const rect = (el) => {
          if (!el) return null;
          const r = el.getBoundingClientRect(),
            s = getComputedStyle(el);
          return {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            clientWidth: el.clientWidth,
            scrollWidth: el.scrollWidth,
            clientHeight: el.clientHeight,
            scrollHeight: el.scrollHeight,
            font: s.fontSize,
          };
        };
        const test = (id) => document.querySelector(`[data-testid="${id}"]`);
        return {
          documentWidth: document.documentElement.clientWidth,
          scrollWidth: document.documentElement.scrollWidth,
          heading: document.querySelector("h1")?.textContent,
          nav: rect(document.querySelector("nav")),
          navLinks: [...document.querySelectorAll("nav a")].map((a) => ({
            text: a.textContent,
            href: a.getAttribute("href"),
            ...rect(a),
          })),
          taskTitle: rect(test("task-row")?.querySelector("a")),
          reportHeadline: rect(test("console-report-headline")),
          tables: [...document.querySelectorAll("main table")].map((e) => ({
            table: rect(e),
            parent: rect(e.parentElement),
          })),
          stream: rect(test("console-stream")),
          input: rect(test("console-text")),
          send: rect(test("console-send")),
          controls: [...document.querySelectorAll("main button,main select,main input,main textarea")]
            .filter((e) => e.getBoundingClientRect().height > 0)
            .map((e) => ({
              label: e.textContent?.trim().slice(0, 60) || e.getAttribute("aria-label") || e.getAttribute("name"),
              ...rect(e),
            })),
        };
      });
      assert.equal(response.status(), 200, `${name}@${width}: response`);
      assert.equal(errors.length, 0, errors.join("\n"));
      assert.ok(measurement.scrollWidth <= measurement.documentWidth + 1, `${name}@${width}: document overflow`);
      if (route === "/tasks") assert.ok(measurement.taskTitle?.width > 150, `task title disappeared @${width}`);
      await page.screenshot({ path: `${out}/${name}-${width}.png`, fullPage: true });
      if (route === "/") {
        assert.ok(measurement.reportHeadline?.width > 100, `report headline disappeared @${width}`);
        assert.ok(measurement.send?.height >= 44, `send touch target @${width}`);
        assert.ok(parseFloat(measurement.input.font) >= 16, `input font @${width}`);
        await page.getByTestId("console-text").fill("日本語変換");
        await page
          .getByTestId("console-text")
          .dispatchEvent("keydown", {
            key: "Enter",
            code: "Enter",
            isComposing: true,
            bubbles: true,
            cancelable: true,
          });
        assert.equal(await page.getByTestId("console-text").inputValue(), "日本語変換");
        if (width < 500) {
          await page.getByTestId("console-text").press("Enter");
          assert.ok(
            (await page.getByTestId("console-text").inputValue()).includes("\n"),
            JSON.stringify({
              message: "touch Enter must insert newline",
              coarse: await page.evaluate(() => matchMedia("(pointer: coarse)").matches),
              value: await page.getByTestId("console-text").inputValue(),
            }),
          );
        }

        await page.evaluate(() => scrollTo(0, 250));
        await page.waitForTimeout(100);
        measurement.scrolled = await page.evaluate(() => {
          const a = document.querySelector("aside").getBoundingClientRect(),
            b = document.querySelector('[data-testid="console-waiting-strip"]').getBoundingClientRect();
          return {
            scrollY,
            aside: { top: a.top, bottom: a.bottom },
            waiting: { top: b.top, bottom: b.bottom },
            overlapWidth: Math.max(0, Math.min(a.right, b.right) - Math.max(a.left, b.left)),
            overlapHeight: Math.max(0, Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top)),
            overlapArea:
              Math.max(0, Math.min(a.right, b.right) - Math.max(a.left, b.left)) *
              Math.max(0, Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top)),
          };
        });
        assert.notEqual(
          await page.getByTestId("console-waiting-strip").evaluate((el) => getComputedStyle(el).position),
          "sticky",
          `waiting strip must scroll with the page @${width}`,
        );
        await page.screenshot({ path: `${out}/console-scrolled-${width}.png` });
        await page.getByTestId("console-text").fill("検証用の未送信テキスト");
        await page.getByTestId("console-send").scrollIntoViewIfNeeded();
        await page.screenshot({ path: `${out}/console-input-${width}.png` });
      }
      if (route === "/tasks") {
        await page.getByTestId("task-list-scroll").scrollIntoViewIfNeeded();
        await page.screenshot({ path: `${out}/tasks-rows-${width}.png` });
      }
      if (width < 1024 && route === "/reports") {
        await page.getByTestId("mobile-menu").click();
        assert.equal(
          await page.locator('nav[aria-label="メイン"] a:visible').count(),
          19,
          "all destinations must be reachable",
        );
        const links = await page.locator('nav[aria-label="メイン"] a:visible').evaluateAll((items) =>
          items.map((el) => {
            const r = el.getBoundingClientRect();
            return { left: r.left, right: r.right, height: r.height };
          }),
        );
        assert.ok(
          links.every((r) => r.left >= 0 && r.right <= width && r.height >= 44),
          "menu targets fit the screen",
        );
        await page.screenshot({ path: `${out}/menu-${width}.png` });
        await page.keyboard.press("Escape");
        assert.equal(await page.getByTestId("mobile-menu").getAttribute("aria-expanded"), "false");
        assert.equal(await page.getByTestId("mobile-menu").evaluate((el) => el === document.activeElement), true);
        await page.getByTestId("mobile-menu").click();
        await page.locator('nav a[href="/help"]').click();
        await page.waitForURL("**/help");
        measurement.navEndReached = page.url();
      }
      results.push({ width, height, route, status: response.status(), errors: [...errors], ...measurement });
    }
    await context.close();
  }
  fs.writeFileSync(
    out + "/measurements.json",
    JSON.stringify(
      {
        timestamp: new Date().toISOString(),
        kind: "actual local GUI built from worktree; synthetic read-only API fixtures; Chromium viewport/touch emulation, not Nothing 2a",
        browser: browser.version(),
        results,
      },
      null,
      2,
    ),
  );
  assert.ok(
    requests.every((r) => r.method === "GET"),
    "fixtures must remain read-only",
  );
  console.log(
    JSON.stringify(
      {
        browser: browser.version(),
        pages: results.length,
        errors: results.filter((r) => r.errors.length || r.status !== 200),
        widths: [360, 393, 412, 1023, 1024, 1440],
      },
      null,
      2,
    ),
  );
} finally {
  fs.writeFileSync(out + "/api-requests.json", JSON.stringify(requests, null, 2));
  await browser?.close();
  gui?.kill("SIGTERM");
  api.closeAllConnections();
  await new Promise((r) => api.close(r));
}
