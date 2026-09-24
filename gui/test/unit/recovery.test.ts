import { describe, expect, it, vi } from "vitest";
import {
  AUTO_RETRY_DELAYS_MS,
  CHUNK_RELOAD_KEY,
  createResumeGate,
  isChunkLoadError,
  isTransientStatus,
  nextRetryDelay,
  shouldReloadForChunkError,
} from "~/lib/recovery";

describe("nextRetryDelay", () => {
  it("returns the backoff delays in order, then null", () => {
    AUTO_RETRY_DELAYS_MS.forEach((d, i) => {
      expect(nextRetryDelay(i)).toBe(d);
    });
    expect(nextRetryDelay(AUTO_RETRY_DELAYS_MS.length)).toBeNull();
  });
});

describe("createResumeGate", () => {
  it("coalesces resume events within the min gap and fires again after it", () => {
    let t = 10_000;
    const run = vi.fn();
    const gate = createResumeGate(run, 1_000, () => t);
    expect(gate.fire()).toBe(true);
    t += 200; // visibilitychange の直後に pageshow / focus が来る
    expect(gate.fire()).toBe(false);
    t += 1_000;
    expect(gate.fire()).toBe(true);
    expect(run).toHaveBeenCalledTimes(2);
  });
});

describe("isChunkLoadError", () => {
  it("recognizes stale-build chunk failures across browsers", () => {
    expect(isChunkLoadError(new TypeError("Failed to fetch dynamically imported module: /assets/a.js"))).toBe(true);
    expect(isChunkLoadError(new Error("error loading dynamically imported module"))).toBe(true);
    expect(isChunkLoadError(new Error("Importing a module script failed."))).toBe(true);
    expect(isChunkLoadError(new Error("boom"))).toBe(false);
  });
});

describe("shouldReloadForChunkError", () => {
  const mem = () => {
    const m = new Map<string, string>();
    return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
  };
  it("reloads once, not again within the window (no reload loop), and again afterwards", () => {
    const s = mem();
    expect(shouldReloadForChunkError(s, 1_000_000)).toBe(true);
    expect(s.getItem(CHUNK_RELOAD_KEY)).toBe("1000000");
    expect(shouldReloadForChunkError(s, 1_010_000)).toBe(false);
    expect(shouldReloadForChunkError(s, 1_100_000)).toBe(true);
  });
  it("does not reload when storage is unavailable", () => {
    const broken = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {},
    };
    expect(shouldReloadForChunkError(broken)).toBe(false);
  });
});

describe("isTransientStatus", () => {
  it("5xx / 408 / 429 / 不明は再試行対象、404 / 401 / 400 は対象外", () => {
    for (const s of [500, 503, 504, 408, 429, undefined]) expect(isTransientStatus(s)).toBe(true);
    for (const s of [400, 401, 403, 404, 409]) expect(isTransientStatus(s)).toBe(false);
  });
});
