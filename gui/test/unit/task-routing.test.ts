import { describe, expect, it } from "vitest";
import type { RoutingAudit, TaskRoutingView } from "~/celeris/types";
import {
  droppedAssigneeNote,
  escalationHistory,
  featureRows,
  formatCostUsd,
  formatTokens,
  formatWallMs,
  latestRoutingRun,
  reviewResultLabel,
  routingSummaryLine,
  tierSourceLabel,
} from "~/lib/task-routing";

/** `~/lib/task-routing.ts`（celeris ADR-0068 D5 の「ルーティング」パネル）の純粋関数。 */

function run(over: Partial<RoutingAudit> = {}): RoutingAudit {
  return { task_id: "T1", run_id: "R1", ...over };
}

function view(runs: RoutingAudit[], routing: TaskRoutingView["routing"] = null): TaskRoutingView {
  return { task_id: "T1", routing, runs };
}

describe("task-routing", () => {
  it("1 行の要約は org / harness / lane / model、無い値は —", () => {
    expect(
      routingSummaryLine(run({ org_node: "coding", harness: "coding", lane: "standard", model: "model-std" })),
    ).toBe("coding / coding / standard / model-std");
    expect(routingSummaryLine(run())).toBe("— / — / — / —");
  });

  it("表示の中心は最新の run。run が無ければ null", () => {
    expect(latestRoutingRun(view([run({ run_id: "A" }), run({ run_id: "B" })]))?.run_id).toBe("B");
    expect(latestRoutingRun(view([]))).toBeNull();
    expect(latestRoutingRun(null)).toBeNull();
  });

  it("features は 9 軸を固定の順で、段階は低・中・高", () => {
    const rows = featureRows({
      judgment: "high",
      ambiguity: "low",
      verifiability: "medium",
      reversibility: "high",
      consequence: "low",
      context_size: "medium",
      tool_intensity: "low",
      expected_length: "medium",
      cross_cutting: "low",
    });
    expect(rows).toHaveLength(9);
    expect(rows[0]).toEqual({ key: "judgment", label: "判断の重さ", level: "高" });
    expect(rows[1].level).toBe("低");
    expect(rows[2].level).toBe("中");
  });

  it("捨てた担当があれば注記を出す", () => {
    expect(droppedAssigneeNote(view([], { dropped_assignee: "research" }))).toContain("「research」");
    expect(droppedAssigneeNote(view([], { tier_source: "hint" }))).toBeNull();
    expect(droppedAssigneeNote(null)).toBeNull();
  });

  it("tier の出自のラベル", () => {
    expect(tierSourceLabel("human")).toBe("人の明示");
    expect(tierSourceLabel(undefined)).toBe("policy");
  });

  it("コスト・トークン・所要時間・レビューの整形", () => {
    expect(formatCostUsd(1.234)).toBe("$1.23");
    expect(formatCostUsd(0.0012)).toBe("$0.0012");
    expect(formatCostUsd(null)).toBe("—");
    expect(formatTokens(1200, 34)).toBe("入力 1,200 / 出力 34");
    expect(formatTokens(undefined, undefined)).toBe("—");
    expect(formatWallMs(250)).toBe("250 ms");
    expect(formatWallMs(12_400)).toBe("12 秒");
    expect(formatWallMs(125_000)).toBe("2 分 5 秒");
    expect(reviewResultLabel({ passed: true })).toBe("合格");
    expect(reviewResultLabel({ passed: false, failed_criteria: [0, 2] })).toBe("不合格（条件 #0, #2）");
    expect(reviewResultLabel(null)).toBe("—");
  });

  it("エスカレーションの履歴は escalation の付いた run だけ（古い順）", () => {
    expect(
      escalationHistory(view([run({ run_id: "A" }), run({ run_id: "B", escalation: "standard -> frontier" })])),
    ).toEqual([{ runId: "B", text: "standard -> frontier" }]);
  });
});
