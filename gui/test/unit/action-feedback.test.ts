import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * 監査 H1 の回帰テスト: **操作の失敗が 0.3 秒で消えない**こと。
 *
 * 原因は「ナビゲーション方式の `<Form method="post">` + `actionData`」で、SSE の `daemon` イベント
 * （celeris が tick ごとに無条件で流す）を受けた `revalidator.revalidate()` のたびに `actionData` が捨てられる。
 * 直し方は fetcher 方式（`useFetcher` の `data` は再検証では消えない）に寄せること。
 *
 * ここでは DOM を描画せず、**変更系のフォームを持つルートが `actionData` を使っていない**ことを
 * ソースから確かめる（DOM を描画する unit テストがこのリポジトリに無いため。G10-U1）。
 * `<Form method="get">`（絞り込み）は再検証と無関係なので対象外。
 */

const ROUTES_WITH_WRITE_FORMS = [
  "inbox.tsx",
  "projects.tsx",
  "projects.$id.tsx",
  "org.tsx",
  "approvals.tsx",
  "reports.tsx",
  "tasks.$id.tsx",
  "tasks.new.tsx",
  "plans.new.tsx",
  "daemon.tsx",
  "artifacts.tsx",
  // Phase G14（ADR-0040 D6）: 昇格の 202 / 409 は行に残り続ける必要がある（引き継ぎ中は
  // 2 秒ごとに再検証するので、`actionData` だとその都度消えてしまう）。
  "releases.tsx",
];

/** コメント（`//` と `/* *​/`）は落としてから見る（説明文で `actionData` に触れてよいように）。 */
function readRoute(name: string): string {
  const source = readFileSync(fileURLToPath(new URL(`../../app/routes/${name}`, import.meta.url)), "utf8");
  return source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

describe("操作の結果は fetcher に載せる（監査 H1）", () => {
  for (const name of ROUTES_WITH_WRITE_FORMS) {
    it(`${name} は actionData を使わない`, () => {
      const source = readRoute(name);
      expect(source).not.toContain("actionData");
    });
  }

  for (const name of ROUTES_WITH_WRITE_FORMS) {
    it(`${name} に <Form method="post"> が残っていない`, () => {
      const source = readRoute(name);
      expect(source).not.toMatch(/<Form\s+method="post"/);
      expect(source).not.toMatch(/<Form\n\s+method="post"/);
    });
  }
});
