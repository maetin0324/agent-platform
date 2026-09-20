import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { classifyStreamJsonLine } from "~/lib/stream-json";

const fixturesDir = join(import.meta.dirname, "../fixtures/stream-json");

function readLines(name: string): string[] {
  return readFileSync(join(fixturesDir, name), "utf-8")
    .split("\n")
    .filter((line) => line.trim().length > 0);
}

describe("classifyStreamJsonLine — claude-code", () => {
  const lines = readLines("claude-code.jsonl");

  it("classifies text as utterance", () => {
    expect(classifyStreamJsonLine(lines[0])).toEqual({ kind: "utterance", text: "working on it" });
  });

  it("classifies tool_use as tool", () => {
    expect(classifyStreamJsonLine(lines[1])).toEqual({ kind: "tool", label: "Bash", detail: "{}" });
  });

  it("classifies result as result (isError=false)", () => {
    const result = classifyStreamJsonLine(lines[2]);
    expect(result.kind).toBe("result");
    expect(result).toMatchObject({ kind: "result", isError: false });
  });
});

describe("classifyStreamJsonLine — codex", () => {
  const lines = readLines("codex.jsonl");

  it("classifies thread.started as raw (the line itself)", () => {
    expect(classifyStreamJsonLine(lines[0])).toEqual({ kind: "raw", text: lines[0] });
  });

  it("classifies item.started (command_execution) as tool", () => {
    expect(classifyStreamJsonLine(lines[1])).toEqual({
      kind: "tool",
      label: "command_execution",
      detail: JSON.stringify({ type: "command_execution", command: "cargo test" }),
    });
  });

  it("classifies turn.completed as result (isError=false)", () => {
    const result = classifyStreamJsonLine(lines[2]);
    expect(result.kind).toBe("result");
    expect(result).toMatchObject({ kind: "result", isError: false });
  });

  it("classifies turn.failed as result (isError=true)", () => {
    expect(classifyStreamJsonLine(lines[3])).toEqual({
      kind: "result",
      text: "sandbox denied write",
      isError: true,
    });
  });
});

describe("classifyStreamJsonLine — fake worker (celeris 独自プロトコル)", () => {
  const lines = readLines("fake.jsonl");

  it("classifies every line as raw", () => {
    for (const line of lines) {
      expect(classifyStreamJsonLine(line)).toEqual({ kind: "raw", text: line });
    }
  });
});

describe("classifyStreamJsonLine — malformed / unknown input", () => {
  it("classifies invalid JSON as raw", () => {
    expect(classifyStreamJsonLine("not json")).toEqual({ kind: "raw", text: "not json" });
  });

  it("classifies a line without type as raw", () => {
    expect(classifyStreamJsonLine("{}")).toEqual({ kind: "raw", text: "{}" });
  });
});
