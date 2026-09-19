import { describe, expect, it } from "vitest";
import {
  boardColumnLabel,
  commentAuthorLabel,
  commentEffectMessage,
  decisionLabel,
  milestoneStatusLabel,
  orgKindMark,
  parseTaskTab,
  priorityFullLabel,
  projectStatusLabel,
  TASK_TABS,
  taskCategoryLabel,
  taskFieldLabel,
  taskStatusLabel,
  taskTabLabel,
  tierLabel,
  timelineKindLabel,
} from "~/lib/labels";

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

/** ADR-0044（Phase 53）のタスク管理で足した言葉。 */
describe("タスク管理の言葉（ADR-0044）", () => {
  it("ボードの列（D4）", () => {
    expect(boardColumnLabel("waiting")).toBe("待ち");
    expect(boardColumnLabel("in_progress")).toBe("進行中");
    expect(boardColumnLabel("blocked")).toBe("止まっている");
    expect(boardColumnLabel("done")).toBe("完了");
    expect(boardColumnLabel("failed")).toBe("失敗");
    expect(boardColumnLabel("cancelled")).toBe("中止");
    expect(boardColumnLabel("unknown")).toBe("unknown");
  });

  it("タスクの種類（D3）", () => {
    expect(taskCategoryLabel("feature")).toBe("機能");
    expect(taskCategoryLabel("bug")).toBe("不具合");
    expect(taskCategoryLabel("research")).toBe("調査");
    expect(taskCategoryLabel("ops")).toBe("運用");
    expect(taskCategoryLabel("docs")).toBe("文書");
    expect(taskCategoryLabel("other")).toBe("その他");
    expect(taskCategoryLabel("unknown")).toBe("unknown");
  });

  it("優先度（D3）は記号に言葉を添える", () => {
    expect(priorityFullLabel("P0")).toBe("P0（今すぐ）");
    expect(priorityFullLabel("P3")).toBe("P3（低い）");
    expect(priorityFullLabel("unknown")).toBe("unknown");
  });

  it("担当エージェントのレベル（D1）", () => {
    expect(tierLabel("frontier")).toBe("最上位（frontier）");
    expect(tierLabel("cheap")).toBe("軽い（cheap）");
  });

  it("コメントの書き手（D2）", () => {
    expect(commentAuthorLabel("human")).toBe("あなた");
    expect(commentAuthorLabel("node")).toBe("担当");
    expect(commentAuthorLabel("system")).toBe("taskd");
  });

  it("コメントの効き方（D2 の表）は何が起きたかを必ず言う", () => {
    expect(commentEffectMessage("stored")).toContain("記録");
    expect(commentEffectMessage("interrupted")).toContain("run を止めて ready に戻しました");
    expect(commentEffectMessage("answered")).toContain("回答");
    expect(commentEffectMessage("terminal")).toContain("終わったタスク");
    expect(commentEffectMessage("unknown")).toBe("unknown");
  });

  it("タイムラインの種類（D5）", () => {
    expect(timelineKindLabel("comment")).toBe("コメント");
    expect(timelineKindLabel("delegation")).toBe("委譲");
    expect(timelineKindLabel("release")).toBe("リリース");
    // ADR-0043 A2 が入るまで作られない種類も、来たら言葉にする。
    expect(timelineKindLabel("integration")).toBe("取り込み");
    expect(timelineKindLabel("unknown")).toBe("unknown");
  });

  it("編集で変わった項目（D1 の `fields`）", () => {
    expect(taskFieldLabel("tier")).toBe("レベル");
    expect(taskFieldLabel("assignee")).toBe("担当");
    expect(taskFieldLabel("max_turns")).toBe("max_turns");
  });

  it("タスク画面のタブ（D5）は概要が既定で、知らない値も概要にする", () => {
    expect(TASK_TABS).toEqual(["overview", "timeline", "changes", "files", "artifacts"]);
    expect(TASK_TABS.map(taskTabLabel)).toEqual(["概要", "タイムライン", "変更", "ファイル", "成果物"]);
    expect(parseTaskTab(null)).toBe("overview");
    expect(parseTaskTab("")).toBe("overview");
    expect(parseTaskTab("timeline")).toBe("timeline");
    expect(parseTaskTab("nope")).toBe("overview");
  });
});
