import { describe, expect, it } from "vitest";
import type { ProjectTaskView } from "~/celeris/types";
import { milestoneDecisionNoteRequired, milestoneDecisionValid, milestoneIsStalled } from "~/lib/milestone-review";

/**
 * `~/lib/milestone-review.ts` の純粋関数（ADR-0038、Phase 41 / G13j）。
 * `milestoneIsStalled` は celeris 側の決定的な判定（`crates/celeris/src/milestone_review.rs::ready_milestones`）
 * と同じ条件（裏方を除く、その途中目標のタスクだけ、動いているものが無く done が 1 件以上）。
 */

const task = (over: Partial<ProjectTaskView> = {}): ProjectTaskView => ({
  id: "t1",
  title: "調べる",
  status: "done",
  parent_id: null,
  depends_on: [],
  assignee: "research-survey",
  milestone_id: "m1",
  conversation: false,
  ...over,
});

describe("milestoneIsStalled", () => {
  it("done が 1 件以上で、動いているものが無ければ止まっている", () => {
    const tasks = [task({ id: "t1", status: "done" }), task({ id: "t2", status: "done" })];
    expect(milestoneIsStalled(tasks, "m1")).toBe(true);
  });

  it("ready/running/reviewing/blocked が 1 件でもあれば止まっていない", () => {
    for (const status of ["ready", "running", "reviewing", "blocked"] as const) {
      const tasks = [task({ id: "t1", status: "done" }), task({ id: "t2", status })];
      expect(milestoneIsStalled(tasks, "m1")).toBe(false);
    }
  });

  it("done が 0 件なら止まっていない扱い（達成の判断材料が無い）", () => {
    const tasks = [task({ id: "t1", status: "failed" })];
    expect(milestoneIsStalled(tasks, "m1")).toBe(false);
  });

  it("その途中目標のタスクが 1 件も無ければ止まっていない", () => {
    expect(milestoneIsStalled([], "m1")).toBe(false);
  });

  it("裏方タスク（support あり）は数えない", () => {
    const tasks = [
      task({ id: "t1", status: "done" }),
      task({ id: "t2", status: "running", support: "milestone_review" }),
    ];
    // 裏方の running は無視されるので、work タスクだけ見ると done が 1 件・動いているものが無い → 止まっている
    expect(milestoneIsStalled(tasks, "m1")).toBe(true);
  });

  it("他の途中目標のタスクは数えない", () => {
    const tasks = [task({ id: "t1", status: "done", milestone_id: "m2" })];
    expect(milestoneIsStalled(tasks, "m1")).toBe(false);
  });
});

describe("milestoneDecisionNoteRequired / milestoneDecisionValid", () => {
  it("ok は note が無くてもよい", () => {
    expect(milestoneDecisionNoteRequired("ok")).toBe(false);
    expect(milestoneDecisionValid("ok", "")).toBe(true);
    expect(milestoneDecisionValid("ok", "  ")).toBe(true);
  });

  it("discuss / ng は note が空（空白のみを含む）なら無効", () => {
    for (const decision of ["discuss", "ng"] as const) {
      expect(milestoneDecisionNoteRequired(decision)).toBe(true);
      expect(milestoneDecisionValid(decision, "")).toBe(false);
      expect(milestoneDecisionValid(decision, "   ")).toBe(false);
      expect(milestoneDecisionValid(decision, "理由はこれ")).toBe(true);
    }
  });
});
