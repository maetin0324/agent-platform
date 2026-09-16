import { describe, expect, it } from "vitest";
import { layoutGraph } from "~/lib/graph-layout";
import type { Graph, GraphNode } from "~/taskd/types";

/**
 * `layoutGraph` は純粋関数（DOM に依存しない）。ここで見るのは、taskd の応答をそのまま写しているか
 * （役割ラベル、親子のグルーピング、辺の取捨）だけで、座標そのものは dagre に任せる。
 */

const node = (id: string, over: Partial<GraphNode> = {}): GraphNode => ({
  id,
  title: id,
  status: "done",
  kind: "execute",
  parent_id: null,
  ...over,
});

const labelOf = (result: ReturnType<typeof layoutGraph>, id: string): string => {
  const found = result.nodes.find((n) => n.id === id);
  if (!found) throw new Error(`node not found: ${id}`);
  return String(found.data.label);
};

describe("layoutGraph", () => {
  it("役割があるノードはラベルの 2 行目に役割を出す（色分けはしない）", () => {
    const graph: Graph = {
      nodes: [
        node("01LEAD", { title: "Lead-Delegator", role: "lead" }),
        node("01CHILD", { title: "Delegated-Child-1", role: "implementer" }),
        node("01PLAIN", { title: "No-Role" }),
      ],
      edges: [],
    };
    const result = layoutGraph(graph);
    expect(labelOf(result, "01LEAD")).toBe("Lead-Delegator\n[lead]");
    expect(labelOf(result, "01CHILD")).toBe("Delegated-Child-1\n[implementer]");
    expect(labelOf(result, "01PLAIN")).toBe("No-Role");
  });

  it("role が無い（古い taskd の）応答でもタイトルだけで描ける", () => {
    const withoutRole = { id: "01OLD", title: "Old", status: "ready", kind: "execute" } as GraphNode;
    const result = layoutGraph({ nodes: [withoutRole], edges: [] });
    expect(labelOf(result, "01OLD")).toBe("Old");
  });

  it("委譲で生まれた子は親の group にまとめられる（docs/adr/0010-g7-decisions.md D4、G7-U4）", () => {
    const graph: Graph = {
      nodes: [
        node("01LEAD", { title: "Lead-Delegator", role: "lead" }),
        node("01C1", { title: "Delegated-Child-1", role: "implementer", parent_id: "01LEAD" }),
        node("01C2", { title: "Delegated-Child-2", role: "implementer", parent_id: "01LEAD" }),
        node("01OTHER", { title: "Unrelated" }),
      ],
      edges: [],
    };
    const result = layoutGraph(graph);

    // 親ごとに group ノードが 1 つ合成され、子はその中（parentId 付き）に置かれる。
    const groups = result.nodes.filter((n) => n.type === "group");
    expect(groups.map((g) => g.id)).toEqual(["group-01LEAD"]);
    const children = result.nodes.filter((n) => n.parentId === "group-01LEAD").map((n) => n.id);
    expect(children.sort()).toEqual(["01C1", "01C2"]);

    // 親自身と無関係のノードは group に入らない。
    expect(result.nodes.find((n) => n.id === "01LEAD")?.parentId).toBeUndefined();
    expect(result.nodes.find((n) => n.id === "01OTHER")?.parentId).toBeUndefined();

    // group は子より前（React Flow の要件）。
    const order = result.nodes.map((n) => n.id);
    expect(order.indexOf("group-01LEAD")).toBeLessThan(order.indexOf("01C1"));
  });

  it("部下待ちの親はラベルに「部下待ち」が付く（ADR-0023 D3。GUI 側では判定しない）", () => {
    const graph: Graph = {
      nodes: [
        node("01LEAD", { title: "Lead-Delegator", role: "lead", status: "reviewing" }),
        node("01OTHER", { title: "Reviewing-Alone", status: "reviewing" }),
      ],
      edges: [],
    };
    const result = layoutGraph(graph, { awaitingChildren: ["01LEAD"] });
    expect(labelOf(result, "01LEAD")).toBe("Lead-Delegator\n[lead] 部下待ち");
    // 同じ reviewing でも、スナップショットに載っていなければ印は付かない。
    expect(labelOf(result, "01OTHER")).toBe("Reviewing-Alone");
    // 印を渡さなければ従来どおり。
    expect(labelOf(layoutGraph(graph), "01LEAD")).toBe("Lead-Delegator\n[lead]");
  });

  it("両端が nodes に無い辺は落とす", () => {
    const graph: Graph = {
      nodes: [node("01A"), node("01B")],
      edges: [
        { from: "01A", to: "01B", kind: "depends_on" },
        { from: "01A", to: "01MISSING", kind: "depends_on" },
      ],
    };
    const result = layoutGraph(graph);
    expect(result.edges.map((e) => e.id)).toEqual(["01A-01B"]);
  });
});
