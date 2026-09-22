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
 *
 * ADR-0055 D2 ラウンド 20（Phase 96、P-G46-1）: `/accounts` の cooldown 表示が、mock fixture が
 * cooldown を「常に未来」にするために使う固定日時（2030-01-01）のせいで「1196日20時間」のような
 * 非現実的な桁数になっていた。実運用の cooldown は分〜時間のオーダーだが、`resets_at` など他の値が
 * 稀に長期化した場合にも読めるよう、`formatDuration` に月・年の単位を足した（30 日以上は月、365 日
 * 以上は年。どちらも上位 2 単位までの規律は変えていない）。
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

/** 1 か月・1 年とみなす日数（暦月・閏年の厳密な計算はしない。表示の読みやすさのための概算）。 */
const DAYS_PER_MONTH = 30;
const DAYS_PER_YEAR = 365;

/**
 * 期間（経過・残り時間）を日本語で、上位 2 単位までに丸めて表示する
 * （例: `"45秒"`、`"3分12秒"`、`"1時間12分"`、`"2日3時間"`、`"2か月3日"`、`"3年3か月"`）。負値は `"0秒"`。
 * 30 日以上は月、365 日以上は年に切り替える（P-G46-1: 日のままだと 3 桁を超えて非現実的に見える）。
 */
export function formatDuration(totalSeconds: number): string {
  const { days, hours, minutes, seconds } = splitDuration(totalSeconds);
  if (days >= DAYS_PER_YEAR) {
    const years = Math.floor(days / DAYS_PER_YEAR);
    const remMonths = Math.floor((days % DAYS_PER_YEAR) / DAYS_PER_MONTH);
    return remMonths > 0 ? `${years}年${remMonths}か月` : `${years}年`;
  }
  if (days >= DAYS_PER_MONTH) {
    const months = Math.floor(days / DAYS_PER_MONTH);
    const remDays = days % DAYS_PER_MONTH;
    return remDays > 0 ? `${months}か月${remDays}日` : `${months}か月`;
  }
  if (days > 0) return hours > 0 ? `${days}日${hours}時間` : `${days}日`;
  if (hours > 0) return minutes > 0 ? `${hours}時間${minutes}分` : `${hours}時間`;
  if (minutes > 0) return seconds > 0 ? `${minutes}分${seconds}秒` : `${minutes}分`;
  return `${seconds}秒`;
}
