// Drives the real releases GUI against a stateful mock API to verify the upgrade
// flow end-to-end: the flash text, the current-commit badge, and the failure banner.
// Isolated browser check; never talks to a live celeris.
import fs from "node:fs";
import http from "node:http";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import assert from "node:assert/strict";
import os from "node:os";
import { fileURLToPath } from "node:url";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(repo + "/gui/package.json");
const { chromium } = require("@playwright/test");
const f = await import(repo + "/gui/test/mock-celeris/fixtures.ts");

const OLD_SHA = "aaaaaaaaaaaa";
const NEW_SHA = "cccccccccccc";
const out = fs.mkdtempSync(path.join(os.tmpdir(), "celeris-upgrade-gui-"));

async function run(scenario, apiPort, guiPort) {
  const requests = [];
  let promoted = false;
  let pollsSincePromote = 0;

  const releasesBody = () => {
    const target = f.releaseItem({
      sha12: NEW_SHA,
      built_at: "2026-09-21T00:00:00Z",
      promoting: promoted && pollsSincePromote < 2,
      is_current: promoted && pollsSincePromote >= 2 && scenario === "success",
      promote_failed:
        promoted && pollsSincePromote >= 2 && scenario === "failure"
          ? { failed_at: "2026-09-21T00:00:05Z", error: "missing $REL/bin/celeris" }
          : null,
    });
    const currentStillCurrent = !(promoted && pollsSincePromote >= 2 && scenario === "success");
    const current = f.releaseItem({ sha12: OLD_SHA, is_current: currentStillCurrent, promoted_at: "2026-09-19T02:00:00Z" });
    return {
      ...f.defaultReleases,
      current: currentStillCurrent ? OLD_SHA : NEW_SHA,
      items: [target, current],
    };
  };

  const api = http.createServer((req, res) => {
    requests.push(`${req.method} ${req.url}`);
    const p = new URL(req.url, "http://localhost").pathname.replace("/api/v1", "");
    if (req.method === "POST" && p === `/releases/${NEW_SHA}/promote`) {
      promoted = true;
      pollsSincePromote = 0;
      res.writeHead(202, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          log: "/tmp/promote.log",
          script_from: "current",
          sha12: NEW_SHA,
          started_at: "2026-09-21T00:00:00Z",
        }),
      );
      return;
    }
    if (req.method !== "GET") {
      res.writeHead(405);
      res.end();
      return;
    }
    if (p.endsWith("/stream")) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.write(": fixture\n\n");
      return;
    }
    const values = {
      "/health": f.defaultHealth,
      "/org": { items: [] },
      "/projects": { items: [] },
      "/config": { genres: [] },
      "/daemon": { snapshot: { approvals_pending: 0, reports: { unread_secretary: 0, unread_total: 0 } } },
      "/inbox": { counts: { approvals: 0, questions: 0, drafts: 0, attention: 0 }, approvals: [], questions: [], drafts: [], attention: [] },
      "/releases": releasesBody(),
    };
    if (p === "/releases" && promoted) pollsSincePromote += 1;
    res.writeHead(p in values ? 200 : 404, { "content-type": "application/json" });
    res.end(JSON.stringify(values[p] ?? { code: "not_found", detail: p }));
  });

  let gui, browser;
  try {
    await new Promise((resolve, reject) => {
      api.once("error", reject);
      api.listen(apiPort, "127.0.0.1", resolve);
    });
    const log = fs.openSync(path.join(out, `gui-${scenario}.log`), "w");
    gui = spawn(process.execPath, ["server.js"], {
      cwd: repo + "/gui",
      env: { ...process.env, NODE_ENV: "production", CELERIS_GUI_BIND: `127.0.0.1:${guiPort}`, CELERIS_API_URL: `http://127.0.0.1:${apiPort}` },
      stdio: ["ignore", log, log],
    });
    for (let i = 0; i < 100; i++) {
      try {
        if ((await fetch(`http://127.0.0.1:${guiPort}/healthz`)).ok) break;
      } catch {}
      await new Promise((r) => setTimeout(r, 100));
    }
    browser = await chromium.launch({ headless: true });
    const page = await browser.newPage({ viewport: { width: 393, height: 900 } });
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("dialog", (d) => d.accept());
    await page.route("**/*", (route) => {
      const u = new URL(route.request().url());
      return u.hostname === "127.0.0.1" && u.port === String(guiPort) ? route.continue() : route.abort();
    });

    await page.goto(`http://127.0.0.1:${guiPort}/releases`);
    await page.locator(`#release-${NEW_SHA}`).waitFor();

    // 「昇格」という表記が残っていないことを確認する。
    assert.equal(await page.getByText("昇格").count(), 0, "「昇格」の表記が残っている");
    assert.equal((await page.getByText("upgrade", { exact: false }).count()) > 0, true, "「upgrade」の表記が見当たらない");

    await page.locator(`#release-${NEW_SHA} summary`).filter({ hasText: /^upgrade$/ }).click();
    await page.locator(`#release-${NEW_SHA}`).getByTestId("release-promote").click();

    await page.getByText("upgrade を始めました").waitFor();
    assert.match(await page.getByTestId("flash-release-promote").innerText(), new RegExp(NEW_SHA));
    await page.screenshot({ path: path.join(out, `${scenario}-01-started.png`), fullPage: true });

    if (scenario === "success") {
      await page.getByText("upgrade が完了しました").waitFor({ timeout: 15000 });
      await page.getByTestId("flash-release-promote").waitFor();
      assert.match(await page.getByTestId("flash-release-promote").innerText(), new RegExp(NEW_SHA));
      await page.locator(`#release-${NEW_SHA}`).getByTestId("release-position").waitFor();
      assert.equal(await page.locator(`#release-${NEW_SHA}`).getByTestId("release-position").innerText(), "現行");
      await page.screenshot({ path: path.join(out, `${scenario}-02-succeeded.png`), fullPage: true });
    } else {
      await page.locator(`#release-${NEW_SHA}`).getByTestId("release-promote-failed").waitFor({ timeout: 15000 });
      assert.match(
        await page.locator(`#release-${NEW_SHA}`).getByTestId("release-promote-failed").innerText(),
        /upgrade に失敗しました/,
      );
      assert.match(
        await page.locator(`#release-${NEW_SHA}`).getByTestId("release-promote-failed").innerText(),
        /missing \$REL\/bin\/celeris/,
      );
      // 失敗時は現行のコミットハッシュは変わらないまま。
      assert.equal(await page.locator(`#release-${OLD_SHA}`).getByTestId("release-position").innerText(), "現行");
      await page.screenshot({ path: path.join(out, `${scenario}-02-failed.png`), fullPage: true });
    }
    assert.deepEqual(errors, []);
    await page.close();
    return { scenario, requests: requests.length };
  } finally {
    await browser?.close();
    gui?.kill("SIGTERM");
    api.closeAllConnections();
    await new Promise((r) => api.close(r));
  }
}

const results = [];
results.push(await run("success", 17995, 17925));
results.push(await run("failure", 17996, 17926));
console.log(JSON.stringify({ ok: true, results, evidence: out }));
