import { execFileSync } from "node:child_process";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Inbox, TaskDetail, TaskList } from "~/taskd/types";
import { expect, test } from "./test";

// Phase G4 の受け入れ条件 1〜4（docs/DESIGN.md §10 Phase G4、docs/adr/0007-g4-decisions.md）。
// `multi-account` は cooldown がプロセス内メモリのみで DB から再構築できないため（ADR-0007 D1）、
// フィクスチャの構築（taskctl add/approve でスロットルを起こす）を、このファイルの中で
// `start multi-account` した生きたプロセスに対して直接行う。他の条件は `basic` / `unroutable` を使う。
//
// このファイルは他の e2e ファイルより頻繁に taskd を stop/start して別名のインスタンスへ切り替える
// （multi-account → basic → unroutable → basic …）。Node の `fetch` は既定でコネクションを
// keep-alive で使い回すため、直前の stop で消えたソケットへの再利用が「SocketError: other side
// closed」を起こすことがある（実測）。`apiGet` は `node:http` を `agent: false` で直接使い、
// 呼び出しごとに新しいソケットを張ることでこれを避ける（e2e/g0.spec.ts の `getWithHost` と同じ方針）。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");
const TASKD_API_HOST = "127.0.0.1";
const TASKD_API_PORT = 7710;
const INSTANCE_NAMES = ["dev", "basic", "multi-account", "unroutable"] as const;

function sh(...args: string[]): string {
  return execFileSync(TASKD_SH, args, { cwd: REPO_ROOT, stdio: "pipe" }).toString();
}

function taskctl(name: string, ...args: string[]): string {
  return execFileSync(TASKD_SH, ["taskctl", name, ...args], { cwd: REPO_ROOT, stdio: "pipe" })
    .toString()
    .trim();
}

function stopAll(): void {
  for (const name of INSTANCE_NAMES) {
    try {
      sh("stop", name);
    } catch {
      // 動いていなければ何もしない
    }
  }
}

function apiGet<T>(pathAndQuery: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: TASKD_API_HOST, port: TASKD_API_PORT, path: `/api/v1${pathAndQuery}`, method: "GET", agent: false },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => {
          const status = res.statusCode ?? 0;
          if (status < 200 || status >= 300) {
            reject(new Error(`taskd ${pathAndQuery} responded ${status}`));
            return;
          }
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")) as T);
          } catch (e) {
            reject(e);
          }
        });
      },
    );
    req.on("error", reject);
    req.end();
  });
}

async function idOf(title: string): Promise<string> {
  const list = await apiGet<TaskList>(`/tasks?q=${encodeURIComponent(title)}&limit=500`);
  const item = list.items.find((i) => i.title === title);
  if (!item) throw new Error(`fixture task not found: ${title}`);
  return item.id;
}

async function waitForStatus(id: string, status: string, timeoutMs: number): Promise<void> {
  await expect
    .poll(async () => (await apiGet<TaskDetail>(`/tasks/${id}`)).task.status, { timeout: timeoutMs, intervals: [500] })
    .toBe(status);
}

test.describe("受け入れ条件 1: プロバイダ画面（multi-account）", () => {
  let fallbackId = "";

  test.beforeAll(async () => {
    stopAll();
    sh("fixture", "multi-account");
    sh("start", "multi-account");
    fallbackId = taskctl(
      "multi-account",
      "add",
      "--title",
      "Fallback-MA",
      "--objective",
      "multi-account scenario",
      "--check-cmd",
      "test -f account.txt",
      "--max-retries",
      "0",
      "--workspace",
      "ws-fallback",
    );
    taskctl("multi-account", "approve", fallbackId);
    // 単体では 1 秒未満で done になるが、他の e2e ファイルで観測されている taskd の間欠停止
    // （docs/taskd-requests.md R1、G2-U1）に当たると数十秒 tick が止まることがあるため、
    // G2/G3 の e2e と同じ方針で上限を 60 秒にする。
    await waitForStatus(fallbackId, "done", 60_000);
  });

  test.afterAll(() => {
    sh("stop", "multi-account");
  });

  test("acct-a が requeue 1・cooldown 残り時間表示、acct-b が done 1、tokens 合計が runs の usage と一致", async ({
    page,
  }) => {
    await page.goto("/providers");
    await expect(page.getByTestId("providers-section")).toBeVisible();

    const rowA = page.locator('[data-testid="provider-row"][data-provider-id="acct-a"]');
    const rowB = page.locator('[data-testid="provider-row"][data-provider-id="acct-b"]');
    await expect(rowA.getByTestId("provider-requeue")).toHaveText("1");
    await expect(rowA.getByTestId("provider-done")).toHaveText("0");
    await expect(rowB.getByTestId("provider-done")).toHaveText("1");
    await expect(rowB.getByTestId("provider-requeue")).toHaveText("0");

    const remaining = await rowA.getByTestId("provider-cooldown-remaining").textContent();
    expect(remaining).toMatch(/\d/);
    await expect(rowA.getByTestId("provider-cooldown-reason")).toHaveText("throttled");
    await expect(rowB.getByTestId("provider-cooldown-remaining")).toHaveCount(0);

    // ADR-0022 D2: 疎通確認は自動では走らないので、叩いていないアカウントは「未確認」。
    await expect(rowA.getByTestId("provider-last-check")).toHaveText("未確認");
    await expect(rowB.getByTestId("provider-last-check")).toHaveText("未確認");

    const runs = await apiGet<{ runs: { usage: { input_tokens: number; output_tokens: number } | null }[] }>(
      `/tasks/${fallbackId}/runs`,
    );
    const expectedTokens = runs.runs.reduce(
      (sum, r) => sum + (r.usage?.input_tokens ?? 0) + (r.usage?.output_tokens ?? 0),
      0,
    );
    const tokensA = Number(await rowA.getByTestId("provider-tokens").textContent());
    const tokensB = Number(await rowB.getByTestId("provider-tokens").textContent());
    expect(tokensA + tokensB).toBe(expectedTokens);
    expect(expectedTokens).toBeGreaterThan(0);
  });
});

test.describe("受け入れ条件 2a: デーモン画面（pid/hostname/ticks、fixture (b) の awaiting_human）", () => {
  let humanBId = "";

  test.beforeAll(async () => {
    stopAll();
    sh("fixture", "basic");
    sh("start", "basic");
    humanBId = await idOf("Human-B");
  });

  test.afterAll(() => {
    sh("stop", "basic");
  });

  test("pid/hostname/ticks が表示され、5 秒後の再読込で ticks が増え、Human-B が awaiting_human に 1 件", async ({
    page,
  }) => {
    await page.goto("/daemon");
    await expect(page.getByTestId("daemon-pid")).not.toHaveText("");
    await expect(page.getByTestId("daemon-hostname")).not.toHaveText("");

    const before = Number(await page.getByTestId("daemon-ticks").textContent());
    await page.waitForTimeout(5_000);
    await page.reload();
    const after = Number(await page.getByTestId("daemon-ticks").textContent());
    expect(after).toBeGreaterThan(before);

    await expect(page.locator(`[data-testid="awaiting-human-item"] a[href="/tasks/${humanBId}"]`)).toHaveCount(1);
  });
});

test.describe("受け入れ条件 2b: unroutable フィクスチャ", () => {
  let unroutableId = "";

  test.beforeAll(async () => {
    stopAll();
    sh("fixture", "unroutable");
    sh("start", "unroutable");
    const inbox = await apiGet<Inbox>("/inbox");
    const item = inbox.attention.find((a) => a.type !== "cluster_unavailable" && a.task.title === "Unroutable-U");
    if (!item || item.type === "cluster_unavailable") throw new Error("unroutable task not found in /inbox attention");
    unroutableId = item.task.id;
  });

  test.afterAll(() => {
    sh("stop", "unroutable");
  });

  test("受信箱の注意と /daemon の unroutable に同じ id が出る", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator(`[data-testid="attention-item"] a[href="/tasks/${unroutableId}"]`)).toHaveCount(1);

    await page.goto("/daemon");
    await expect(page.locator(`[data-testid="unroutable-item"] a[href="/tasks/${unroutableId}"]`)).toHaveCount(1);
  });
});

test.describe("受け入れ条件 3: 停止/復旧バナーと SSE の再接続", () => {
  test.beforeAll(() => {
    stopAll();
    sh("fixture", "basic");
    sh("start", "basic");
  });

  test.afterAll(() => {
    sh("stop", "basic");
  });

  test("stop → 5 秒以内に全ページでバナー、start → 5 秒以内に消え、SSE が再接続して task.event が再び届く", async ({
    page,
  }) => {
    const eventsConnected = page.waitForResponse((res) => res.url().endsWith("/events") && res.status() === 200);
    await page.goto("/tasks");
    await eventsConnected;
    await expect(page.getByTestId("taskd-banner")).toHaveCount(0);

    // root は既に「切断」を検知していない限り再検証しない（app/root.tsx）ので、次の操作（ここではナビゲーション）が
    // 停止を検知する最初の機会になる。G0 の e2e（e2e/g0.spec.ts）と同じ規約。
    sh("stop", "basic");
    const stoppedAt = Date.now();
    await page.reload();
    await expect(page.getByTestId("taskd-banner")).toBeVisible();
    await expect(page.getByTestId("taskd-banner")).toContainText("taskd に接続できません");
    expect(Date.now() - stoppedAt).toBeLessThan(5_000);

    // /daemon・/providers は自身の ErrorBoundary でも TaskdBanner を出すため、root のものと合わせて
    // 2 つ描画されうる（ADR-0007 D8。どちらも同じ文言なので `.first()` で見る）。
    await page.goto("/daemon");
    await expect(page.getByTestId("taskd-banner").first()).toBeVisible();
    await page.goto("/providers");
    await expect(page.getByTestId("taskd-banner").first()).toBeVisible();

    const reconnected = page.waitForResponse((res) => res.url().endsWith("/events") && res.status() === 200);
    sh("start", "basic");
    await page.goto("/tasks");
    await reconnected;
    await expect(page.getByTestId("taskd-banner")).toBeHidden({ timeout: 5_000 });

    await expect(page.getByText("Reconnect-Check")).toHaveCount(0);
    taskctl(
      "basic",
      "add",
      "--title",
      "Reconnect-Check",
      "--objective",
      "sse reconnect after recovery",
      "--accept",
      "y",
      "--workspace",
      "ws-reconnect",
    );
    await expect(page.getByText("Reconnect-Check")).toBeVisible({ timeout: 5_000 });
  });
});

test.describe("受け入れ条件 4: 実行中 run の in_flight 表示", () => {
  test.beforeAll(() => {
    stopAll();
    sh("fixture", "basic");
    sh("start", "basic");
  });

  test.afterAll(() => {
    sh("stop", "basic");
  });

  test("20 秒ワーカー実行中は in_flight に task/run_id/provider/経過時間が出て、終了後に消える", async ({ page }) => {
    const id = taskctl(
      "basic",
      "add",
      "--title",
      "Slow-H",
      "--objective",
      "emits nothing for 20s",
      "--check-cmd",
      "test -f artifacts/out.txt",
      "--workspace",
      "ws-h",
    );
    taskctl("basic", "approve", id);

    await expect
      .poll(async () => (await apiGet<{ runs: { run_id: string }[] }>(`/tasks/${id}/runs`)).runs.length, {
        timeout: 5_000,
        intervals: [200],
      })
      .toBeGreaterThan(0);

    await page.goto("/daemon");
    const row = page.locator(`[data-testid="in-flight-row"][data-task-id="${id}"]`);
    await expect(row).toBeVisible({ timeout: 5_000 });
    await expect(row.getByTestId("in-flight-provider")).toHaveText("fake-local");
    await expect(row.getByTestId("in-flight-run-id")).not.toHaveText("");
    const elapsed = await row.getByTestId("in-flight-elapsed").textContent();
    expect(elapsed).toMatch(/\d/);

    // 20 秒 sleep に加え、taskd の間欠停止（G2-U1）の余地を見て 60 秒にする。
    await waitForStatus(id, "done", 60_000);
    await page.reload();
    await expect(page.locator(`[data-testid="in-flight-row"][data-task-id="${id}"]`)).toHaveCount(0);
  });
});
