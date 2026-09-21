import { describe, expect, it } from "vitest";
import { formatDuration, secondsBetween, splitDuration } from "~/lib/time-delta";

describe("secondsBetween", () => {
  it("returns the positive difference in seconds", () => {
    expect(secondsBetween("2026-09-15T00:00:00Z", "2026-09-15T00:00:05Z")).toBe(5);
  });

  it("returns a negative value when to is before from", () => {
    expect(secondsBetween("2026-09-15T00:00:05Z", "2026-09-15T00:00:00Z")).toBe(-5);
  });
});

describe("splitDuration", () => {
  it.each<[number, ReturnType<typeof splitDuration>]>([
    [0, { days: 0, hours: 0, minutes: 0, seconds: 0 }],
    [45, { days: 0, hours: 0, minutes: 0, seconds: 45 }],
    [192, { days: 0, hours: 0, minutes: 3, seconds: 12 }],
    [4320, { days: 0, hours: 1, minutes: 12, seconds: 0 }],
    [183600, { days: 2, hours: 3, minutes: 0, seconds: 0 }],
    [-5, { days: 0, hours: 0, minutes: 0, seconds: 0 }],
  ])("splits %d seconds", (totalSeconds, expected) => {
    expect(splitDuration(totalSeconds)).toEqual(expected);
  });
});

// ADR-0055 D2 ラウンド 7（Phase 75、P-G30-1）: 分・秒だけの表示（「4320m0s」等）を、上位 2 単位までの
// 日本語表記（時間・日を含む）に変えた。
describe("formatDuration", () => {
  it.each<[number, string]>([
    [0, "0秒"],
    [45, "45秒"], // 1 分未満は秒のみ
    [59, "59秒"],
    [60, "1分"], // ちょうど 1 分は秒を出さない
    [192, "3分12秒"], // 1 分以上 1 時間未満は分+秒
    [3600, "1時間"], // ちょうど 1 時間は分を出さない
    [4320, "1時間12分"], // 1 時間以上 1 日未満は時間+分
    [86400, "1日"], // ちょうど 1 日は時間を出さない
    [97200, "1日3時間"],
    [183600, "2日3時間"], // 1 日以上は日+時間（分・秒は丸める）
    [-5, "0秒"], // 負値は 0 として扱う
  ])("formats %d seconds as %s", (totalSeconds, expected) => {
    expect(formatDuration(totalSeconds)).toBe(expected);
  });
});
