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

/**
 * `WorkerFinished.outcome`（`done: <要約>` / `error: <理由>` のように接頭辞 + 長文）を、
 * ステータス名（短い語）と本文に分ける。ステータス欄にはステータス名だけを出し、本文は別の領域に出す。
 * 接頭辞が無ければ全体がステータス名。
 */
export function splitOutcome(outcome: string): { status: string; text: string | null } {
  const i = outcome.indexOf(": ");
  if (i <= 0) return { status: outcome, text: null };
  const text = outcome.slice(i + 2);
  return { status: outcome.slice(0, i), text: text === "" ? null : text };
}
