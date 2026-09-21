/**
 * 表示の省略（ADR-0055 D2）。id（ULID）・sha・パスは `font-mono text-xs break-all` にした上で、
 * 表示だけ短くする（全文は呼び出し側が `title` 属性やコピー用の要素に渡す。celeris に送る値は
 * 変えない — 表示専用の純関数）。
 */

/** 長い id / sha を末尾 `tailLength` 字に省略する（`…` を先頭に付ける）。短ければそのまま。 */
export function shortId(id: string, tailLength = 8): string {
  if (id.length <= tailLength + 1) return id;
  return `…${id.slice(-tailLength)}`;
}

/** 長い見出し・題名を `maxLength` 字で省略する（`…` を末尾に付ける）。短ければそのまま。 */
export function truncateLabel(text: string, maxLength = 40): string {
  if (text.length <= maxLength) return text;
  if (maxLength <= 1) return "…";
  return `${text.slice(0, maxLength - 1)}…`;
}
