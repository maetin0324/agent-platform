import { describe, expect, it } from "vitest";
import {
  AUTO_TIME_ZONE,
  detectBrowserTimeZone,
  getResolvedTimeZoneSnapshot,
  getServerTimeZonePreferenceSnapshot,
  getServerTimeZoneSnapshot,
  getTimeZonePreferenceSnapshot,
  isKnownTimeZoneValue,
  readTimeZonePreference,
  resolveTimeZone,
  subscribeTimeZonePreference,
  writeTimeZonePreference,
} from "~/lib/time-zone";

/**
 * `~/lib/time-zone.ts`（ADR-0055 ラウンド 14、U-G37-1 の受け入れ条件 2: 視聴者の「表示タイムゾーン」設定）。
 * このテストは vitest の `environment: "node"`（`window`/`document` が無い）で走るので、SSR と同じ
 * 「window が無い」経路を自然に確かめられる: 例外を投げず `"auto"`/`"UTC"` にフォールバックすること。
 */
describe("time-zone", () => {
  it("getServerTimeZoneSnapshot は常に UTC（SSR はホストのタイムゾーンに依存しない）", () => {
    expect(getServerTimeZoneSnapshot()).toBe("UTC");
  });

  it("getServerTimeZonePreferenceSnapshot は常に auto", () => {
    expect(getServerTimeZonePreferenceSnapshot()).toBe(AUTO_TIME_ZONE);
  });

  it("window が無い環境（SSR・この vitest の node 環境）では auto にフォールバックし、例外を投げない", () => {
    expect(readTimeZonePreference()).toBe(AUTO_TIME_ZONE);
    expect(() => writeTimeZonePreference("Asia/Tokyo")).not.toThrow();
    expect(() => subscribeTimeZonePreference(() => {})()).not.toThrow();
    expect(getTimeZonePreferenceSnapshot()).toBe(AUTO_TIME_ZONE);
  });

  it("resolveTimeZone: auto はブラウザ検出、固定値はそのまま", () => {
    expect(resolveTimeZone("Asia/Tokyo")).toBe("Asia/Tokyo");
    expect(resolveTimeZone(AUTO_TIME_ZONE)).toBe(detectBrowserTimeZone());
  });

  it("getResolvedTimeZoneSnapshot: window が無ければ auto → detectBrowserTimeZone() と同じ", () => {
    expect(getResolvedTimeZoneSnapshot()).toBe(detectBrowserTimeZone());
  });

  it("detectBrowserTimeZone: 例外を投げず、IANA タイムゾーン名らしき文字列を返す", () => {
    expect(typeof detectBrowserTimeZone()).toBe("string");
    expect(detectBrowserTimeZone().length).toBeGreaterThan(0);
  });

  it("isKnownTimeZoneValue: 候補一覧に無い値は false", () => {
    expect(isKnownTimeZoneValue(AUTO_TIME_ZONE)).toBe(true);
    expect(isKnownTimeZoneValue("Asia/Tokyo")).toBe(true);
    expect(isKnownTimeZoneValue("America/Chicago")).toBe(true);
    expect(isKnownTimeZoneValue("Mars/Olympus_Mons")).toBe(false);
  });
});
