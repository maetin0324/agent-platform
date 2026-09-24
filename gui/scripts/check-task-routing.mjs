// gui/scripts/check-task-routing.mjs — タスク詳細の「ルーティング」パネル（celeris ADR-0068 D5、
// `GET /tasks/{id}/routing`）が、閉じた状態で 1 行（org / harness / lane / model）、開くと features・規則・
// 理由・コスト等・レビュー結果・捨てた担当の注記を出すこと、360px 幅でも横スクロールが出ないことを確かめる。
// 偽の celeris（fixture）だけを使い、実 celeris・認証・LLM・外部ネットワークには出ない。
// 使い方: pnpm build && node scripts/check-task-routing.mjs   → JSON 1 行。失敗は exit 1。
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { getFreePort, MOBILE_DEVICE, setupMockCeleris, TASK_ID, waitForHealth } from "./lib/celeris-fixture.mjs";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(path.join(GUI_DIR, "package.json"));
const { chromium } = require("@playwright/test");

const send = (res, body) => {
  res.writeHead(200, { "content-type": "application/json; charset=utf-8" });
  res.end(JSON.stringify(body));
};

const ROUTING = {
  task_id: TASK_ID,
  assignee: "coding-poc",
  routing: { tier_source: "hint", dropped_assignee: "research-very-long-node-identifier-without-breaks" },
  runs: [
    {
      task_id: TASK_ID,
      run_id: "01RUNROUTING0000000000001",
      org_node: "coding-poc",
      harness: "coding",
      lane: "standard",
      model: "model-std",
      escalation: null,
    },
    {
      task_id: TASK_ID,
      run_id: "01RUNROUTING0000000000002",
      org_node: "coding-poc",
      harness: "coding",
      adapter: "claude-code",
      provider: "cc-1",
      lane: "frontier",
      model: "claude-some-very-long-model-identifier-20260924",
      reasoning_effort: "high",
      features: {
        judgment: "high",
        ambiguity: "high",
        verifiability: "low",
        reversibility: "high",
        consequence: "medium",
        context_size: "medium",
        tool_intensity: "medium",
        expected_length: "medium",
        cross_cutting: "low",
      },
      rule_id: "frontier/judgment-under-uncertainty",
      policy_version: "2026-09-24.1",
      reasons: ["judgment=high and (ambiguity=high or verifiability=low)", "ceiling: none"],
      escalation: "retry after review_fail: standard -> frontier",
      outcome: "done: ok",
      cost_usd: 0.4821,
      input_tokens: 123456,
      output_tokens: 7890,
      wall_ms: 184000,
      retries: 1,
      review: { passed: false, failed_criteria: [1] },
    },
  ],
};

const mock = await setupMockCeleris();
mock.on("GET", `/api/v1/tasks/${TASK_ID}/routing`, (_req, res) => send(res, ROUTING));
const base = mock.baseUrl;

const guiPort = await getFreePort();
const guiBind = `127.0.0.1:${guiPort}`;
const gui = spawn(process.execPath, ["server.js"], {
  cwd: GUI_DIR,
  env: { ...process.env, NODE_ENV: "production", CELERIS_GUI_BIND: guiBind, CELERIS_API_URL: base },
  stdio: "ignore",
});
let failed = false;
const results = [];
let browser;
try {
  await waitForHealth(`http://${guiBind}/healthz`);
  browser = await chromium.launch({ headless: true });
  const devices = [
    ["desktop", { viewport: { width: 1440, height: 900 } }],
    ["mobile-393", MOBILE_DEVICE],
    ["mobile-360", { ...MOBILE_DEVICE, viewport: { width: 360, height: 780 } }],
  ];
  for (const [name, opts] of devices) {
    const context = await browser.newContext(opts);
    const page = await context.newPage();
    await page.goto(`http://${guiBind}/tasks/${TASK_ID}`, { waitUntil: "load" });
    const panel = page.locator('[data-testid="task-routing"]');
    const summary = (await page.locator('[data-testid="task-routing-summary"]').innerText()).trim();
    const closedBodyHidden = !(await page.locator('[data-testid="task-routing-body"]').isVisible());
    await panel.locator("summary").click();
    const checks = {
      panel: (await panel.count()) === 1,
      summaryLine: summary === "coding-poc / coding / frontier / claude-some-very-long-model-identifier-20260924",
      closedBodyHidden,
      opened: await page.locator('[data-testid="task-routing-body"]').isVisible(),
      dropped: (await page.locator('[data-testid="task-routing-dropped"]').innerText()).includes("research-very-long"),
      rule: (await page.locator('[data-testid="task-routing-rule"]').innerText()).includes("2026-09-24.1"),
      reasons: (await page.locator('[data-testid="task-routing-reasons"] li').count()) === 2,
      features: (await page.locator('[data-testid="task-routing-features"] tbody tr').count()) === 9,
      review: (await page.locator('[data-testid="task-routing-review"]').innerText()).includes("#1"),
      escalations: (await page.locator('[data-testid="task-routing-escalations"] li').count()) === 1,
      noPageHScroll: await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1),
    };
    if (Object.values(checks).some((v) => !v)) failed = true;
    results.push({ device: name, summary, checks });
    await context.close();
  }
} finally {
  await browser?.close();
  gui.kill("SIGTERM");
  await mock.close();
}
process.stdout.write(`${JSON.stringify({ ok: !failed, results }, null, 1)}\n`);
process.exit(failed ? 1 : 0);
