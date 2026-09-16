import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { AxeBuilder } from "@axe-core/playwright";
import { expect, test } from "./test";

// Phase G6 の受け入れ条件（docs/DESIGN.md §10 Phase G6）。
// `scripts/taskd.sh fixture basic && scripts/taskd.sh start basic` で作った既知の DB に対して検証する
// （`/help` 自身は loader を持たないので taskd の内容には依存しないが、`/help` 内のリンク先の画面は taskd を要る）。
// 受け入れ条件 2 の「受信箱が空のとき」だけは `dev`（他の spec が taskctl でタスクを足さない、常に空の instance）に対して検証する。
// このファイルは `basic` を起動したまま終える。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");

function sh(...args: string[]): string {
  return execFileSync(TASKD_SH, args, { cwd: REPO_ROOT, stdio: "pipe" }).toString();
}

const HELP_SECTIONS = [
  { id: "flow", heading: "3 分で分かる流れ" },
  { id: "screens", heading: "画面ごとの説明" },
  { id: "acceptance", heading: "受け入れ条件" },
  { id: "status", heading: "状態" },
  { id: "glossary", heading: "用語集" },
  { id: "trouble", heading: "困ったとき" },
];

test.beforeAll(() => {
  try {
    sh("stop", "dev");
  } catch {
    // 動いていなければ何もしない
  }
  try {
    sh("stop", "basic");
  } catch {
    // 動いていなければ何もしない
  }
  sh("fixture", "basic");
  sh("start", "basic");
});

test.describe("受け入れ条件 1: /help の 6 節", () => {
  test("/help が 200 で、6 節の見出しと id が全て存在する", async ({ page }) => {
    const response = await page.goto("/help");
    expect(response?.status()).toBe(200);
    for (const { id, heading } of HELP_SECTIONS) {
      const section = page.locator(`#${id}`);
      await expect(section).toHaveText(heading);
    }
  });
});

test.describe("受け入れ条件 2: 導線", () => {
  test("ナビゲーションから 1 クリックで /help を開ける", async ({ page }) => {
    await page.goto("/tasks");
    await page.getByRole("link", { name: "使い方", exact: true }).click();
    await expect(page).toHaveURL(/\/help$/);
    await expect(page.getByRole("heading", { level: 1, name: "使い方" })).toBeVisible();
  });

  test("受信箱が空のとき導線が出て、押すと /help に遷移する（basic には出ない）", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("inbox-empty-help")).toHaveCount(0);

    sh("stop", "basic");
    sh("start", "dev");
    try {
      await page.goto("/");
      await expect(page.getByTestId("inbox-empty-help")).toBeVisible();
      await page.getByTestId("inbox-help-onboarding-link").click();
      await expect(page).toHaveURL(/\/help$/);
    } finally {
      sh("stop", "dev");
      sh("start", "basic");
    }
  });
});

test.describe("受け入れ条件 3: /help 内のリンクがすべて 200", () => {
  test("本文内のリンク先が全て 200 を返す", async ({ page }) => {
    await page.goto("/help");
    const hrefs = await page
      .locator("a[href]")
      .evaluateAll((els) =>
        Array.from(
          new Set(
            els.map((el) => el.getAttribute("href") ?? "").filter((href) => href.startsWith("/") && href !== "/help"),
          ),
        ),
      );
    expect(hrefs.length).toBeGreaterThan(0);
    for (const href of hrefs) {
      const response = await page.request.get(href);
      expect(response.status(), `${href} を GET した`).toBe(200);
    }
  });
});

test.describe("受け入れ条件 4: a11y", () => {
  test("/help の critical / serious な a11y violation が無い", async ({ page }) => {
    await page.goto("/help");
    const results = await new AxeBuilder({ page }).analyze();
    const gating = results.violations.filter((v) => v.impact === "critical" || v.impact === "serious");
    expect(gating, JSON.stringify(gating, null, 2)).toEqual([]);
  });
});
