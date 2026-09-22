/**
 * 視聴者ごとの「表示タイムゾーン」設定（ADR-0055 ラウンド 14、U-G37-1 の受け入れ条件 2）。
 * `auto`（既定。ブラウザが検出したタイムゾーンに従う）か、固定の IANA タイムゾーン名。
 *
 * `localStorage` は「その閲覧者だけの見た目の好み」（celeris への問い合わせや判断ロジックには一切
 * 関わらない。gui/CLAUDE.md が禁じる「判断ロジックの再実装」ではない、純粋な表示設定）なので、
 * 読み書きは必ず `typeof window` チェック + try/catch で包む（SSR・プライベートブラウジング・
 * ストレージ無効化のいずれでも例外にしない。失敗したら「auto」として扱うか、書き込みを諦めるだけ）。
 */

export const AUTO_TIME_ZONE = "auto";
const STORAGE_KEY = "celeris:viewer-time-zone";
/** 同じタブ内の他のコンポーネントに変更を伝えるためだけの自作イベント（`storage` はタブをまたいだ変更にしか飛ばない）。 */
const CHANGE_EVENT = "celeris:viewer-time-zone-changed";

/** 「その他」シート・`/help` の `<select>` に出す既知の候補。自由入力ではなくここから選ぶ。 */
export const TIME_ZONE_OPTIONS: { value: string; label: string }[] = [
  { value: AUTO_TIME_ZONE, label: "自動（ブラウザの設定に従う）" },
  { value: "UTC", label: "UTC" },
  { value: "Asia/Tokyo", label: "日本（Asia/Tokyo）" },
  { value: "America/Chicago", label: "米国中部（America/Chicago）" },
  { value: "America/New_York", label: "米国東部（America/New_York）" },
  { value: "America/Los_Angeles", label: "米国太平洋（America/Los_Angeles）" },
  { value: "Europe/London", label: "英国（Europe/London）" },
];

export function isKnownTimeZoneValue(value: string): boolean {
  return TIME_ZONE_OPTIONS.some((o) => o.value === value);
}

/** 保存された設定を読む。無い・壊れている・読めない環境では `"auto"`。 */
export function readTimeZonePreference(): string {
  if (typeof window === "undefined") return AUTO_TIME_ZONE;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    return raw && raw.trim() !== "" ? raw : AUTO_TIME_ZONE;
  } catch {
    return AUTO_TIME_ZONE;
  }
}

/** 設定を保存する。`"auto"` はキー自体を消す（既定値を明示的に持たない）。書けなければ静かに諦める。 */
export function writeTimeZonePreference(value: string): void {
  if (typeof window === "undefined") return;
  try {
    if (value === AUTO_TIME_ZONE) window.localStorage.removeItem(STORAGE_KEY);
    else window.localStorage.setItem(STORAGE_KEY, value);
  } catch {
    // 保存できない環境（プライベートモード等）。見た目の好みが保存されないだけで致命的ではない。
  }
  try {
    window.dispatchEvent(new Event(CHANGE_EVENT));
  } catch {
    // ignore
  }
}

/** ブラウザが検出した実行環境のタイムゾーン。取得できなければ `"UTC"`。 */
export function detectBrowserTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

/** 設定値（`"auto"` または固定の IANA 名）から、実際に表示に使うタイムゾーンを決める。 */
export function resolveTimeZone(preference: string): string {
  return preference === AUTO_TIME_ZONE ? detectBrowserTimeZone() : preference;
}

/**
 * `useSyncExternalStore` の subscribe。同じタブ内の変更（`writeTimeZonePreference`）と、他のタブでの
 * 変更（ブラウザ標準の `storage` イベント）の両方で再描画させる。
 */
export function subscribeTimeZonePreference(listener: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  window.addEventListener(CHANGE_EVENT, listener);
  window.addEventListener("storage", listener);
  return () => {
    window.removeEventListener(CHANGE_EVENT, listener);
    window.removeEventListener("storage", listener);
  };
}

/** クライアントのスナップショット: 保存されている設定を解決した実際のタイムゾーン。 */
export function getResolvedTimeZoneSnapshot(): string {
  return resolveTimeZone(readTimeZonePreference());
}

/** クライアントのスナップショット: 保存されている設定そのもの（`"auto"` または固定の IANA 名）。設定 UI 用。 */
export function getTimeZonePreferenceSnapshot(): string {
  return readTimeZonePreference();
}

/** 設定 UI のサーバ・ハイドレーション前のスナップショット。常に `"auto"`（決定的）。 */
export function getServerTimeZonePreferenceSnapshot(): string {
  return AUTO_TIME_ZONE;
}

/**
 * サーバ・ハイドレーション前は常に `"UTC"`（決定的。GUI サーバーのホストのタイムゾーンにも、
 * 視聴者のブラウザのタイムゾーンにも依存しない）。`~/components/LocalTime.tsx` が使う。
 */
export function getServerTimeZoneSnapshot(): string {
  return "UTC";
}
