import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "./test";

// docs/adr/0025-codex-accounts-in-the-pool.md（taskd 側）D5/D6、gui/docs/adr/0012 D3 の拡張。
// codex アカウント（デバイス認証: URL + user_code をこの画面に出すだけで、コードは画面に貼り戻さない）を、
// e2e/g8.spec.ts と同じ `scripts/taskd.sh fixture accounts`（claude-code / codex 両方のスタブを持つ）に対して確認する。
//
// このファイルも人間の本番 taskd/GUI（127.0.0.1:7710 / 0.0.0.0:7700）と衝突しないポートを使う想定
// （e2e/g8.spec.ts のコメントと同じ手順）:
//
//   cd gui
//   TASKD_API_LISTEN=127.0.0.1:7810 scripts/taskd.sh fixture accounts
//   TASKD_GUI_BIND=127.0.0.1:7800 TASKD_API_URL=http://127.0.0.1:7810 TASKD_API_LISTEN=127.0.0.1:7810 \
//     TASKD_API_TOKEN_FILE="$(pwd)/.run/accounts/api.token" \
//     pnpm exec playwright test e2e/g9.spec.ts

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

test.describe("受け入れ条件: codex アカウント（デバイス認証、fixture accounts）", () => {
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

  test("provider 追加（adapter=codex）→ account 追加（adapter=codex）→ デバイス認証ログイン → 確認 → 削除", async ({
    page,
  }) => {
    // --- プロバイダの追加（codex, account_pool on） ---
    await page.goto("/providers");
    await expect(page.getByTestId("providers-section")).toBeVisible();

    const addForm = page.getByTestId("provider-add-form");
    await addForm.locator("#add-id").fill("poolcodex");
    await addForm.locator("#add-adapter").selectOption("codex");
    await addForm.locator("#add-account-pool").check();
    await page.getByTestId("provider-add-submit").click();
    await expect(page.getByTestId("provider-action-flash")).toContainText(/追加[\s\S]*反映しました/);

    const poolRow = page.locator('[data-testid="provider-row"][data-provider-id="poolcodex"]');
    await expect(poolRow).toBeVisible();
    await expect(poolRow.getByTestId("provider-account-pool")).toBeVisible();

    // --- アカウントの追加（adapter=codex） ---
    await page.goto("/accounts");
    await expect(page.getByTestId("accounts-page")).toBeVisible();

    const accountAddForm = page.getByTestId("account-add-form");
    await accountAddForm.locator("#account-add-id").fill("co");
    await accountAddForm.getByTestId("account-add-adapter").selectOption("codex");
    await page.getByTestId("account-add-submit").click();
    await expect(page.getByTestId("flash-account-op")).toContainText("追加");

    const accountCard = page.locator(
      '[data-testid="account-card"][data-account-id="co"][data-account-adapter="codex"]',
    );
    await expect(accountCard).toBeVisible();
    await expect(accountCard.getByTestId("account-adapter")).toHaveText("codex");
    await expect(accountCard.getByTestId("account-logged-in")).toHaveText("未ログイン");

    // --- ログイン開始: kind=device_code、URL + user_code を表示、コード入力欄は無い ---
    await accountCard.getByTestId("account-login-start").click();
    await expect(accountCard.getByTestId("account-login-kind")).toHaveText("device_code");

    const loginUrl = accountCard.getByTestId("account-login-url");
    await expect(loginUrl).toBeVisible();
    const href = await loginUrl.getAttribute("href");
    expect(href).toBe("https://auth.openai.com/codex/device");
    await expect(loginUrl).toHaveAttribute("target", "_blank");
    await expect(loginUrl).toHaveAttribute("rel", "noreferrer noopener");

    const userCode = accountCard.getByTestId("account-login-user-code");
    await expect(userCode).toBeVisible();
    await expect(userCode).toHaveText("ABCD-EFGHI");

    // codex はデバイス認証だけで完結する（ADR-0025 D5）ので、コード入力欄は無い
    await expect(accountCard.getByTestId("account-login-code")).toHaveCount(0);
    await expect(accountCard.getByTestId("account-login-submit")).toHaveCount(0);

    // --- 完了はスタブの子プロセスの終了待ち（~1 秒後に auth.json ができる）。この画面が自動で更新される ---
    await expect(accountCard.getByTestId("account-logged-in")).toHaveText("ログイン済み", { timeout: 20_000 });
    // ログインが終わればパネルは自動的に閉じる
    await expect(accountCard.getByTestId("account-login-user-code")).toHaveCount(0);

    // --- 残量確認: primary 30% / secondary 12%（window_minutes で短い枠・長い枠に振り分け、ADR-0025 D3） ---
    await accountCard.getByTestId("account-check").click();
    await expect(page.getByTestId("flash-account-check-result")).toHaveText("ok");
    await expect(accountCard.getByTestId("account-usage-five-hour")).toContainText("30%");
    await expect(accountCard.getByTestId("account-usage-seven-day")).toContainText("12%");

    // --- アカウントの削除 ---
    const deleteAccountDetails = accountCard.locator("details", { hasText: "削除" });
    await deleteAccountDetails.locator("summary").click();
    await deleteAccountDetails.getByTestId("account-delete").click();
    await expect(page.getByTestId("flash-account-op")).toContainText("削除");
    await expect(
      page.locator('[data-testid="account-card"][data-account-id="co"][data-account-adapter="codex"]'),
    ).toHaveCount(0);

    // --- プロバイダの削除 ---
    await page.goto("/providers");
    const deleteProviderDetails = page
      .locator('[data-testid="provider-row"][data-provider-id="poolcodex"]')
      .locator("details", { hasText: "削除" });
    await deleteProviderDetails.locator("summary").click();
    await deleteProviderDetails.getByTestId("provider-delete").click();
    await expect(page.getByTestId("provider-action-flash")).toContainText(/削除[\s\S]*反映しました/);
    await expect(page.locator('[data-testid="provider-row"][data-provider-id="poolcodex"]')).toHaveCount(0);
  });
});
