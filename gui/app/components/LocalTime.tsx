import { useSyncExternalStore } from "react";
import { getClockSnapshot, getServerClockSnapshot, subscribeClock } from "~/lib/clock";
import { absoluteDateLabel, dateTimeLabel, relativeTimeLabel } from "~/lib/reports";
import { getResolvedTimeZoneSnapshot, getServerTimeZoneSnapshot, subscribeTimeZonePreference } from "~/lib/time-zone";

export type LocalTimeMode = "relative" | "absolute" | "datetime";

/**
 * タイムゾーンに安全な時刻表示（ADR-0055 ラウンド 14、U-G37-1）。
 *
 * サーバ（SSR）とハイドレーション直後のクライアントは**必ず同じ文字列**を描画する: `useSyncExternalStore`
 * の `getServerSnapshot` はタイムゾーンを常に `"UTC"`、「今」を常に `null`（→ `fetchedAtIso` にフォール
 * バック）にする。これはホストの実行環境にも視聴者のブラウザのタイムゾーンにも依存しないため、SSR と
 * 最初のクライアント描画が必ず一致する（React のハイドレーションが警告を出さない）。
 *
 * マウント後（`useSyncExternalStore` が購読し、クライアント側のスナップショットに切り替わったあと）
 * だけ、視聴者の解決済みタイムゾーン（`~/lib/time-zone.ts`。既定はブラウザ検出、設定すれば固定 IANA 名）
 * と、毎分すくなくとも 1 回更新される共有の時計（`~/lib/clock.ts`。要素ごとに `setInterval` を持たない。
 * 受け入れ条件 3）に差し替える。この切り替えは通常の状態更新（コミット後の再描画）であり、ハイドレー
 * ション自体は常にサーバと同じ内容で一致するので「Hydration failed」等の警告にはならない。
 *
 * 絶対時刻は常に `title`/`dateTime` 属性に生の ISO 文字列として残す（`~/lib/reports.ts` の規律をそのまま
 * 引き継ぐ）。`relativeTimeLabel`/`absoluteDateLabel` の直接呼び出しは JSX からは行わず、この
 * コンポーネント経由にする（`gui/CLAUDE.md`・ADR-0055 D2 の「絶対時刻は title に残す」規律を 1 箇所に
 * まとめ、取りこぼしを防ぐ）。
 */
export function LocalTime({
  iso,
  fetchedAtIso,
  mode = "relative",
  className,
  dataTestId,
}: {
  /** 表示する時刻（celeris が返す ISO 8601 文字列）。 */
  iso: string;
  /** 相対表示・絶対日付フォールバックの「今」の基準（loader が読み込んだ時刻）。`mode="datetime"` では未使用。 */
  fetchedAtIso?: string;
  mode?: LocalTimeMode;
  className?: string;
  dataTestId?: string;
}) {
  const timeZone = useSyncExternalStore(
    subscribeTimeZonePreference,
    getResolvedTimeZoneSnapshot,
    getServerTimeZoneSnapshot,
  );
  const nowMs = useSyncExternalStore(subscribeClock, getClockSnapshot, getServerClockSnapshot);
  const nowIso = nowMs === null ? (fetchedAtIso ?? iso) : new Date(nowMs).toISOString();

  let label: string;
  if (mode === "datetime") label = dateTimeLabel(iso, timeZone);
  else if (mode === "absolute") label = absoluteDateLabel(iso, nowIso, timeZone);
  else label = relativeTimeLabel(iso, nowIso, timeZone);

  return (
    <time dateTime={iso} title={iso} className={className} data-testid={dataTestId}>
      {label}
    </time>
  );
}
