/**
 * `/daemon` の in_flight 経過時間・`/providers` / `/accounts` の cooldown 残り時間、`~/lib/reports.ts::
 * relativeTimeLabel`（Console・タイムライン・`/approvals` 等の「n 前」表示）の基礎になる純関数群。
 * celeris の値（`since` / `until` / 各種 `*_at`）の単純な差分表示であり、判断ロジックの再実装ではない。
 *
 * ADR-0055 D2 ラウンド 7（Phase 75、P-G30-1）: 分・秒だけの表示（例: 3 日前が「4320m0s」）だったのを、
 * 時間・日を含む日本語表記に変えた。`formatDuration`（期間そのもの。経過・残り時間）は読みやすさのため
 * 上位 2 単位まで（例: `"1時間12分"`、`"2日3時間"`）、`relativeTimeLabel`（`~/lib/reports.ts`）は「n 前」
 * の相対表示なので 1 単位に丸める（直近は「たった今」）。どちらも絶対時刻は返さない（呼び出し側が
 * `title` 属性に生の ISO 文字列を残す）。
 */
export function secondsBetween(fromIso: string, toIso: string): number {
  return (new Date(toIso).getTime() - new Date(fromIso).getTime()) / 1000;
}

export interface DurationParts {
  days: number;
  hours: number;
  minutes: number;
  seconds: number;
}

/** 経過秒数を日/時間/分/秒に分解する（負値は 0 として扱う。四捨五入してから分解）。 */
export function splitDuration(totalSeconds: number): DurationParts {
  const s = Math.max(0, Math.round(totalSeconds));
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const seconds = s % 60;
  return { days, hours, minutes, seconds };
}

/**
 * 期間（経過・残り時間）を日本語で、上位 2 単位までに丸めて表示する
 * （例: `"45秒"`、`"3分12秒"`、`"1時間12分"`、`"2日3時間"`）。負値は `"0秒"`。
 */
export function formatDuration(totalSeconds: number): string {
  const { days, hours, minutes, seconds } = splitDuration(totalSeconds);
  if (days > 0) return hours > 0 ? `${days}日${hours}時間` : `${days}日`;
  if (hours > 0) return minutes > 0 ? `${hours}時間${minutes}分` : `${hours}時間`;
  if (minutes > 0) return seconds > 0 ? `${minutes}分${seconds}秒` : `${minutes}分`;
  return `${seconds}秒`;
}
