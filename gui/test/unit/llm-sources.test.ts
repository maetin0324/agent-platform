import { describe, expect, it } from "vitest";
import {
  cooldownRemainingLabel,
  formatRemaining,
  forwardStatusWord,
  isAccountCoolingDown,
  sourceLabel,
  sourceStatusWord,
  tierLabel,
  tierResolutionLabel,
} from "~/lib/llm-sources";

describe("sourceLabel", () => {
  it("maps the two oauth pools to human labels", () => {
    expect(sourceLabel("claude-oauth")).toBe("Claude");
    expect(sourceLabel("codex-oauth")).toBe("Codex");
  });

  it("strips the openai-compatible: prefix", () => {
    expect(sourceLabel("openai-compatible:qwen")).toBe("qwen");
  });

  it("returns unknown ids unchanged", () => {
    expect(sourceLabel("something-else")).toBe("something-else");
    expect(sourceLabel("openai-compatible:")).toBe("openai-compatible:");
  });
});

describe("sourceStatusWord", () => {
  it("is disabled when the source itself is disabled, regardless of reachable", () => {
    expect(sourceStatusWord({ enabled: false, reachable: true })).toBe("disabled");
  });

  it("reflects the probe result for openai-compatible sources", () => {
    expect(sourceStatusWord({ enabled: true, reachable: true })).toBe("reachable");
    expect(sourceStatusWord({ enabled: true, reachable: false })).toBe("unreachable");
  });

  it("falls back to enabled for oauth pools (no reachable field)", () => {
    expect(sourceStatusWord({ enabled: true, reachable: undefined })).toBe("enabled");
    expect(sourceStatusWord({ enabled: true, reachable: null })).toBe("enabled");
  });

  it("never produces a badge with whitespace or more than 12 characters", () => {
    for (const input of [
      { enabled: false, reachable: true },
      { enabled: true, reachable: true },
      { enabled: true, reachable: false },
      { enabled: true, reachable: undefined },
    ]) {
      const word = sourceStatusWord(input);
      expect(word).not.toMatch(/\s/);
      expect(word.length).toBeLessThanOrEqual(12);
    }
  });
});

describe("formatRemaining", () => {
  it("renders a percentage rounded to the nearest integer", () => {
    expect(formatRemaining(0.62)).toBe("62%");
    expect(formatRemaining(1)).toBe("100%");
    expect(formatRemaining(0)).toBe("0%");
  });

  it("does not fabricate a value when it cannot be measured", () => {
    expect(formatRemaining(null)).toBe("不明");
    expect(formatRemaining(undefined)).toBe("不明");
    expect(formatRemaining(Number.NaN)).toBe("不明");
  });

  it("clamps out-of-range values instead of showing nonsense percentages", () => {
    expect(formatRemaining(1.5)).toBe("100%");
    expect(formatRemaining(-0.5)).toBe("0%");
  });
});

describe("tierResolutionLabel", () => {
  it("labels a resolved source", () => {
    expect(tierResolutionLabel("openai-compatible:qwen")).toBe("qwen");
    expect(tierResolutionLabel("claude-oauth")).toBe("Claude");
  });

  it("says there is no source when nothing resolved", () => {
    expect(tierResolutionLabel(null)).toBe("供給元なし");
    expect(tierResolutionLabel(undefined)).toBe("供給元なし");
  });
});

describe("tierLabel", () => {
  it("passes known tiers through and leaves unknown ones unchanged", () => {
    expect(tierLabel("frontier")).toBe("frontier");
    expect(tierLabel("cheap")).toBe("cheap");
    expect(tierLabel("mystery")).toBe("mystery");
  });
});

describe("cooldownRemainingLabel", () => {
  it("formats the remaining time when still cooling down", () => {
    expect(cooldownRemainingLabel(1_000, 940)).toBe("1分");
  });

  it("says it has expired once past the deadline", () => {
    expect(cooldownRemainingLabel(1_000, 1_000)).toBe("切れています");
    expect(cooldownRemainingLabel(1_000, 1_500)).toBe("切れています");
  });
});

describe("isAccountCoolingDown", () => {
  it("is true only while cooldown_until is in the future", () => {
    expect(isAccountCoolingDown({ cooldown_until: 1_000 }, 500)).toBe(true);
    expect(isAccountCoolingDown({ cooldown_until: 1_000 }, 1_000)).toBe(false);
    expect(isAccountCoolingDown({ cooldown_until: 1_000 }, 1_500)).toBe(false);
  });

  it("is false when there is no cooldown", () => {
    expect(isAccountCoolingDown({ cooldown_until: null }, 500)).toBe(false);
    expect(isAccountCoolingDown({ cooldown_until: undefined }, 500)).toBe(false);
  });
});

describe("forwardStatusWord", () => {
  it("reflects the observed reachability", () => {
    expect(forwardStatusWord({ up: true })).toBe("up");
    expect(forwardStatusWord({ up: false })).toBe("down");
  });

  it("does not fabricate a value when there is no observation yet", () => {
    expect(forwardStatusWord({ up: null })).toBe("unknown");
    expect(forwardStatusWord({ up: undefined })).toBe("unknown");
    expect(forwardStatusWord({})).toBe("unknown");
  });

  // ADR-0053 Phase 85: 転送（listener）はあるのに先方（target）が応答しないのは、転送そのものが
  // 無い（down）とは別の状態。celeris はこの状態では転送を再発行しない。
  it("distinguishes a present listener with an unhealthy target from a missing forward", () => {
    expect(forwardStatusWord({ up: false, listener: true, target_healthy: false })).toBe("unreachable");
    expect(forwardStatusWord({ up: true, listener: true, target_healthy: true })).toBe("up");
    expect(forwardStatusWord({ up: false, listener: false, target_healthy: false })).toBe("down");
  });

  it("never produces a badge with whitespace or more than 12 characters", () => {
    const cases: Array<{ up?: boolean | null; listener?: boolean | null; target_healthy?: boolean | null }> = [
      { up: true },
      { up: false },
      { up: null },
      { up: undefined },
      { up: false, listener: true, target_healthy: false },
    ];
    for (const forward of cases) {
      const word = forwardStatusWord(forward);
      expect(word).not.toMatch(/\s/);
      expect(word.length).toBeLessThanOrEqual(12);
    }
  });
});
