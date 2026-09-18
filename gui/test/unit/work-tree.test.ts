import { describe, expect, it } from "vitest";
import { isSupportTask, projectTasksToGraph, visibleWorkTasks } from "~/lib/work-tree";
import type { OrgNode, ProjectTaskView } from "~/taskd/types";

/**
 * `projectTasksToGraph`（案件の「仕事の木」を `/graph` と同じ `layoutGraph` に渡せる `Graph` に写す）のテスト。
 * DAG の辺は既存どおり `parent_id` / `depends_on`（docs/gui/api.md §3.47）。
 */

const task = (id: string, over: Partial<ProjectTaskView> = {}): ProjectTaskView => ({
  id,
  title: id,
  status: "running",
  parent_id: null,
  depends_on: [],
  assignee: null,
  milestone_id: null,
  conversation: false,
  ...over,
});

describe("projectTasksToGraph", () => {
  it("parent_id をそのまま写し、depends_on から depends_on の辺を作る", () => {
    const graph = projectTasksToGraph(
      [
        task("plan", { title: "計画" }),
        task("survey", { parent_id: "plan", title: "調査" }),
        task("impl", { parent_id: "plan", depends_on: ["survey"], title: "実装" }),
      ],
      new Map(),
    );
    expect(graph.nodes.map((n) => ({ id: n.id, parent_id: n.parent_id }))).toEqual([
      { id: "plan", parent_id: null },
      { id: "survey", parent_id: "plan" },
      { id: "impl", parent_id: "plan" },
    ]);
    expect(graph.edges).toEqual([{ from: "survey", kind: "depends_on", to: "impl" }]);
  });

  it("assignee があれば、組織ノードの名前を role に入れる（ノード名を出すため）", () => {
    const org = new Map<string, OrgNode>([
      [
        "research-survey",
        {
          id: "research-survey",
          parent_id: "research",
          name: "関連研究調査課",
          kind: "section",
          position: 0,
          created_at: "…",
          updated_at: "…",
        },
      ],
    ]);
    const graph = projectTasksToGraph([task("t1", { assignee: "research-survey" })], org);
    expect(graph.nodes[0].role).toBe("関連研究調査課");
  });

  it("assignee が組織に見つからないときは id をそのまま出す", () => {
    const graph = projectTasksToGraph([task("t1", { assignee: "unknown-node" })], new Map());
    expect(graph.nodes[0].role).toBe("unknown-node");
  });

  it("assignee が無ければ role は null", () => {
    const graph = projectTasksToGraph([task("t1")], new Map());
    expect(graph.nodes[0].role).toBeNull();
  });

  it("存在しないタスクを指す depends_on の辺は layoutGraph 側が捨てる前提でそのまま渡す", () => {
    const graph = projectTasksToGraph([task("t1", { depends_on: ["gone"] })], new Map());
    expect(graph.edges).toEqual([{ from: "gone", kind: "depends_on", to: "t1" }]);
  });

  it("対話用タスク（conversation）は仕事の木から完全に外す（GUI-R3、Phase 27）", () => {
    const graph = projectTasksToGraph(
      [task("work", { title: "調べる" }), task("chat", { title: "対話: 秘書", conversation: true })],
      new Map(),
    );
    expect(graph.nodes.map((n) => n.id)).toEqual(["work"]);
  });

  it("対話用タスクを指す depends_on / parent_id は既存の欠落扱い（layoutGraph 側）にそのまま乗る", () => {
    const graph = projectTasksToGraph(
      [task("chat", { conversation: true }), task("work", { parent_id: "chat", depends_on: ["chat"] })],
      new Map(),
    );
    expect(graph.nodes.map((n) => n.id)).toEqual(["work"]);
    expect(graph.nodes[0].parent_id).toBe("chat");
    expect(graph.edges).toEqual([{ from: "chat", kind: "depends_on", to: "work" }]);
  });
});

/**
 * 裏方のタスク（Phase 29 の `support`: `"conversation"` | `"compaction"` | `"approval"` | `"review"`）は
 * 仕事の木にも件数にも出さない（監査 H4 / 裏方の印）。GUI 側で `role` や `title` から推測しない。
 */
describe("isSupportTask / visibleWorkTasks", () => {
  it("support が付いていれば裏方（対話・まとめ・承認待ち・レビュー）", () => {
    expect(isSupportTask({ support: "compaction", conversation: false })).toBe(true);
    expect(isSupportTask({ support: "approval", conversation: false })).toBe(true);
    expect(isSupportTask({ support: "review", conversation: false })).toBe(true);
    expect(isSupportTask({ support: "conversation", conversation: true })).toBe(true);
    expect(isSupportTask({ support: null, conversation: false })).toBe(false);
  });

  it("support が無い古い応答でも conversation だけは見る（保険）", () => {
    expect(isSupportTask({ conversation: true } as never)).toBe(true);
    expect(isSupportTask({ conversation: false } as never)).toBe(false);
  });

  it("裏方は木からも一覧からも外れる", () => {
    const tasks = [
      task("w1", { title: "調べる" }),
      task("chat", { conversation: true, support: "conversation" }),
      task("c1", { title: "報告のまとめ: 研究部", support: "compaction" }),
      task("a1", { title: "Approval needed: …", support: "approval" }),
    ];
    expect(visibleWorkTasks(tasks).map((t) => t.id)).toEqual(["w1"]);
    expect(projectTasksToGraph(tasks, new Map()).nodes.map((n) => n.id)).toEqual(["w1"]);
  });
});
