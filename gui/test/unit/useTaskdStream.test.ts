import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createStreamController } from "~/hooks/useTaskdStream";

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("createStreamController", () => {
  it("calls revalidate once, debounceMs after a single notify", () => {
    const revalidate = vi.fn();
    const controller = createStreamController(revalidate, 10);

    controller.notify("task.event");
    expect(revalidate).not.toHaveBeenCalled();

    vi.advanceTimersByTime(10);
    expect(revalidate).toHaveBeenCalledTimes(1);
  });

  it("coalesces notify calls within one window into a single revalidate", () => {
    const revalidate = vi.fn();
    const controller = createStreamController(revalidate, 10);

    controller.notify("task.event");
    vi.advanceTimersByTime(3);
    controller.notify("daemon");
    vi.advanceTimersByTime(3);
    controller.notify("reset");
    vi.advanceTimersByTime(4);

    expect(revalidate).toHaveBeenCalledTimes(1);
  });

  it("keeps firing at least once per window under a continuous stream (does not livelock)", () => {
    // 実機バグの再現: `daemon` イベントが debounceMs より短い間隔で届き続けても、
    // タイマーをリセットし続けて revalidate が一生呼ばれない、ということがあってはならない
    // （fixture の tick_ms=200ms < debounceMs=250ms で実際に発生した）。
    const revalidate = vi.fn();
    const controller = createStreamController(revalidate, 10);

    for (let elapsed = 0; elapsed < 55; elapsed += 4) {
      controller.notify("daemon");
      vi.advanceTimersByTime(4);
    }

    expect(revalidate.mock.calls.length).toBeGreaterThanOrEqual(4);
  });

  it("stops calling revalidate after dispose", () => {
    const revalidate = vi.fn();
    const controller = createStreamController(revalidate, 10);

    controller.notify("task.event");
    controller.dispose();
    vi.advanceTimersByTime(50);

    expect(revalidate).not.toHaveBeenCalled();
  });
});
