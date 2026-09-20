import { describe, expect, it } from "vitest";
import type { Status, TaskSummary } from "~/celeris/types";
import {
  BOARD_COLUMNS,
  type BoardColumnId,
  type BoardFilter,
  boardColumnOf,
  boardFilterIsEmpty,
  boardFilterToParams,
  boardFilterToQuery,
  compareBoardCards,
  DEFAULT_PRIORITY,
  groupByColumn,
  isValidLabel,
  PRIORITY_VALUES,
  parseBoardFilter,
  priorityLabelOf,
  priorityValue,
  summaryPriorityLabel,
} from "~/lib/board";
import { taskSummary } from "../mock-celeris/fixtures";

/**
 * ボードの純粋なヘルパー（ADR-0044 D3 / D4）。優先度の対応と丸めの境界は celeris の
 * `task_core::PRIORITY_LABELS` / `priority_label` と同じでなければならない（GUI は並べ替えにだけ使う）。
 */
describe("優先度の対応（ADR-0044 D3）", () => {
  it("ラベル → 整数（P0 = 30 / P1 = 20 / P2 = 10 / P3 = 0）", () => {
    expect(priorityValue("P0")).toBe(30);
    expect(priorityValue("P1")).toBe(20);
    expect(priorityValue("P2")).toBe(10);
    expect(priorityValue("P3")).toBe(0);
  });

  it("大文字小文字・前後の空白は無視し、知らない値は既定（P2 = 10）", () => {
    expect(priorityValue(" p1 ")).toBe(20);
    expect(priorityValue("urgent")).toBe(DEFAULT_PRIORITY);
    expect(DEFAULT_PRIORITY).toBe(PRIORITY_VALUES.P2);
  });

  it("整数 → ラベル（celeris と同じ境界: >=30 → P0、>=20 → P1、>=10 → P2、それ未満 → P3）", () => {
    expect(priorityLabelOf(30)).toBe("P0");
    expect(priorityLabelOf(31)).toBe("P0");
    expect(priorityLabelOf(29)).toBe("P1");
    expect(priorityLabelOf(20)).toBe("P1");
    expect(priorityLabelOf(19)).toBe("P2");
    expect(priorityLabelOf(10)).toBe("P2");
    expect(priorityLabelOf(9)).toBe("P3");
    expect(priorityLabelOf(0)).toBe("P3");
    expect(priorityLabelOf(-5)).toBe("P3");
  });

  it("ラベル → 整数 → ラベルは往復する", () => {
    for (const label of ["P0", "P1", "P2", "P3"] as const) {
      expect(priorityLabelOf(priorityValue(label))).toBe(label);
    }
  });

  it("行は celeris の `priority_label` を優先し、無い・知らない値のときだけ丸める", () => {
    expect(summaryPriorityLabel({ priority: 0, priority_label: "P0" })).toBe("P0");
    expect(summaryPriorityLabel({ priority: 25, priority_label: "unknown" })).toBe("P1");
  });
});

describe("ボードの列（ADR-0044 D4）", () => {
  const ALL: Status[] = ["draft", "ready", "running", "blocked", "reviewing", "done", "failed", "cancelled"];

  it("8 つの状態がちょうど 6 列に割り当たる", () => {
    const mapping = Object.fromEntries(ALL.map((s) => [s, boardColumnOf(s)]));
    expect(mapping).toEqual({
      draft: "waiting",
      ready: "waiting",
      running: "in_progress",
      reviewing: "in_progress",
      blocked: "blocked",
      done: "done",
      failed: "failed",
      cancelled: "cancelled",
    });
  });

  it("列の並びは ADR-0044 D4 の順（待ち・進行中・止まっている・完了・失敗・中止）", () => {
    expect(BOARD_COLUMNS.map((c) => c.id)).toEqual([
      "waiting",
      "in_progress",
      "blocked",
      "done",
      "failed",
      "cancelled",
    ] satisfies BoardColumnId[]);
  });

  it("知らない状態はどの列にも入れない（画面を壊さない）", () => {
    expect(boardColumnOf("archived")).toBeNull();
    const grouped = groupByColumn([taskSummary({ id: "X", status: "archived" as Status })]);
    expect(Object.values(grouped).flat()).toEqual([]);
  });

  it("列の中は優先度の降順 → created_at の昇順に並ぶ", () => {
    const items: TaskSummary[] = [
      taskSummary({ id: "C", status: "ready", priority_label: "P2", created_at: "2026-09-19T00:00:00Z" }),
      taskSummary({ id: "A", status: "ready", priority_label: "P0", created_at: "2026-09-19T02:00:00Z" }),
      taskSummary({ id: "B", status: "draft", priority_label: "P2", created_at: "2026-09-18T00:00:00Z" }),
      taskSummary({ id: "D", status: "done", priority_label: "P3", created_at: "2026-09-17T00:00:00Z" }),
    ];
    const grouped = groupByColumn(items);
    expect(grouped.waiting.map((t) => t.id)).toEqual(["A", "B", "C"]);
    expect(grouped.done.map((t) => t.id)).toEqual(["D"]);
    expect(grouped.failed).toEqual([]);
  });

  it("同じ優先度・同じ時刻は id で決定的に並ぶ（SSE の再検証で順が揺れない）", () => {
    const a = taskSummary({ id: "AAA", created_at: "2026-09-19T00:00:00Z" });
    const b = taskSummary({ id: "BBB", created_at: "2026-09-19T00:00:00Z" });
    expect(compareBoardCards(a, b)).toBeLessThan(0);
    expect(compareBoardCards(b, a)).toBeGreaterThan(0);
    expect(compareBoardCards(a, a)).toBe(0);
  });
});

describe("フィルタの読み書き（ADR-0044 D4）", () => {
  it("空のクエリは全部「指定なし」", () => {
    const filter = parseBoardFilter(new URLSearchParams(""));
    expect(filter).toEqual({
      project: null,
      labels: [],
      categories: [],
      assignee: null,
      milestone: null,
      tiers: [],
      priorities: [],
      q: null,
    });
    expect(boardFilterIsEmpty(filter)).toBe(true);
  });

  it("繰り返しのパラメータ（label / category / tier / priority）を全部読む", () => {
    const params = new URLSearchParams(
      "project=P1&label=pluvio&label=survey&category=research&category=docs&tier=frontier&priority=P0&priority=P1&assignee=research-survey&milestone=M1&q=関連研究",
    );
    expect(parseBoardFilter(params)).toEqual({
      project: "P1",
      labels: ["pluvio", "survey"],
      categories: ["research", "docs"],
      assignee: "research-survey",
      milestone: "M1",
      tiers: ["frontier"],
      priorities: ["P0", "P1"],
      q: "関連研究",
    });
  });

  it("空文字の欄は「指定なし」として落とす", () => {
    const filter = parseBoardFilter(new URLSearchParams("project=&label=&q=&assignee="));
    expect(filter.project).toBeNull();
    expect(filter.labels).toEqual([]);
    expect(filter.q).toBeNull();
    expect(filter.assignee).toBeNull();
  });

  it("読んで書いて読み直すと同じになる（繰り返しのパラメータを含めて往復する）", () => {
    const source = new URLSearchParams(
      "project=P1&label=pluvio&label=survey&category=research&assignee=a1&milestone=M1&tier=frontier&tier=cheap&priority=P0&q=ねらい",
    );
    const once = parseBoardFilter(source);
    const twice = parseBoardFilter(boardFilterToParams(once));
    expect(twice).toEqual(once);
    expect(boardFilterToParams(twice).toString()).toBe(boardFilterToParams(once).toString());
  });

  it("`GET /tasks` のクエリ名に写す（繰り返しは配列、指定なしは undefined）", () => {
    const filter: BoardFilter = {
      project: "P1",
      labels: ["pluvio"],
      categories: ["research", "docs"],
      assignee: null,
      milestone: "M1",
      tiers: [],
      priorities: ["P0"],
      q: null,
    };
    expect(boardFilterToQuery(filter)).toEqual({
      project: "P1",
      label: ["pluvio"],
      category: ["research", "docs"],
      assignee: undefined,
      milestone: "M1",
      tier: [],
      priority: ["P0"],
      q: undefined,
    });
  });
});

describe("ラベルの形（ADR-0044 D3。正は celeris。ここは入力補助）", () => {
  it("小文字の英数字とハイフンだけを通す", () => {
    expect(isValidLabel("pluvio")).toBe(true);
    expect(isValidLabel("a-1")).toBe(true);
    expect(isValidLabel("Pluvio")).toBe(false);
    expect(isValidLabel("pluvio survey")).toBe(false);
    expect(isValidLabel("研究")).toBe(false);
    expect(isValidLabel("")).toBe(false);
  });
});
