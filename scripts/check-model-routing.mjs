// Local fixture only: no live daemon, credentials, account login, or LLM requests.
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { once } from "node:events";
import { resolve } from "node:path";
import { chromium } from "../gui/node_modules/@playwright/test/index.mjs";
const output = resolve(process.argv[2] ?? new URL("../docs/gui/model-routing", import.meta.url).pathname);
const measurementsPath = resolve(process.argv[3] ?? "/tmp/celeris-model-routing-measurements.json");
await mkdir(output, { recursive: true });
const providers = ["codex", "claude-code"].map((adapter, i) => ({
  id: i ? "claude" : "gpt",
  adapter,
  tiers: ["frontier", "standard", "cheap"],
  concurrency: 2,
  model: "legacy-id",
  env_keys: [],
  account_pool: true,
  account_id: "subscription-a",
  credential_refs: {},
  tier_models: Object.fromEntries(
    ["frontier", "standard", "cheap"].map((tier, j) => [
      tier,
      {
        name: (i ? ["fable", "opus", "sonnet"] : ["astra", "sol", "luna"])[j],
        model_id: j ? `fixture-${adapter}-${tier}` : null,
        unavailable_reason: j ? null : "利用可能な実行モデルIDの確認待ち",
      },
    ]),
  ),
  in_use: 0,
  cooldown: null,
  last_check: null,
  stats: {
    runs: 0,
    done: 0,
    question: 0,
    error: 0,
    requeue: 0,
    lease_expired: 0,
    input_tokens: 0,
    output_tokens: 0,
    by_day: [],
  },
}));
let saved;
const api = createServer(async (req, res) => {
  let body = "";
  for await (const part of req) body += part;
  const path = new URL(req.url, "http://localhost").pathname;
  let result = {};
  if (path.endsWith("/providers/gpt") && req.method === "PATCH") {
    saved = JSON.parse(body);
    Object.assign(providers[0], saved);
    result = providers[0];
  } else if (path.endsWith("/providers")) result = { items: providers };
  else if (path.endsWith("/reload")) result = { changed: true };
  else if (path.endsWith("/health"))
    result = {
      api_version: "1",
      schema_version: 11,
      celeris_version: "fixture",
      instance_id: "fixture",
      started_at: new Date().toISOString(),
      now: new Date().toISOString(),
      release: "fixture",
      mode: "normal",
      role: "active",
      db: { journal_mode: "wal", busy_timeout_ms: 5000 },
    };
  else if (path.endsWith("/inbox"))
    result = { counts: { ready: 0, running: 0, blocked: 0, reviewing: 0, failed: 0 }, items: [] };
  else if (path.endsWith("/daemon")) result = { snapshot: null };
  else if (path.endsWith("/accounts"))
    result = {
      root: "/fixture/claude",
      roots: { "claude-code": "/fixture/claude", codex: "/fixture/gpt" },
      max_runs_per_account: 2,
      items: ["claude-code", "codex"].map((adapter) => ({
        adapter,
        id: "subscription-a",
        dir: `/fixture/${adapter}/subscription-a`,
        logged_in: true,
        in_use: 0,
        usage: null,
        score: null,
        excluded_reason: null,
        cooldown: null,
        last_check: null,
        login_pending: false,
        stats: { runs: 0, done: 0, error: 0, input_tokens: 0, output_tokens: 0 },
      })),
    };
  else if (path.endsWith("/secrets"))
    result = {
      dir: "/fixture/secrets",
      items: [{ id: "api-key-reference", fingerprint: "fixture", updated_at: null, used_by: [] }],
    };
  else if (path.endsWith("/stream")) {
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.end();
    return;
  }
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify(result));
});
api.listen(0, "127.0.0.1");
await once(api, "listening");
const reservation = createServer();
reservation.listen(0, "127.0.0.1");
await once(reservation, "listening");
const guiPort = reservation.address().port;
await new Promise((r) => reservation.close(r));
const env = {
  ...process.env,
  CELERIS_API_URL: `http://127.0.0.1:${api.address().port}`,
  CELERIS_GUI_BIND: `127.0.0.1:${guiPort}`,
};
for (const key of ["CELERIS_API_TOKEN_FILE", "CELERIS_GUI_PASSWORD_FILE", "CELERIS_GUI_SESSION_SECRET_FILE"])
  delete env[key];
const gui = spawn(process.execPath, ["server.js"], { cwd: new URL("../gui", import.meta.url).pathname, env, stdio: ["ignore", "ignore", "pipe"] });
let serverLog = "";
gui.stderr.on("data", (b) => {
  serverLog += b;
});
let browser;
const measurements = [];
try {
  const base = `http://127.0.0.1:${guiPort}`;
  for (let i = 0; i < 100; i++) {
    if (gui.exitCode !== null) throw new Error(serverLog);
    try {
      if ((await fetch(`${base}/healthz`)).ok) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  browser = await chromium.launch({ headless: true, args: ["--no-sandbox"] });
  const page = await browser.newPage();
  const pageErrors = [];
  page.on("pageerror", (e) => pageErrors.push(String(e)));
  for (const width of [360, 393, 412, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    for (const route of ["providers", "accounts"]) {
      await page.goto(`${base}/${route}`);
      await page
        .getByRole("heading", { name: route === "providers" ? "プロバイダ" : "アカウント", level: 1 })
        .waitFor();
      if (route === "providers") {
        const card = page.locator('[data-provider-id="gpt"]');
        await card.locator("summary").filter({ hasText: "編集" }).click();
        await card.locator('input[name="model_standard"]').fill("saved-explicit-id");
        await card.locator('[data-testid="provider-edit"]').click();
        await page.waitForFunction(() => document.body.textContent.includes("saved-explicit-id"));
        if (saved?.tier_models?.standard?.model_id !== "saved-explicit-id")
          throw new Error("model save did not reach API");
        if ("env" in saved) throw new Error("form unexpectedly sent inline credentials");
      }
      await page.evaluate(() => window.scrollTo(0, 0));
      const metrics = await page.evaluate(() => ({
        viewport: innerWidth,
        content: document.documentElement.scrollWidth,
        fields: [...document.querySelectorAll("input:not([type=hidden]):not([type=checkbox]), select")]
          .filter((e) => e.getBoundingClientRect().width > 0)
          .map((e) => ({ id: e.id, width: e.getBoundingClientRect().width, height: e.getBoundingClientRect().height })),
      }));
      if (route === "providers" && metrics.fields.some((field) => field.height < 44))
        throw new Error("provider input smaller than 44px");
      if (metrics.content > width) {
        console.log(await page.evaluate(() => [...document.querySelectorAll("body *")].filter(el=>el.getBoundingClientRect().right > innerWidth).slice(0,15).map(el=>({tag:el.tagName, cls:el.className,text:el.textContent.slice(0,80), right:el.getBoundingClientRect().right}))));
        await page.screenshot({path:`${output}/${route}-${width}.png`,fullPage:true});
        throw new Error(`${route} at ${width}: horizontal overflow ${metrics.content}`);
      }
      measurements.push({ route, width, ...metrics });
      await page.screenshot({ path: `${output}/${route}-${width}.png`, fullPage: true });
    }
  }
  if (pageErrors.length) throw new Error(pageErrors.join("\n"));
  await writeFile(measurementsPath, JSON.stringify({ measurements, saved, pageErrors }, null, 2));
  console.log(
    JSON.stringify(
      { checks: measurements.length, widths: [360, 393, 412, 1440], saveVerified: true, pageErrors },
      null,
      2,
    ),
  );
} finally {
  if (browser) await browser.close();
  gui.kill("SIGTERM");
  await new Promise((r) => api.close(r));
}
