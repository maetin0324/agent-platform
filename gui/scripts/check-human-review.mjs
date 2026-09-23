// gui/scripts/check-human-review.mjs — タスク詳細の (1) run の outcome 欄がステータス名だけであること、
// (2) 人のレビュー待ち（reviewing + human 条件）の判断材料が出ることを、デスクトップ幅とスマホ幅で確認する。
// 偽の celeris（fixture）だけを使い、実 celeris・認証・LLM・外部ネットワークには出ない。
// 使い方: pnpm build && node scripts/check-human-review.mjs   → docs/gui/human-review/*.png と JSON 1 行を出力。
// 失敗（期待の要素が無い・横スクロールが出る）は exit 1。
import { spawn } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { getFreePort, MOBILE_DEVICE, setupMockCeleris, TASK_ID, waitForHealth } from "./lib/celeris-fixture.mjs";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(path.join(GUI_DIR, "package.json"));
const { chromium } = require("@playwright/test");
const OUT_DIR = path.join(GUI_DIR, "docs/gui/human-review");
fs.mkdirSync(OUT_DIR, { recursive: true });

const LONG = `${"長い要約の文章。".repeat(40)}\n- 変更: crates/foo/src/lib.rs\n- テスト: cargo test 成功`;
const APPROVAL_ID = "01HUMANAPPROVAL00000000001";
const send = (res, body) => {
  res.writeHead(200, { "content-type": "application/json; charset=utf-8" });
  res.end(JSON.stringify(body));
};

const mock = await setupMockCeleris();
const base = await new Promise((resolve) => {
  // 既存の task 詳細を reviewing + human 条件 + 長い outcome_text の run に差し替える。
  mock.on("GET", `/api/v1/tasks/${TASK_ID}`, (_req, res) =>
    send(res, {
      actions: ["cancel"],
      answers: [],
      approvals: [
        { approval: { id: APPROVAL_ID, kind: "approval", status: "ready", title: "承認", actions: ["approve", "reject"] }, criterion_idx: 1, attempt: 1 },
      ],
      children: [],
      criteria: [
        { idx: 0, text: "cargo test が通る", check: { type: "command", cmd: "cargo test", expect_exit: 0 }, latest_verdict: { criterion_idx: 0, pass: true, reason: "exit 0 を確認", run_id: "R1", ts: "2026-09-21T00:00:00Z" } },
        { idx: 1, text: "人が挙動を確認して承認する", check: { type: "human" }, approval: { approval: { id: APPROVAL_ID, kind: "approval", status: "ready", title: "承認", actions: ["approve", "reject"] }, criterion_idx: 1, attempt: 1 } },
      ],
      delegated: [],
      dependencies: [],
      dependents: [],
      priority_label: "P2",
      prior_review: [{ criterion: 0, pass: false, reason: "前回はテストが 1 件失敗していた" }],
      runs: [
        { run_id: "01RUNAAAAAAAAAAAAAAAAAAAAA", role: "worker", adapter: "claude-code", model: "m", provider: "p", started_at: "2026-09-21T00:00:00Z", finished_at: "2026-09-21T00:05:00Z", outcome: "done", outcome_text: LONG, usage: null, progress: 0, artifacts: 1, verdicts: 1, reviewer_deferrals: 0, files: { stdout: true, stderr: true, result: true } },
      ],
      task: { ...(await_task()), status: "reviewing" },
      timers: { consecutive_requeues: 0, consecutive_reviewer_requeues: 0, max_requeues: 2, now: "2026-09-21T00:10:00Z" },
    }),
  );
  mock.on("GET", "/api/v1/inbox", (_req, res) =>
    send(res, {
      approvals: [
        {
          approval: { id: APPROVAL_ID, kind: "approval", status: "ready", title: "承認", actions: ["approve", "reject"] },
          parent: { id: TASK_ID, kind: "execute", status: "reviewing", title: "GUI 改善", actions: ["cancel"] },
          criterion_idx: 1,
          criterion_text: "人が挙動を確認して承認する",
          requested_at: "2026-09-21T00:06:00Z",
          last_run: { run_id: "R1", role: "worker", adapter: "claude-code", model: "m", started_at: "2026-09-21T00:00:00Z", outcome: "done", outcome_text: LONG, progress: 0, artifacts: 1, verdicts: 1, reviewer_deferrals: 0 },
          evidence: [{ criterion: 0, command: "cargo test --workspace", exit: 0, stdout_tail: "test result: ok. 1978 passed" }],
          other_verdicts: [{ criterion_idx: 0, pass: true, reason: "exit 0", run_id: "R1", ts: "2026-09-21T00:05:00Z" }],
          artifacts: [{ kind: "report", name: "report.md", path: "artifacts/report.md", sha256: "abc" }],
          previous_decisions: [],
        },
      ],
      questions: [],
      drafts: [],
      attention: [],
      counts: { approvals: 1, attention: 0, by_status: {}, drafts: 0, questions: 0 },
    }),
  );
  resolve(mock.baseUrl);
});
function await_task() {
  return {
    id: TASK_ID, kind: "execute", status: "reviewing", title: "GUI 改善", objective: "GUI を直す", priority: 5, attempts: 1,
    created_at: "2026-09-21T00:00:00Z", updated_at: "2026-09-21T00:06:00Z", acceptance: [], depends_on: [], inputs: [],
    worker_hint: { tier: "standard" }, budget: { max_retries: 3, max_turns: 10, max_wall_secs: 600 }, workspace: { kind: "local", path: "." },
  };
}

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
    ["mobile", MOBILE_DEVICE],
  ];
  for (const [name, opts] of devices) {
    const context = await browser.newContext(opts);
    const page = await context.newPage();
    await page.goto(`http://${guiBind}/tasks/${TASK_ID}`, { waitUntil: "load" });
    await page.waitForTimeout(500);
    const outcomeBadgeText = await page.locator('[data-testid="run-row"] td:nth-child(9) span').first().innerText();
    const checks = {
      outcomeBadgeOnlyStatus: outcomeBadgeText.trim() === "done",
      outcomeDetailFolded: (await page.locator('[data-testid="run-outcome-detail"]').count()) === 1,
      humanReviewSection: (await page.locator('[data-testid="human-review-section"]').count()) === 1,
      summary: (await page.locator('[data-testid="human-review-summary"]').count()) === 1,
      criteria: (await page.locator('[data-testid="human-review-criteria"] li').count()) === 2,
      evidence: (await page.locator('[data-testid="human-review-evidence"]').count()) === 1,
      artifacts: (await page.locator('[data-testid="human-review-artifacts"] li').count()) === 1,
      approve: (await page.locator('[data-testid="human-review-approve"]').count()) === 1,
      reject: (await page.locator('[data-testid="human-review-reject"]').count()) === 1,
      noPageHScroll: await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1),
    };
    await page.screenshot({ path: path.join(OUT_DIR, `task-reviewing-${name}.png`), fullPage: true });
    if (Object.values(checks).some((v) => !v)) failed = true;
    results.push({ device: name, outcomeBadgeText, checks });
    await context.close();
  }
} finally {
  await browser?.close();
  gui.kill("SIGTERM");
  await mock.close();
}
console.log(JSON.stringify({ ok: !failed, results }, null, 1));
process.exit(failed ? 1 : 0);
