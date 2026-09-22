import type { ChangeEvent } from "react";
import { useSyncExternalStore } from "react";
import { LocalTime } from "~/components/LocalTime";
import { hintClass, labelClass, selectClass } from "~/components/ui/form";
import { getClockSnapshot, getServerClockSnapshot, subscribeClock } from "~/lib/clock";
import {
  getResolvedTimeZoneSnapshot,
  getServerTimeZonePreferenceSnapshot,
  getServerTimeZoneSnapshot,
  getTimeZonePreferenceSnapshot,
  subscribeTimeZonePreference,
  TIME_ZONE_OPTIONS,
  writeTimeZonePreference,
} from "~/lib/time-zone";
import { cn } from "~/lib/utils";

/**
 * 「表示タイムゾーン」設定（ADR-0055 ラウンド 14、U-G37-1 の受け入れ条件 2）。モバイルの「その他」
 * シート（`~/root.tsx`）と `/help`（`~/routes/help.tsx`）の両方から使う共通部品。
 *
 * `localStorage` の読み書き自体は `~/lib/time-zone.ts` に閉じている（ここでは呼ぶだけ）。`<select>`
 * の値も `useSyncExternalStore` 経由で読む: サーバ・ハイドレーション前は常に `"auto"`（決定的）を見せ、
 * ハイドレーション後に実際に保存されている設定へ切り替える（このコンポーネント自身もハイドレーション
 * 不一致を起こさない設計。`~/components/LocalTime.tsx` と同じ考え方）。
 *
 * 「いま何時と表示されるか」の例を `LocalTime`（`mode="datetime"`）で添える。設定を変えた効果がその場で
 * 分かるようにするため（celeris への問い合わせは無い、GUI だけの見た目の設定）。
 */
export function TimeZonePreference({ className }: { className?: string }) {
  const preference = useSyncExternalStore(
    subscribeTimeZonePreference,
    getTimeZonePreferenceSnapshot,
    getServerTimeZonePreferenceSnapshot,
  );
  const resolved = useSyncExternalStore(
    subscribeTimeZonePreference,
    getResolvedTimeZoneSnapshot,
    getServerTimeZoneSnapshot,
  );
  const nowMs = useSyncExternalStore(subscribeClock, getClockSnapshot, getServerClockSnapshot);

  function onChange(event: ChangeEvent<HTMLSelectElement>) {
    writeTimeZonePreference(event.target.value);
  }

  return (
    <div className={cn("space-y-1.5", className)} data-testid="time-zone-preference">
      <label className={labelClass} htmlFor="viewer-time-zone">
        表示タイムゾーン
      </label>
      <select
        id="viewer-time-zone"
        name="viewer-time-zone"
        data-testid="time-zone-select"
        className={selectClass}
        value={preference}
        onChange={onChange}
      >
        {TIME_ZONE_OPTIONS.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
      <p className={hintClass} data-testid="time-zone-preview">
        いま「{resolved}」として表示しています（この端末・このブラウザだけの設定）。現在時刻の例:{" "}
        {nowMs === null ? "…" : <LocalTime iso={new Date(nowMs).toISOString()} mode="datetime" />}
      </p>
    </div>
  );
}
