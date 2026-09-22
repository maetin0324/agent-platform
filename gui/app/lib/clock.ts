/**
 * 「毎分すくなくとも 1 回」更新する共有の時計（ADR-0055 ラウンド 14、U-G37-1 の受け入れ条件 3）。
 * `~/components/LocalTime.tsx` を画面にいくつ並べても `setInterval` は 1 本だけ（このモジュールに
 * 状態を集約する。要素ごとに interval を持たない）。タブが非表示の間は止める（無駄な再描画をしない。
 * 表示に戻ったら即座に 1 回進めてから再開する）。
 *
 * サーバ（SSR）では `useSyncExternalStore` は `subscribeClock` を一切呼ばない（`getServerSnapshot` だけを
 * 使う）ので、`document` はここでしか触らない。それでも Node（vitest 含む）で読み込まれても例外にならない
 * よう、`document` の有無は毎回 `typeof` で確認する。
 */

const TICK_MS = 60_000;

let now: number | null = null;
let intervalId: ReturnType<typeof setInterval> | null = null;
let visibilityBound = false;
const listeners = new Set<() => void>();

function tick(): void {
  now = Date.now();
  for (const listener of listeners) listener();
}

function isVisible(): boolean {
  return typeof document === "undefined" || document.visibilityState !== "hidden";
}

function ensureRunning(): void {
  if (intervalId !== null || !isVisible()) return;
  intervalId = setInterval(tick, TICK_MS);
}

function stopRunning(): void {
  if (intervalId === null) return;
  clearInterval(intervalId);
  intervalId = null;
}

function bindVisibility(): void {
  if (visibilityBound || typeof document === "undefined") return;
  visibilityBound = true;
  document.addEventListener("visibilitychange", () => {
    if (isVisible()) {
      tick();
      ensureRunning();
    } else {
      stopRunning();
    }
  });
}

/**
 * `useSyncExternalStore` の subscribe。購読者が 0 になったら interval を止め、また誰かが購読したら
 * 再開する（画面遷移で `LocalTime` が全部アンマウントされている間はタイマーを持たない）。
 */
export function subscribeClock(listener: () => void): () => void {
  bindVisibility();
  listeners.add(listener);
  if (now === null) tick(); // 初回購読時に即座に「今」を持たせる（次の分まで待たせない）。
  ensureRunning();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) stopRunning();
  };
}

/** クライアントの現在のスナップショット。まだ 1 度も動いていなければ `null`。 */
export function getClockSnapshot(): number | null {
  return now;
}

/**
 * サーバ・ハイドレーション前は常に `null`（決定的）。`~/components/LocalTime.tsx` はこれを見て、
 * 実時計ではなく `fetchedAtIso`（loader が読み込んだ時刻）に基づく値へフォールバックする。
 */
export function getServerClockSnapshot(): number | null {
  return null;
}
