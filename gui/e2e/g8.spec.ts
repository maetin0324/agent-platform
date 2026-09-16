import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "./test";

// gui/docs/adr/0012-provider-and-account-management.md D2〜D4。プロバイダ管理・アカウントのプール画面を、
// `[api] token_file` + `providers_include` + `[accounts]` 付きの taskd（`scripts/taskd.sh fixture accounts`）に対して確認する。
//
// このファイルは人間の本番 taskd/GUI（127.0.0.1:7710 / 0.0.0.0:7700）と衝突しないポートを使う。実行前に
// 1 回だけ fixture を作り（トークンファイルが生成される）、その絶対パスを `TASKD_API_TOKEN_FILE` として
// Playwright の webServer（`playwright.config.ts`）に渡す。`env` を指定していない webServer も
// 呼び出し元の `process.env` をそのまま引き継ぐ（Playwright の既定動作。実測で確認済み）ので、
// `playwright.config.ts` 自体は変更しない。管理系 API はトークンを起動時に 1 回読むのではなく
// 最初のリクエストで遅延して読む（`app/taskd/client.server.ts` の `getTaskdClient()`）ので、
// トークンファイルの中身は webServer の起動後（この spec の `beforeAll` が taskd を起動した後）でよい:
//
//   cd gui
//   TASKD_API_LISTEN=127.0.0.1:7810 scripts/taskd.sh fixture accounts
//   TASKD_GUI_BIND=127.0.0.1:7800 TASKD_API_URL=http://127.0.0.1:7810 TASKD_API_LISTEN=127.0.0.1:7810 \
//     TASKD_API_TOKEN_FILE="$(pwd)/.run/accounts/api.token" \
//     pnpm exec playwright test e2e/g8.spec.ts
//
// （`fixture` はファイルを用意するだけで taskd プロセスは起動しない。起動はこの spec の `beforeAll` が行う。
// multi-account / auth と同じ理由、docs/adr/0007 D1。）

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");
const TASKD_API_LISTEN = process.env.TASKD_API_LISTEN ?? "127.0.0.1:7810";

function sh(...args: string[]): string {
  return execFileSync(TASKD_SH, args, {
    cwd: REPO_ROOT,
    stdio: "pipe",
    env: { ...process.env, TASKD_API_LISTEN },
  }).toString();
}

test.describe("受け入れ条件: プロバイダ管理とアカウントのプール（fixture accounts）", () => {
  test.beforeAll(() => {
    try {
      sh("stop", "accounts");
    } catch {
      // 動いていなければ何もしない
    }
    sh("start", "accounts");
  });

  test.afterAll(() => {
    sh("stop", "accounts");
  });

  test("provider 追加 → reload → account_pool 表示、account 追加 → ログイン（誤り→成功）→ 確認 → 削除", async ({
    page,
  }) => {
    // --- プロバイダの追加（claude-code, account_pool on） ---
    await page.goto("/providers");
    await expect(page.getByTestId("providers-section")).toBeVisible();

    const addForm = page.getByTestId("provider-add-form");
    await addForm.locator("#add-id").fill("pool");
    await addForm.locator("#add-adapter").selectOption("claude-code");
    await addForm.locator("#add-account-pool").check();
    await page.getByTestId("provider-add-submit").click();

    // taskd の SSE（`daemon` イベント、fixture は tick_ms=200）を受けて自動で再検証が走ると action の
    // actionData（flash）は次のナビゲーションで消える（`app/hooks/useTaskdStream.ts`）ので、複数の
    // testid にまたがる文言は 1 回の `expect` で（間に retry の間隔を置かずに）まとめて確認する。
    await expect(page.getByTestId("provider-action-flash")).toContainText(/追加[\s\S]*反映しました/);

    const poolRow = page.locator('[data-testid="provider-row"][data-provider-id="pool"]');
    await expect(poolRow).toBeVisible();
    await expect(poolRow.getByTestId("provider-account-pool")).toBeVisible();

    // --- アカウントの追加 ---
    await page.goto("/accounts");
    await expect(page.getByTestId("accounts-page")).toBeVisible();

    await page.getByTestId("account-add-form").locator("#account-add-id").fill("a");
    await page.getByTestId("account-add-submit").click();
    await expect(page.getByTestId("flash-account-op")).toContainText("追加");

    const accountCard = page.locator('[data-testid="account-card"][data-account-id="a"]');
    await expect(accountCard).toBeVisible();
    await expect(accountCard.getByTestId("account-logged-in")).toHaveText("未ログイン");

    // --- ログイン開始 → URL 表示 → 誤りコード ---
    await accountCard.getByTestId("account-login-start").click();
    const loginUrl = accountCard.getByTestId("account-login-url");
    await expect(loginUrl).toBeVisible();
    const href = await loginUrl.getAttribute("href");
    expect(href).toMatch(/^https:\/\/claude\.example\.invalid\/cai\/oauth\/authorize\?/);
    await expect(loginUrl).toHaveAttribute("target", "_blank");
    await expect(loginUrl).toHaveAttribute("rel", "noreferrer noopener");

    await accountCard.getByTestId("account-login-code").fill("wrong-code");
    await accountCard.getByTestId("account-login-submit").click();
    await expect(page.getByTestId("flash")).toHaveAttribute("data-flash-kind", "error");
    await expect(accountCard.getByTestId("account-logged-in")).toHaveText("未ログイン");

    // --- もう一度ログイン開始 → 正しいコード ---
    await accountCard.getByTestId("account-login-start").click();
    await expect(accountCard.getByTestId("account-login-url")).toBeVisible();
    await accountCard.getByTestId("account-login-code").fill("good-code");
    await accountCard.getByTestId("account-login-submit").click();
    await expect(page.getByTestId("flash-account-login-result")).toHaveText("ok");
    await expect(accountCard.getByTestId("account-logged-in")).toHaveText("ログイン済み");

    // --- 残量確認: 5 時間枠 42% / 週次枠 18% ---
    await accountCard.getByTestId("account-check").click();
    await expect(page.getByTestId("flash-account-check-result")).toHaveText("ok");
    await expect(accountCard.getByTestId("account-usage-five-hour")).toContainText("42%");
    await expect(accountCard.getByTestId("account-usage-seven-day")).toContainText("18%");

    // --- アカウントの削除 ---
    const deleteAccountDetails = accountCard.locator("details", { hasText: "削除" });
    await deleteAccountDetails.locator("summary").click();
    await deleteAccountDetails.getByTestId("account-delete").click();
    await expect(page.getByTestId("flash-account-op")).toContainText("削除");
    await expect(page.locator('[data-testid="account-card"][data-account-id="a"]')).toHaveCount(0);

    // --- プロバイダの削除 ---
    await page.goto("/providers");
    const deleteProviderDetails = page
      .locator('[data-testid="provider-row"][data-provider-id="pool"]')
      .locator("details", { hasText: "削除" });
    await deleteProviderDetails.locator("summary").click();
    await deleteProviderDetails.getByTestId("provider-delete").click();
    await expect(page.getByTestId("provider-action-flash")).toContainText(/削除[\s\S]*反映しました/);
    await expect(page.locator('[data-testid="provider-row"][data-provider-id="pool"]')).toHaveCount(0);
  });
});
