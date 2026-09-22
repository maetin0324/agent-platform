import { normalizeScope, scopeForNode } from "./console";

/**
 * Console composer（入力欄）をレイアウトレベル（`~/root.tsx`）へ移す配線（ADR-0057）。
 * どの画面が composer を出すか・どの scope で送るかは、Console を表示する 2 つのルート
 * （`~/routes/home.tsx` = `/`、`~/routes/org.$id.tsx` = `/org/:id`）の**パスだけ**から決まる
 * （celeris への問い合わせなしに同期的に決められる。SSR でも同じ結果になる）。
 * ここは純粋関数だけ（HTTP も React も持ち込まない。`~/lib/console.ts` と同じ方針。G10-U1）。
 */

/** `/org/:id`（1 セグメントだけ。`/org` 自体・`/org/x/y` は対象外）にだけマッチする。 */
const ORG_NODE_PATTERN = /^\/org\/([^/]+)$/;

/**
 * モバイル composer の実高さがまだ測れていないとき（`ConsoleComposerContext` の `mobileHeight` が
 * `null`。SSR・ハイドレーション直後）に使うフォールバック（px）。`~/components/Console.tsx` の
 * `console-input-spacer` の既定値（`h-52` = 13rem = 208px、Phase 72 から）と同じ値を使う
 * （composer の実測値はこれより小さいのが通常なので、フォールバックとして十分に大きい）。
 */
export const DEFAULT_MOBILE_COMPOSER_HEIGHT_PX = 208;

/**
 * この pathname で Console composer を出すか。`/`（home）と `/org/:id`（org-node）だけ。
 * `/org/secretary` は 302 で終わる resource route（何もレンダーしない）なので、ここでは判定不要
 * （実際にブラウザの pathname がこれになることは無い）。
 */
export function isConsoleComposerPathname(pathname: string): boolean {
  return pathname === "/" || ORG_NODE_PATTERN.test(pathname);
}

/**
 * この pathname・検索パラメータでの Console の既定 scope（`~/routes/home.tsx` の `normalizeScope`、
 * `~/routes/org.$id.tsx` の `scopeForNode` と同じ規則）。composer の「この案件の文脈で話す」バッジと
 * `POST /console/instruct` の既定 scope（`~/lib/console.ts::buildInstructBody` の第 3 引数）に使う。
 * `/`（scope=all）は `null` を返す（Phase 92 より前の `~/components/Console.tsx` の
 * `defaultScope={parsedScope.kind === "all" ? null : scope}` と同じ挙動を保つ。celeris の
 * `POST /console/instruct` は `scope: "all"` も `scope` 省略も同じ意味〈CoS 宛て〉に扱うので機能的な差は
 * 無いが、既存の挙動〈`scope` フィールド自体を送らない〉をそのまま保つ）。composer の対象パスでなければ
 * `null`。
 */
export function consoleComposerScopeForLocation(pathname: string, search: string): string | null {
  if (pathname === "/") {
    const scope = normalizeScope(new URLSearchParams(search).get("scope"));
    return scope === "all" ? null : scope;
  }
  const match = ORG_NODE_PATTERN.exec(pathname);
  if (match) return scopeForNode(decodeURIComponent(match[1]));
  return null;
}

/**
 * Phase 102（本番不具合の修正）: `POST /console/instruct` の送信先（`fetcher.submit` の `action`）。
 * `pathname` だけを受ける（末尾スラッシュ・`?scope=` を含む location 全体は考えない）。
 *
 * `/`（`~/routes/home.tsx`）は**インデックスルート**。React Router では素の `action: "/"` はインデックス
 * ルート自身ではなく親（`root`）に解決され、`root` は action を持たないため 405 になる
 * （本番で実際に起きた不具合。`ADR-0057` 追記参照）。インデックスルート自身へ送るには `"/?index"` の形が要る
 * （React Router の仕様）。`/org/:id`（`~/routes/org.$id.tsx`）は非インデックスなのでそのまま。
 */
export function consoleComposerActionFor(pathname: string): string {
  return pathname === "/" ? "/?index" : pathname;
}
