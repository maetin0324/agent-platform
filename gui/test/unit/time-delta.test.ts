import { describe, expect, it } from "vitest";
import { formatDuration, secondsBetween } from "~/lib/time-delta";

describe("secondsBetween", () => {
  it("returns the positive difference in seconds", () => {
    expect(secondsBetween("2026-09-15T00:00:00Z", "2026-09-15T00:00:05Z")).toBe(5);
  });

  it("returns a negative value when to is before from", () => {
    expect(secondsBetween("2026-09-15T00:00:05Z", "2026-09-15T00:00:00Z")).toBe(-5);
  });
});

describe("formatDuration", () => {
  it("formats sub-minute durations as seconds", () => {
    expect(formatDuration(45)).toBe("45s");
  });

  it("formats durations over a minute as m/s", () => {
    expect(formatDuration(192)).toBe("3m12s");
  });

  it("clamps negative durations to 0s", () => {
    expect(formatDuration(-5)).toBe("0s");
  });
});
