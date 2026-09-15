import { test as base, expect, type Page } from "@playwright/test";

// 全 e2e spec が `@playwright/test` の代わりにここから `test` / `expect` を import する。
// docs/DESIGN.md §10 Phase G5 受け入れ条件 3「Playwright の全シナリオでコンソールに CSP 違反が 0 件」を
// spec ごとに書かせるのではなく、auto fixture（`{ auto: true }`）として一度だけ実装し全 spec に効かせる
// （docs/adr/0008-g5-decisions.md D7）。
//
// CSP 違反はブラウザのコンソールに `Content Security Policy` / `Content-Security-Policy` を含む
// エラーとして出る（Chromium は "violates the following Content Security Policy directive" という
// 文言も使う）。`page.on("console")` と `page.on("pageerror")` の両方を張り、`context.on("page")` で
// テスト中に新しく開かれたページ（`window.open` 等）にも同じ監視を付ける。テスト本体が終わった後
// （このフィクスチャの `use()` の後、つまり teardown）で集めたメッセージが 0 件であることを assert する。
// 0 件でなければ違反の全文がテスト失敗のメッセージに出る。

const CSP_PATTERNS = [/Content Security Policy/i, /Content-Security-Policy/i, /violates the following/i];

function isCspMessage(text: string): boolean {
  return CSP_PATTERNS.some((pattern) => pattern.test(text));
}

// biome-ignore lint/suspicious/noConfusingVoidType: Playwright の no-value fixture の慣例（`void` なら `use()` を引数無しで呼べる）
export const test = base.extend<{ cspGuard: void }>({
  cspGuard: [
    async ({ page, context }, use) => {
      const violations: string[] = [];

      const attach = (p: Page) => {
        p.on("console", (msg) => {
          if (isCspMessage(msg.text())) violations.push(msg.text());
        });
        p.on("pageerror", (err) => {
          if (isCspMessage(err.message)) violations.push(err.message);
        });
      };

      attach(page);
      context.on("page", attach);

      await use();

      expect(violations).toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
