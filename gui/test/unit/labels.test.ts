import { describe, expect, it } from "vitest";
import { decisionLabel, milestoneStatusLabel, orgKindMark, projectStatusLabel, taskStatusLabel } from "~/lib/labels";

/**
 * 業務の 6 画面に出す日本語（Phase G13f-1、監査 5）。taskd の値は変えず、表示だけを写す。
 * 知らない値が来たらそのまま出す（画面を壊さない）。
 */
describe("labels", () => {
  it("案件の状態", () => {
    expect(projectStatusLabel("proposed")).toBe("提案中");
    expect(projectStatusLabel("active")).toBe("進行中");
    expect(projectStatusLabel("paused")).toBe("一時停止");
    expect(projectStatusLabel("done")).toBe("完了");
    expect(projectStatusLabel("unknown")).toBe("unknown");
  });

  it("途中目標の状態", () => {
    expect(milestoneStatusLabel("proposed")).toBe("提案");
    expect(milestoneStatusLabel("approved")).toBe("承認済み");
    expect(milestoneStatusLabel("in_progress")).toBe("進行中");
    expect(milestoneStatusLabel("reached")).toBe("達成");
    expect(milestoneStatusLabel("redesigned")).toBe("再設計");
  });

  it("タスクの状態", () => {
    expect(taskStatusLabel("running")).toBe("作業中");
    expect(taskStatusLabel("blocked")).toBe("質問待ち");
    expect(taskStatusLabel("done")).toBe("完了");
  });

  it("役職の印は部・課の 1 文字だけ（英語のバッジは出さない）", () => {
    expect(orgKindMark("department")).toBe("部");
    expect(orgKindMark("section")).toBe("課");
    expect(orgKindMark("secretary")).toBe("");
  });

  it("認可の決定", () => {
    expect(decisionLabel("once")).toBe("今回だけ");
    expect(decisionLabel("standing")).toBe("今後ずっと");
    expect(decisionLabel("denied")).toBe("認めない");
  });
});
