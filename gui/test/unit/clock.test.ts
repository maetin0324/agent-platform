import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getClockSnapshot, getServerClockSnapshot, subscribeClock } from "~/lib/clock";

/**
 * `~/lib/clock.ts`（ADR-0055 ラウンド 14、U-G37-1 の受け入れ条件 3: 「毎分すくなくとも 1 回」更新する
 * 共有の時計。`~/components/LocalTime.tsx` を何個並べても `setInterval` は 1 本だけ）。
 * モジュールスコープの単一ストアなので、各テストは自分で購読した分を必ず解除して次のテストに影響を
 * 残さない（`intervalId` が `null` に戻る）。
 */
describe("clock", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("getServerClockSnapshot は常に null（SSR・ハイドレーション前は決定的な値にフォールバックさせる）", () => {
    expect(getServerClockSnapshot()).toBeNull();
  });

  it("複数回購読しても setInterval は 1 本だけ（要素ごとに interval を持たない）", () => {
    const setIntervalSpy = vi.spyOn(globalThis, "setInterval");
    const unsub1 = subscribeClock(() => {});
    const unsub2 = subscribeClock(() => {});
    const unsub3 = subscribeClock(() => {});
    expect(setIntervalSpy).toHaveBeenCalledTimes(1);
    unsub1();
    unsub2();
    unsub3();
    setIntervalSpy.mockRestore();
  });

  it("購読すると即座に現在時刻を持つ（次の 1 分を待たせない）", () => {
    const unsub = subscribeClock(() => {});
    expect(getClockSnapshot()).not.toBeNull();
    unsub();
  });

  it("毎分すくなくとも 1 回、購読者に通知する", () => {
    const listener = vi.fn();
    const unsub = subscribeClock(listener);
    listener.mockClear();
    vi.advanceTimersByTime(60_000);
    expect(listener).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(60_000);
    expect(listener).toHaveBeenCalledTimes(2);
    unsub();
  });

  it("最後の購読者が抜けると interval を止め、再購読すればまた動く", () => {
    const clearIntervalSpy = vi.spyOn(globalThis, "clearInterval");
    const setIntervalSpy = vi.spyOn(globalThis, "setInterval");
    const unsub = subscribeClock(() => {});
    unsub();
    expect(clearIntervalSpy).toHaveBeenCalledTimes(1);

    const listener = vi.fn();
    const unsub2 = subscribeClock(listener);
    expect(setIntervalSpy).toHaveBeenCalledTimes(2); // 1 度止まって、また 1 本張り直す
    listener.mockClear();
    vi.advanceTimersByTime(60_000);
    expect(listener).toHaveBeenCalledTimes(1);
    unsub2();

    clearIntervalSpy.mockRestore();
    setIntervalSpy.mockRestore();
  });
});
