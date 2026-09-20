import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Page } from "@playwright/test";
import type { EventRow, EventsPage, Task, TaskDetail, TaskList } from "~/celeris/types";
import { expect, test } from "./test";

// Phase G2 の受け入れ条件 1〜8（docs/DESIGN.md §10 Phase G2、docs/adr/0005 D7）。
// `scripts/celeris.sh fixture basic && scripts/celeris.sh start basic` の実 celeris（fake ワーカー並走）に対して検証する。
// G1 の e2e が `basic` に sse probe 等を足しているので、beforeAll で作り直す。
//
// 既定は運用中の 7700 / 7710 と同じ値になる。`playwright.config.ts` の注意書きどおり、実行時は必ず
// `CELERIS_GUI_BIND` / `CELERIS_API_URL` / `CELERIS_API_LISTEN` を別ポートへ上書きすること（Phase G13g）。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const CELERIS_SH = path.join(REPO_ROOT, "scripts/celeris.sh");
const CELERIS_API_LISTEN = process.env.CELERIS_API_LISTEN ?? "127.0.0.1:7710";
const CELERIS_API_URL = process.env.CELERIS_API_URL ?? `http://${CELERIS_API_LISTEN}`;
const GUI_URL = `http://${process.env.CELERIS_GUI_BIND ?? "127.0.0.1:7700"}`;
const ULID_RE = /^[0-9A-HJKMNP-TV-Z]{26}$/;

function sh(...args: string[]): string {
  return execFileSync(CELERIS_SH, args, {
    cwd: REPO_ROOT,
    stdio: "pipe",
    env: { ...process.env, CELERIS_API_LISTEN },
  }).toString();
}

function celerisctl(...args: string[]): string {
  return execFileSync(CELERIS_SH, ["celerisctl", "basic", ...args], {
    cwd: REPO_ROOT,
    stdio: "pipe",
    env: { ...process.env, CELERIS_API_LISTEN },
  })
    .toString()
    .trim();
}

async function apiGet<T>(pathAndQuery: string): Promise<T> {
  const res = await fetch(`${CELERIS_API_URL}/api/v1${pathAndQuery}`);
  if (!res.ok) throw new Error(`celeris ${pathAndQuery} responded ${res.status}`);
  return (await res.json()) as T;
}

/** テスト準備用に celeris へ直接 POST する（Node の fetch は Origin を送らないので通る。GUI の経路ではない）。 */
async function apiPost<T>(pathname: string, body: unknown): Promise<T> {
  const res = await fetch(`${CELERIS_API_URL}/api/v1${pathname}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`celeris POST ${pathname} responded ${res.status}: ${await res.text()}`);
  return (await res.json()) as T;
}

async function idOf(title: string, kind?: string): Promise<string> {
  const list = await apiGet<TaskList>(`/tasks?q=${encodeURIComponent(title)}&limit=500`);
  const item = list.items.find((i) => i.title === title && (kind === undefined || i.kind === kind));
  if (!item) throw new Error(`fixture task not found: ${title}`);
  return item.id;
}

async function statusOf(id: string): Promise<string> {
  return (await apiGet<TaskDetail>(`/tasks/${id}`)).task.status;
}

async function eventsOf(id: string): Promise<EventRow[]> {
  return (await apiGet<EventsPage>(`/tasks/${id}/events?limit=5000`)).items;
}

/** ページを開き、root の EventSource が /events に接続するまで待つ（G1 の e2e と同じ理由）。 */
async function gotoWithStream(page: Page, url: string): Promise<void> {
  const connected = page.waitForResponse((res) => res.url().endsWith("/events") && res.status() === 200);
  await page.goto(url);
  await connected;
}

test.beforeAll(() => {
  for (const name of ["dev", "basic"]) {
    try {
      sh("stop", name);
    } catch {
      // 動いていなければ何もしない
    }
  }
  sh("fixture", "basic");
  sh("start", "basic");
});

test.describe("受け入れ条件 1: 受信箱で Approval を note 付きで承認", () => {
  test("approval_decided(approved, note) が記録され、親が SSE 経由で done になり、replay が 0 mismatches", async ({
    browser,
  }) => {
    const humanId = await idOf("Human-B", "execute");
    const approvalId = (await apiGet<TaskDetail>(`/tasks/${humanId}`)).criteria[0]?.approval?.approval.id;
    expect(approvalId).toMatch(ULID_RE);
    expect(await statusOf(humanId)).toBe("reviewing");

    // 親の詳細を別タブで開いたまま（SSE 接続済み）にする
    const watcher = await browser.newPage();
    await gotoWithStream(watcher, `/tasks/${humanId}`);
    await expect(watcher.getByTestId("task-status")).toHaveText("reviewing");

    const page = await browser.newPage();
    // `/` は秘書へ 302 する最初の画面になった（Phase G13f-1）。受信箱は `/inbox`。
    await page.goto("/inbox");
    const item = page.getByTestId("approval-item").first();
    await expect(item).toContainText("Approval needed:");
    await item.getByTestId("approval-note").fill("looks good from the GUI");
    // POST の成否は応答そのもので確認する（`toHaveAttribute`/`toHaveText` で `flash` を待たない）。
    // root は celeris の SSE（`daemon`。tick_ms=200 で無条件に届く。docs/DESIGN.md §6.3、G1-U1）のたびに
    // 再検証し、承認直後は `/inbox` の `approvals` からこの項目が消える（G13f-U4 と同じ「決めたものに
    // 移ると成功表示も一緒に消える」レース）。この環境では 200ms 以内に消えるのが常態で、`flash` の
    // DOM を安定して観測できない（Phase G13g で判明）。承認の中身は celeris 側の記録（下記）で検証する。
    const [response] = await Promise.all([
      // React Router のデータ要求は `/inbox.data` に POST される（`action="/inbox"` の `fetcher.Form`）。
      page.waitForResponse((res) => res.url().includes("/inbox.data") && res.request().method() === "POST"),
      item.getByTestId("approval-approve").click(),
    ]);
    expect(response.status()).toBe(200);
    await expect(page.getByTestId("approval-item")).toHaveCount(0);

    // celeris 側の記録
    expect(await statusOf(approvalId ?? "")).toBe("done");
    const decided = (await eventsOf(approvalId ?? "")).find((r) => r.event.type === "approval_decided");
    expect(decided).toBeTruthy();
    expect(decided?.event).toMatchObject({ type: "approval_decided", approved: true, note: "looks good from the GUI" });

    // 親がリロード無しで done になる（fake ワーカーの他条件は pass 済み）。仕様に時間の上限は無い。
    // 直接 API では 0.5 秒だが、e2e 中に celeris の tick が 10〜25 秒止まる現象を観測している（docs/celeris-requests.md R1）ので
    // 待ちは 60 秒にし、実測値を注記に残す。
    const t0 = Date.now();
    await expect(watcher.getByTestId("task-status")).toHaveText("done", { timeout: 60_000 });
    test.info().annotations.push({ type: "parent-done-after-ms", description: String(Date.now() - t0) });
    expect(await statusOf(humanId)).toBe("done");

    expect(celerisctl("replay")).toContain("0 mismatches");
    await watcher.close();
    await page.close();
  });
});

test.describe("受け入れ条件 2: 2 つのページで同じ Approval を開き、片方で承認", () => {
  test("もう片方の承認は 409 conflict で「状態が変わりました」になり、状態は変わらない", async ({ browser }) => {
    // fixture の唯一の Approval は条件 1 が使ったので、専用の Approval（kind=approval → 初期 ready）を作る
    const approval = await apiPost<Task>("/tasks", {
      title: "Standalone-Approval-G2",
      objective: "approve me twice",
      acceptance: [{ type: "human", text: "someone approves" }],
      kind: "approval",
    });
    expect(approval.status).toBe("ready");

    const stale = await browser.newPage();
    // 古いタブを再現する: SSE を止めて再検証が走らないようにする（docs/adr/0005 D3）
    await stale.route("**/events", (route) => route.abort());
    await stale.goto(`/tasks/${approval.id}`);
    await expect(stale.getByTestId("task-status")).toHaveText("ready");
    await expect(stale.getByTestId("action-approve")).toBeVisible();

    const fresh = await browser.newPage();
    await fresh.goto(`/tasks/${approval.id}`);
    await fresh.getByTestId("action-approve").click();
    await expect(fresh.getByTestId("flash").first()).toHaveAttribute("data-flash-kind", "ok");
    await expect(fresh.getByTestId("task-status")).toHaveText("done");

    await stale.getByTestId("action-approve").click();
    const flash = stale.getByTestId("flash").first();
    await expect(flash).toHaveAttribute("data-flash-kind", "error");
    await expect(flash.getByTestId("flash-conflict")).toContainText("状態が変わりました");
    await expect(flash).toHaveAttribute("data-flash-code", /^(conflict|invalid_transition)$/);
    // action 後の再検証で古いタブも最新（done）になり、状態自体は変わっていない
    await expect(stale.getByTestId("task-status")).toHaveText("done");
    expect(await statusOf(approval.id)).toBe("done");
    const decided = (await eventsOf(approval.id)).filter((r) => r.event.type === "approval_decided");
    expect(decided).toHaveLength(1);

    await stale.close();
    await fresh.close();
  });
});

test.describe("受け入れ条件 3: blocked のタスクに回答", () => {
  test("ready になり、answered イベントの question が画面の質問文と一致する", async ({ page }) => {
    const blockedId = await idOf("Blocked-C", "execute");
    expect(await statusOf(blockedId)).toBe("blocked");

    await page.goto(`/tasks/${blockedId}`);
    const shownQuestion = (await page.getByTestId("action-question").textContent())?.trim() ?? "";
    expect(shownQuestion).toContain("which environment should this target?");

    await page.getByTestId("action-answer").fill("staging, please");
    await page.getByTestId("action-answer-submit").click();

    const flash = page.getByTestId("flash").first();
    await expect(flash).toHaveAttribute("data-flash-kind", "ok");
    await expect(flash.getByTestId("flash-from")).toHaveText("blocked");
    await expect(flash.getByTestId("flash-to")).toHaveText("ready");

    const answered = (await eventsOf(blockedId)).filter((r) => r.event.type === "answered").at(-1);
    expect(answered).toBeTruthy();
    if (answered?.event.type !== "answered") throw new Error("answered event missing");
    expect(answered.event.answer).toBe("staging, please");
    expect(shownQuestion).toContain(answered.event.question);
  });
});

test.describe("受け入れ条件 4: 後続を持つ ready タスクを cancel", () => {
  test("flash に cascaded の後続 id が出て、後続が cancelled（reason dependency_failed）", async ({ page }) => {
    // draft のまま置く依存先 → それに依存する ready（依存未完了なのでワーカーに拾われない）→ その後続 ready
    const blocker = celerisctl(
      "add",
      "--title",
      "Blocker-G2",
      "--objective",
      "stays draft",
      "--accept",
      "never",
      "--workspace",
      "ws-g2-blocker",
    );
    const target = celerisctl(
      "add",
      "--title",
      "Cancel-Target-G2",
      "--objective",
      "o",
      "--accept",
      "x",
      "--depends-on",
      blocker,
      "--workspace",
      "ws-g2-target",
    );
    celerisctl("approve", target);
    const downstream = celerisctl(
      "add",
      "--title",
      "Downstream-G2",
      "--objective",
      "o",
      "--accept",
      "x",
      "--depends-on",
      target,
      "--workspace",
      "ws-g2-downstream",
    );
    celerisctl("approve", downstream);
    expect(await statusOf(target)).toBe("ready");
    expect(await statusOf(downstream)).toBe("ready");

    await page.goto(`/tasks/${target}`);
    await expect(page.getByTestId("task-status")).toHaveText("ready");
    await page.getByTestId("action-cancel").click();

    const flash = page.getByTestId("flash").first();
    await expect(flash).toHaveAttribute("data-flash-kind", "ok");
    await expect(flash.getByTestId("flash-to")).toHaveText("cancelled");
    const cascadedIds = await flash.getByTestId("flash-cascaded-id").allTextContents();
    expect(cascadedIds).toEqual([downstream]);

    expect(await statusOf(downstream)).toBe("cancelled");
    const last = (await eventsOf(downstream)).filter((r) => r.event.type === "transitioned").at(-1);
    expect(last?.event).toMatchObject({ type: "transitioned", to: "cancelled", reason: "dependency_failed" });
  });
});

test.describe("受け入れ条件 5: 作成フォーム", () => {
  test("条件ゼロ → 422 文言、存在しない depends_on → 文言、正しい入力 → draft → 承認 → 30 秒以内に done", async ({
    page,
  }) => {
    await page.goto("/tasks/new");
    await page.locator("#title").fill("Created-From-GUI");
    await page.locator("#objective").fill("touch an artifact");
    // 受け入れ条件の行は既定で 1 行あるが空のまま送る → acceptance: []
    await page.getByTestId("submit").click();
    const acceptanceMessage =
      "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)";
    await expect(page.getByTestId("field-error-acceptance")).toContainText(acceptanceMessage);
    await expect(page.getByTestId("flash")).toHaveAttribute("data-flash-code", "validation");

    // 条件を入れ、存在しない依存 id を入れる
    const row = page.getByTestId("criterion-row").first();
    await row.locator("select").selectOption("command");
    await row.locator("input[name=criterion_value]").fill("test -f artifacts/out.txt");
    const missing = "01HZZZZZZZZZZZZZZZZZZZZZZZ";
    await page.locator("#depends_on_extra").fill(missing);
    await page.getByTestId("submit").click();
    await expect(page.getByTestId("field-error-depends_on")).toContainText(`dependency ${missing} does not exist`);

    // 正しい入力
    await page.locator("#depends_on_extra").fill("");
    await page.getByTestId("submit").click();
    await page.waitForURL(/\/tasks\/[0-9A-HJKMNP-TV-Z]{26}$/);
    const createdId = page.url().split("/").at(-1) ?? "";
    expect(createdId).toMatch(ULID_RE);
    await expect(page.getByTestId("task-title")).toHaveText("Created-From-GUI");
    await expect(page.getByTestId("task-status")).toHaveText("draft");
    expect(await statusOf(createdId)).toBe("draft");

    // 承認 → ready → fake ワーカー並走で done（SSE で自動更新、リロード無し）
    await page.getByTestId("action-approve").click();
    await expect(page.getByTestId("flash").first().getByTestId("flash-to")).toHaveText("ready");
    await expect(page.getByTestId("task-status")).toHaveText("done", { timeout: 30_000 });
    expect(await statusOf(createdId)).toBe("done");
  });
});

test.describe("受け入れ条件 6: Plan フォーム", () => {
  test("plan.auto_accept = false の説明が出て、作成すると draft の Plan になる", async ({ page }) => {
    const config = await apiGet<{ plan_auto_accept: boolean }>("/config");
    expect(config.plan_auto_accept).toBe(false);

    await page.goto("/plans/new");
    await expect(page.getByTestId("plan-auto-accept")).toContainText("plan.auto_accept = false");
    await expect(page.getByTestId("plan-auto-accept")).toContainText("draft");

    // 空の goal → 422 文言
    await page.getByTestId("submit").click();
    await expect(page.getByTestId("field-error-goal")).toContainText("goal must not be blank");

    await page.locator("#goal").fill("plan from the GUI: build two small things");
    await page.getByTestId("submit").click();
    await page.waitForURL(/\/tasks\/[0-9A-HJKMNP-TV-Z]{26}$/);
    const planId = page.url().split("/").at(-1) ?? "";
    await expect(page.getByTestId("task-kind")).toHaveText("plan");
    await expect(page.getByTestId("task-status")).toHaveText("draft");
    const detail = await apiGet<TaskDetail>(`/tasks/${planId}`);
    expect(detail.task.kind).toBe("plan");
    expect(detail.task.status).toBe("draft");
  });
});

test.describe("受け入れ条件 7: CSRF", () => {
  test("Origin: http://evil.example の POST は 403 で、状態は不変", async () => {
    // 条件 4 の生成物に依存しない（Playwright はテスト失敗後にワーカーを再起動して beforeAll を再実行し fixture を作り直す）
    const blocker = celerisctl(
      "add",
      "--title",
      "CSRF-Target-G2",
      "--objective",
      "stays draft",
      "--accept",
      "never",
      "--workspace",
      "ws-g2-csrf",
    );
    const before = await statusOf(blocker);
    expect(before).toBe("draft");

    const code = execFileSync(
      "curl",
      [
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "-X",
        "POST",
        "-H",
        "Origin: http://evil.example",
        "-d",
        "intent=cancel",
        `${GUI_URL}/tasks/${blocker}`,
      ],
      { stdio: "pipe" },
    ).toString();
    expect(code).toBe("403");
    expect(await statusOf(blocker)).toBe(before);

    // 参考: Sec-Fetch-Site: cross-site だけでも 403
    const code2 = execFileSync(
      "curl",
      [
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "-X",
        "POST",
        "-H",
        "Sec-Fetch-Site: cross-site",
        "-d",
        "intent=cancel",
        `${GUI_URL}/tasks/${blocker}`,
      ],
      { stdio: "pipe" },
    ).toString();
    expect(code2).toBe("403");
    expect(await statusOf(blocker)).toBe(before);
  });
});

test.describe("受け入れ条件 8: デーモン画面の replay", () => {
  test("replay ボタン → POST /replay の結果 0 mismatches が表示される", async ({ page }) => {
    await page.goto("/daemon");
    await expect(page.getByTestId("daemon-pid")).not.toBeEmpty();
    await page.getByTestId("replay-button").click();
    const result = page.getByTestId("replay-result");
    await expect(result).toContainText("0 mismatches");
    const expected = celerisctl("replay");
    // celerisctl replay: "replay: 0 mismatches across N tasks"
    expect(expected).toContain("0 mismatches");
    const tasks = /across (\d+) tasks/.exec(expected)?.[1];
    await expect(result).toContainText(`0 mismatches across ${tasks} tasks`);
  });
});
