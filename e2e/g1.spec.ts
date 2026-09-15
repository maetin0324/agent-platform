import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";
import type { TaskList } from "~/taskd/types";

// Phase G1 の受け入れ条件 2〜5（docs/DESIGN.md §10 Phase G1）。
// `scripts/taskd.sh fixture basic && scripts/taskd.sh start basic` で作った既知の DB に対して検証する。
// このファイルは `basic` を起動したまま終える（他の G フェーズの e2e が上書きする）。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");
const TASKD_API_URL = "http://127.0.0.1:7710";

function sh(...args: string[]): string {
  return execFileSync(TASKD_SH, args, { cwd: REPO_ROOT, stdio: "pipe" }).toString();
}

function taskctl(...args: string[]): string {
  return execFileSync(TASKD_SH, ["taskctl", "basic", ...args], { cwd: REPO_ROOT, stdio: "pipe" }).toString();
}

async function apiGet<T>(pathAndQuery: string): Promise<T> {
  const res = await fetch(`${TASKD_API_URL}/api/v1${pathAndQuery}`);
  if (!res.ok) throw new Error(`taskd ${pathAndQuery} responded ${res.status}`);
  return (await res.json()) as T;
}

async function idOf(title: string, kind?: string): Promise<string> {
  const list = await apiGet<TaskList>(`/tasks?q=${encodeURIComponent(title)}&limit=500`);
  const item = list.items.find((i) => i.title === title && (kind === undefined || i.kind === kind));
  if (!item) throw new Error(`fixture task not found: ${title}`);
  return item.id;
}

test.beforeAll(() => {
  // 既に別 name の taskd が 7710 を掴んでいる可能性がある（G0 の e2e は 'dev' を使う）。ポートを空けてから basic を作る。
  try {
    sh("stop", "dev");
  } catch {
    // 'dev' が動いていなければ何もしない
  }
  try {
    sh("stop", "basic");
  } catch {
    // 'basic' が動いていなければ何もしない
  }
  sh("fixture", "basic");
  sh("start", "basic");
});

test.describe("受け入れ条件 2: 受信箱", () => {
  test("承認待ち・質問・draft・注意が fixture のとおり表示される", async ({ page }) => {
    await page.goto("/");

    const approvalItems = page.getByTestId("approval-item");
    await expect(approvalItems).toHaveCount(1);
    await expect(approvalItems.first()).toContainText("Approval needed:");
    await expect(approvalItems.first().getByTestId("approval-parent-title")).toContainText("Human-B");
    await expect(approvalItems.first().getByTestId("approval-criterion-text")).toContainText("someone signs off");
    await expect(approvalItems.first().getByTestId("approval-summary")).toContainText("fixture done");

    const questionItems = page.getByTestId("question-item");
    await expect(questionItems).toHaveCount(1);
    await expect(questionItems.first().getByTestId("question-text")).toContainText(
      "which environment should this target?",
    );

    const draftItems = page.getByTestId("draft-item");
    await expect(draftItems).toHaveCount(2);
    await expect(draftItems.nth(0)).toContainText("Plan-Child");
    await expect(draftItems.nth(1)).toContainText("Plan-Child");

    const attentionItems = page.getByTestId("attention-item");
    await expect(attentionItems).toHaveCount(1);
    await expect(attentionItems.first()).toContainText("Failed-E");
  });
});

test.describe("受け入れ条件 3: 一覧", () => {
  test("/tasks?status=done の行数が taskctl ls --status done と一致する", async ({ page }) => {
    const expectedLines = taskctl("ls", "--status", "done")
      .trim()
      .split("\n")
      .filter((l) => l.length > 0);

    await page.goto("/tasks?status=done");
    await expect(page.getByTestId("task-row")).toHaveCount(expectedLines.length);
  });

  test("limit=2 で「さらに読む」を最後まで押すと重複なく全件集まる", async ({ page }) => {
    const all = await apiGet<TaskList>("/tasks?limit=500&order=updated_desc");
    const expectedIds = new Set(all.items.map((i) => i.id));

    await page.goto("/tasks?limit=2&order=updated_desc");
    const loadMore = page.getByTestId("load-more");
    let guard = 0;
    while ((await loadMore.count()) > 0 && guard < 20) {
      await loadMore.click();
      guard += 1;
      await page.waitForTimeout(100);
    }

    const rows = page.getByTestId("task-row");
    const count = await rows.count();
    expect(count).toBe(expectedIds.size);
    const seenIds = new Set<string>();
    for (let i = 0; i < count; i += 1) {
      const id = await rows.nth(i).getAttribute("data-task-id");
      expect(id).toBeTruthy();
      expect(seenIds.has(id ?? "")).toBe(false);
      seenIds.add(id ?? "");
    }
    expect(seenIds).toEqual(expectedIds);
  });
});

test.describe("受け入れ条件 4: 詳細", () => {
  test("Human-B の詳細に条件・Approval リンク・run・タイムラインが揃う", async ({ page }) => {
    const humanId = await idOf("Human-B", "execute");
    const detail = await apiGet<{ runs: unknown[] }>(`/tasks/${humanId}`);

    await page.goto(`/tasks/${humanId}`);
    await expect(page.getByTestId("task-id")).toContainText(humanId);
    await expect(page.getByTestId("task-status")).toContainText("reviewing");

    await expect(page.getByText("someone signs off")).toBeVisible();
    const approvalLink = page.getByTestId("criterion-approval").locator("a");
    await expect(approvalLink).toHaveCount(1);
    await expect(approvalLink).toHaveAttribute("href", /^\/tasks\/[0-9A-HJKMNP-TV-Z]{26}$/);

    await expect(page.getByTestId("run-row")).toHaveCount(1);
    expect(detail.runs.length).toBe(1);

    const timelineTypes = await page
      .getByTestId("event-item")
      .evaluateAll((els) => els.map((el) => el.getAttribute("data-event-type")));
    expect(timelineTypes).toContain("worker_finished");
    expect(timelineTypes).not.toContain("approval_requested");
  });
});

test.describe("受け入れ条件 5: SSE", () => {
  test("taskctl add がリロード無しで /tasks に反映される", async ({ page }) => {
    // ナビゲーション前に登録する（goto 直後だと EventSource の接続が先に確立してしまい、
    // waitForResponse がその応答を取りこぼすレースになるため）。
    const eventsConnected = page.waitForResponse((res) => res.url().endsWith("/events") && res.status() === 200);
    await page.goto("/tasks");
    await expect(page.getByText("sse probe")).toHaveCount(0);
    // ブラウザの EventSource が /events への接続を確立するまで待つ（それより前に taskctl add すると
    // Created イベントが接続確立時刻より前になり SSE では届かない。実利用ではページを開いてから
    // しばらくして操作が起きるのが通常なので、この待ちは実態に即している）。
    await eventsConnected;

    taskctl("add", "--title", "sse probe", "--objective", "x", "--accept", "y", "--workspace", "ws-sse-probe");

    await expect(page.getByText("sse probe")).toBeVisible({ timeout: 3_000 });
  });

  test("curl で /events の hello と task.event が確認できる", async () => {
    let output = "";
    try {
      output = execFileSync(
        "sh",
        [
          "-c",
          `curl -N -m 3 http://127.0.0.1:7700/events & PID=$!; sleep 0.3; ${TASKD_SH} taskctl basic add --title "sse curl probe" --objective x --accept y --workspace ws-sse-curl-probe >/dev/null; wait $PID`,
        ],
        { cwd: REPO_ROOT, stdio: "pipe" },
      ).toString();
    } catch (e) {
      // curl は -m 3 で必ず非 0 終了する（意図的なタイムアウト）。それまでに受け取った stdout を使う。
      const err = e as { stdout?: Buffer };
      output = err.stdout?.toString() ?? "";
    }
    expect(output).toContain("event: hello");
    expect(output).toContain("event: task.event");
  });
});

test.describe("回帰: 子ルートのエラー表示（docs/adr/0004 D6）", () => {
  test("taskd 停止中の /tasks はバナーを出し、500 の汎用エラーにならない", async ({ page }) => {
    try {
      sh("stop", "basic");

      const response = await page.goto("/tasks");
      expect(response?.status()).not.toBe(500);
      await expect(page.getByTestId("taskd-banner")).toBeVisible();
      await expect(page.locator("body")).not.toContainText("予期しないエラーが起きました");
    } finally {
      sh("start", "basic");
    }
  });

  test("存在しない id の /tasks/:id は 404 で「タスクが見つかりません」になる", async ({ page }) => {
    const response = await page.goto("/tasks/01HZZZZZZZZZZZZZZZZZZZZZZZ");
    expect(response?.status()).toBe(404);
    await expect(page.getByText("タスクが見つかりません")).toBeVisible();
    await expect(page.locator("body")).not.toContainText("予期しないエラーが起きました");
  });
});
