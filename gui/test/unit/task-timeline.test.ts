import { describe, expect, it } from "vitest";
import type { Event, TimelineItem } from "~/celeris/types";
import { groupTimelineWorkerProgress, timelineProgressGroupSummary, workerProgressStep } from "~/lib/task-timeline";

/**
 * `~/lib/task-timeline.ts` の純粋関数（ADR-0048 D2、フェーズ 74・ADR-0055 D2 ラウンド 6）。
 * `~/lib/console.ts` と同じ作り: HTTP も React も持ち込まないので、判断・整形はここで検証する。
 */

function progressEvent(
  over: Partial<Extract<Event, { type: "worker_progress" }>> = {},
): Extract<Event, { type: "worker_progress" }> {
  return { type: "worker_progress", run_id: "run1", msg: "did a thing", ...over };
}

function progressItem(
  seq: number,
  at: string,
  over: Partial<Extract<Event, { type: "worker_progress" }>> = {},
): TimelineItem {
  return { kind: "event", seq, at, event: progressEvent(over) };
}

function transitionedItem(seq: number, at: string): TimelineItem {
  return {
    kind: "event",
    seq,
    at,
    event: { type: "transitioned", from: "running", to: "done", reason: "worker_finished" },
  };
}

function commentItem(at: string): TimelineItem {
  return {
    kind: "comment",
    at,
    comment: { id: "c1", task_id: "T1", author_kind: "human", body: "hi", created_at: at },
  };
}

describe("groupTimelineWorkerProgress", () => {
  it("連続する worker_progress を 1 つの progress_group にまとめる", () => {
    const items: TimelineItem[] = [
      transitionedItem(0, "2026-09-19T00:00:00Z"),
      progressItem(1, "2026-09-19T00:00:01Z"),
      progressItem(2, "2026-09-19T00:00:02Z"),
      progressItem(3, "2026-09-19T00:00:03Z"),
      transitionedItem(4, "2026-09-19T00:00:04Z"),
    ];
    const result = groupTimelineWorkerProgress(items);
    expect(result).toHaveLength(3);
    expect(result[0]).toEqual({ kind: "item", item: items[0] });
    expect(result[1].kind).toBe("progress_group");
    if (result[1].kind === "progress_group") {
      expect(result[1].items.map((i) => i.seq)).toEqual([1, 2, 3]);
    }
    expect(result[2]).toEqual({ kind: "item", item: items[4] });
  });

  it("1 件だけの worker_progress も progress_group（items.length === 1）にする", () => {
    const items: TimelineItem[] = [
      transitionedItem(0, "2026-09-19T00:00:00Z"),
      progressItem(1, "2026-09-19T00:00:01Z"),
    ];
    const result = groupTimelineWorkerProgress(items);
    expect(result[1]).toEqual({ kind: "progress_group", items: [items[1]] });
  });

  it("離れた 2 区間は別々の progress_group になる（間に別種のイベントが挟まる）", () => {
    const items: TimelineItem[] = [
      progressItem(0, "2026-09-19T00:00:00Z"),
      progressItem(1, "2026-09-19T00:00:01Z"),
      commentItem("2026-09-19T00:00:02Z"),
      progressItem(2, "2026-09-19T00:00:03Z"),
    ];
    const result = groupTimelineWorkerProgress(items);
    expect(result.map((r) => r.kind)).toEqual(["progress_group", "item", "progress_group"]);
  });

  it("worker_progress が無ければ全部 item のまま", () => {
    const items: TimelineItem[] = [transitionedItem(0, "2026-09-19T00:00:00Z"), commentItem("2026-09-19T00:00:01Z")];
    expect(groupTimelineWorkerProgress(items)).toEqual([
      { kind: "item", item: items[0] },
      { kind: "item", item: items[1] },
    ]);
  });

  it("空配列は空配列", () => {
    expect(groupTimelineWorkerProgress([])).toEqual([]);
  });
});

describe("timelineProgressGroupSummary", () => {
  it("件数と最後の kind（既定 status）", () => {
    const items = [
      progressItem(0, "t0", { kind: "tool_use" }),
      progressItem(1, "t1", { kind: "tool_result" }),
    ] as Array<Extract<TimelineItem, { kind: "event" }> & { event: Extract<Event, { type: "worker_progress" }> }>;
    expect(timelineProgressGroupSummary(items)).toBe("2 件 ・ 最後: tool_result");
  });

  it("kind が無ければ status 扱い", () => {
    const items = [progressItem(0, "t0")] as Array<
      Extract<TimelineItem, { kind: "event" }> & { event: Extract<Event, { type: "worker_progress" }> }
    >;
    expect(timelineProgressGroupSummary(items)).toBe("1 件 ・ 最後: status");
  });
});

describe("workerProgressStep（Console の ConsoleReplyStep と同じ形にする）", () => {
  it("summary があれば summary、無ければ msg", () => {
    expect(workerProgressStep(progressEvent({ summary: "cargo test", msg: "raw" }))).toEqual({
      kind: "status",
      text: "cargo test",
      tool: null,
      error: false,
    });
    expect(workerProgressStep(progressEvent({ msg: "raw only" }))).toEqual({
      kind: "status",
      text: "raw only",
      tool: null,
      error: false,
    });
  });

  it("tool / error をそのまま写す", () => {
    expect(workerProgressStep(progressEvent({ kind: "tool_use", tool: "Bash", summary: "ls", error: true }))).toEqual({
      kind: "tool_use",
      text: "ls",
      tool: "Bash",
      error: true,
    });
  });
});
