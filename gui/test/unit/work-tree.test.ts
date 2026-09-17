import { describe, expect, it } from "vitest";
import { projectTasksToGraph } from "~/lib/work-tree";
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
});
