import { describe, expect, it } from "vitest";
import { shortId, splitOutcome, truncateLabel } from "~/lib/format";

describe("shortId", () => {
  it("returns the id unchanged when it already fits within tailLength + 1", () => {
    expect(shortId("p1")).toBe("p1");
    expect(shortId("12345678")).toBe("12345678");
    expect(shortId("123456789")).toBe("123456789");
  });

  it("keeps the last tailLength characters and prefixes an ellipsis when longer", () => {
    expect(shortId("01BOARDTASK00000000000001")).toBe("…00000001");
    expect(shortId("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 12)).toBe("…aaaaaaaaaaaa");
  });

  it("respects a custom tailLength", () => {
    expect(shortId("01BOARDTASK00000000000001", 4)).toBe("…0001");
  });
});

describe("truncateLabel", () => {
  it("returns the text unchanged when it fits within maxLength", () => {
    expect(truncateLabel("関連研究を調べる")).toBe("関連研究を調べる");
    expect(truncateLabel("")).toBe("");
  });

  it("truncates and appends an ellipsis when longer than maxLength", () => {
    const long = "a".repeat(50);
    const result = truncateLabel(long, 40);
    expect(result).toBe(`${"a".repeat(39)}…`);
    expect(result.length).toBe(40);
  });

  it("respects a custom maxLength", () => {
    expect(truncateLabel("Pluvio の新テーマ", 5)).toBe("Pluv…");
  });
});

describe("splitOutcome", () => {
  it("接頭辞（ステータス名）と長文の本文に分ける", () => {
    expect(splitOutcome("done: 全部やりました: 詳細は report.md")).toEqual({
      status: "done",
      text: "全部やりました: 詳細は report.md",
    });
  });

  it("接頭辞だけ・本文なしはステータス名だけ", () => {
    expect(splitOutcome("done")).toEqual({ status: "done", text: null });
    expect(splitOutcome("error: ")).toEqual({ status: "error", text: null });
  });
});
