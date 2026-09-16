import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Page } from "@playwright/test";
import type { TaskDetail, TaskList } from "~/taskd/types";
import { expect, test } from "./test";

// Phase G3 の受け入れ条件 1・3・4・6・7（docs/DESIGN.md §10 Phase G3、docs/adr/0006-g3-decisions.md D6）。
// 条件 2（stream-json の整形）と条件 5（mock-taskd の 403）は Playwright ではなく `pnpm test`
// （test/unit/stream-json.test.ts、test/unit/files.route.test.ts、test/unit/artifact-view.test.ts）で確認する
// （DESIGN 本文が「実 taskd での細工 DB は taskd 側のテストに任せる」と明記しているため）。
// `scripts/taskd.sh fixture basic && scripts/taskd.sh start basic` の実 taskd（fake ワーカー並走）に対して検証する。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");
const TASKD_API_URL = "http://127.0.0.1:7710";

function sh(...args: string[]): string {
  return execFileSync(TASKD_SH, args, { cwd: REPO_ROOT, stdio: "pipe" }).toString();
}

function taskctl(...args: string[]): string {
  return execFileSync(TASKD_SH, ["taskctl", "basic", ...args], { cwd: REPO_ROOT, stdio: "pipe" })
    .toString()
    .trim();
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

async function firstRunId(taskId: string): Promise<string> {
  const runs = await apiGet<{ runs: { run_id: string }[] }>(`/tasks/${taskId}/runs`);
  const run = runs.runs[0];
  if (!run) throw new Error(`task ${taskId} has no runs`);
  return run.run_id;
}

/** `wc -l` と同じ数え方（末尾の改行 1 つは行として数えない）。 */
function wcL(text: string): number {
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  return lines.length;
}

function sha256Hex(buf: Buffer): string {
  return createHash("sha256").update(buf).digest("hex");
}

/** ページを開き、root の EventSource が /events に接続するまで待つ（G1/G2 の e2e と同じ理由）。 */
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

test.describe("受け入れ条件 1: run のログビューア", () => {
  test("stdout.jsonl の行数が実ファイルの wc -l と一致し、result.json が整形表示される", async ({ page }) => {
    const taskId = await idOf("Chain-A2", "execute");
    const runId = await firstRunId(taskId);
    const stdoutPath = path.join(REPO_ROOT, ".run/basic/workspaces/ws-a2/runs", runId, "stdout.jsonl");
    const expectedLines = wcL(readFileSync(stdoutPath, "utf8"));
    expect(expectedLines).toBeGreaterThan(0);

    await page.goto(`/tasks/${taskId}/runs/${runId}`);
    await expect(page.getByTestId("stdout-line")).toHaveCount(expectedLines);
    await expect(page.getByTestId("result-section")).toContainText("fixture done");
  });
});

test.describe("受け入れ条件 3: 成果物ビューア", () => {
  test("Markdown はテキストとして表示（script は実行されない）、JSON は CodeMirror、PNG は img、保存の sha256 が一致", async ({
    page,
  }) => {
    const taskId = await idOf("Artifacts-G", "execute");
    let dialogFired = false;
    page.on("dialog", (d) => {
      dialogFired = true;
      d.dismiss();
    });

    await page.goto(`/tasks/${taskId}`);
    const items = page.getByTestId("artifact-item");
    await expect(items).toHaveCount(3);

    // Markdown（note.md）: <script>alert(1)</script> を含むが、テキストとして表示され実行されない。
    const mdItem = items.filter({ hasText: "note.md" });
    await mdItem.getByTestId("artifact-toggle").click();
    const mdViewer = mdItem.getByTestId("markdown-viewer");
    await expect(mdViewer).toBeVisible();
    await expect(mdViewer).toContainText("alert(1)");
    expect(await mdViewer.locator("script").count()).toBe(0);

    // JSON（data.json）: CodeMirror（CodeViewer）。
    const jsonItem = items.filter({ hasText: "data.json" });
    await jsonItem.getByTestId("artifact-toggle").click();
    await expect(jsonItem.getByTestId("code-viewer")).toBeVisible();

    // PNG（image.png）: <img>。
    const pngItem = items.filter({ hasText: "image.png" });
    await pngItem.getByTestId("artifact-toggle").click();
    const img = pngItem.getByTestId("image-viewer");
    await expect(img).toBeVisible();
    await expect(img).toHaveAttribute("src", /\/files\/tasks\/.+\/artifacts\/2$/);

    expect(dialogFired).toBe(false);

    // 保存: ダウンロードしたファイルの sha256 が X-Taskd-Sha256 と一致する（data.json、idx=1）。
    const href = await jsonItem.getByTestId("artifact-download").getAttribute("href");
    expect(href).toBeTruthy();
    const headRes = await page.request.get(href as string);
    const expectedSha = headRes.headers()["x-taskd-sha256"];
    expect(expectedSha).toBeTruthy();

    const downloadPromise = page.waitForEvent("download");
    await jsonItem.getByTestId("artifact-download").click();
    const download = await downloadPromise;
    const savedPath = await download.path();
    expect(savedPath).toBeTruthy();
    const savedBuf = readFileSync(savedPath as string);
    expect(sha256Hex(savedBuf)).toBe(expectedSha);
  });
});

test.describe("受け入れ条件 4: 成果物の sha256 不一致", () => {
  test("fixture 後にファイルを書き換えると画面に sha256 不一致の警告が出る", async ({ page }) => {
    const taskId = await idOf("Artifacts-G", "execute");
    const dataPath = path.join(REPO_ROOT, ".run/basic/workspaces/ws-g/artifacts/data.json");
    const original = readFileSync(dataPath);
    try {
      writeFileSync(dataPath, '{"tampered":true}');

      await page.goto(`/tasks/${taskId}`);
      const jsonItem = page.getByTestId("artifact-item").filter({ hasText: "data.json" });
      await expect(jsonItem.getByTestId("sha256-mismatch")).toBeVisible();
    } finally {
      writeFileSync(dataPath, original);
    }
  });
});

test.describe("受け入れ条件 6: DAG", () => {
  test("depends_on の辺が期待どおり、Plan の子が group の中、スクリーンショットをベースラインとして保存", async ({
    page,
  }) => {
    // (a) = Chain-A3: depends_on は Chain-A2 からの 1 本だけ。
    const chainA3Id = await idOf("Chain-A3", "execute");
    await page.goto(`/graph?root=${chainA3Id}&depth=1`);
    const edgesLocator = page.locator(".react-flow__edge");
    await expect(edgesLocator).toHaveCount(1);

    // Plan の子 2 件が Plan の group ノードの中に収まっている。
    const planId = await idOf("fixture plan goal: build two small things", "plan");
    const planDetail = await apiGet<TaskDetail>(`/tasks/${planId}`);
    expect(planDetail.children.length).toBe(2);

    await page.goto(`/graph?root=${planId}`);
    await expect(page.getByTestId("graph-canvas")).toBeVisible();
    const group = page.locator(`.react-flow__node[data-id="group-${planId}"]`);
    await expect(group).toBeVisible();
    const groupBox = await group.boundingBox();
    expect(groupBox).toBeTruthy();
    for (const child of planDetail.children) {
      const node = page.locator(`.react-flow__node[data-id="${child.id}"]`);
      await expect(node).toBeVisible();
      const box = await node.boundingBox();
      expect(box).toBeTruthy();
      if (groupBox && box) {
        expect(box.x).toBeGreaterThanOrEqual(groupBox.x - 1);
        expect(box.y).toBeGreaterThanOrEqual(groupBox.y - 1);
        expect(box.x + box.width).toBeLessThanOrEqual(groupBox.x + groupBox.width + 1);
        expect(box.y + box.height).toBeLessThanOrEqual(groupBox.y + groupBox.height + 1);
      }
    }

    // 全体図をベースラインとしてスクリーンショット保存。
    await page.goto("/graph");
    await expect(page.getByTestId("graph-canvas")).toBeVisible();
    await page.waitForTimeout(300); // dagre のレイアウト計算とフィットが落ち着くのを待つ
    await expect(page.getByTestId("graph-canvas")).toHaveScreenshot("graph-basic.png", { maxDiffPixelRatio: 0.02 });
  });
});

test.describe("受け入れ条件 7: 実行中の run の追尾", () => {
  test("10 秒かけて progress を 5 回出す run を開くと、リロード無しで行が増える", async ({ page }) => {
    const id = taskctl(
      "add",
      "--title",
      "Slow-F",
      "--objective",
      "emits progress slowly",
      "--check-cmd",
      "test -f artifacts/out.txt",
      "--workspace",
      "ws-f",
    );
    taskctl("approve", id);

    // WorkerStarted が記録され run が現れるまで待つ（次 tick、200ms 間隔）。
    let runId: string | undefined;
    for (let i = 0; i < 50 && !runId; i += 1) {
      const runs = await apiGet<{ runs: { run_id: string }[] }>(`/tasks/${id}/runs`);
      runId = runs.runs[0]?.run_id;
      if (!runId) await new Promise((r) => setTimeout(r, 200));
    }
    expect(runId).toBeTruthy();

    await gotoWithStream(page, `/tasks/${id}/runs/${runId}`);
    const initialCount = await page.getByTestId("stdout-line").count();
    await expect
      .poll(async () => page.getByTestId("stdout-line").count(), { timeout: 8_000, intervals: [500] })
      .toBeGreaterThan(initialCount);

    // 最終的に done まで進む（後片付けと合わせて replay の健全性も確認）。仕様に時間の上限は無い。
    // 直接は 10 秒強のはずだが、e2e 中に taskd の tick が止まる現象（docs/taskd-requests.md R1、G2-U1）を
    // 踏まえ 60 秒にする（G2 の受け入れ条件 1 と同じ方針）。
    await expect
      .poll(async () => (await apiGet<TaskDetail>(`/tasks/${id}`)).task.status, { timeout: 60_000, intervals: [1_000] })
      .toBe("done");
    expect(taskctl("replay")).toContain("0 mismatches");
  });
});
