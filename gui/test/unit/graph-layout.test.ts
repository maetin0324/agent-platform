import { describe, expect, it } from "vitest";
import { layoutGraph, nodeBox, textWidthEm, wrapLabelLines } from "~/lib/graph-layout";
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

/**
 * ノードの寸法（Phase G13f-1、監査 H4）。固定の 180×44 では日本語のタイトル + `[担当]` がはみ出していた。
 * 幅と高さを中身から決め、タイトルは最大 2 行・入り切らなければ `…` にする。
 */
describe("nodeBox（中身に合わせた寸法）", () => {
  it("短い英字のタイトルは最小の大きさのまま", () => {
    const box = nodeBox(node("01A", { title: "Small" }), false);
    expect(box).toEqual({ label: "Small", width: 180, height: 44 });
  });

  it("日本語のタイトル + 担当は 3 行になり、どの行も幅に収まる（はみ出さない）", () => {
    const box = nodeBox(
      node("01B", { title: "Pluvio の非同期ランタイムに関する関連研究の調査", role: "関連研究調査課" }),
      false,
    );
    expect(box.width).toBeGreaterThanOrEqual(180);
    expect(box.width).toBeLessThanOrEqual(320);
    const contentEm = (box.width - 20) / 12;
    for (const line of box.label.split("\n")) expect(textWidthEm(line)).toBeLessThanOrEqual(contentEm);
    // タイトル 2 行 + 担当 1 行
    expect(box.label.split("\n")).toHaveLength(3);
    expect(box.label.split("\n")[2]).toBe("[関連研究調査課]");
    expect(box.height).toBeGreaterThan(44);
  });

  it("2 行に入り切らないタイトルは末尾を … にする（はみ出させない）", () => {
    const title = "あ".repeat(200);
    const box = nodeBox(node("01C", { title }), false);
    const lines = box.label.split("\n");
    expect(lines).toHaveLength(2);
    expect(lines[1].endsWith("…")).toBe(true);
    const contentEm = (box.width - 20) / 12;
    for (const line of lines) expect(textWidthEm(line)).toBeLessThanOrEqual(contentEm);
  });

  it("`layoutGraph` はノードごとの寸法をそのまま style に載せる", () => {
    const graph: Graph = {
      nodes: [
        node("01A", { title: "Small" }),
        node("01B", {
          title: "Pluvio を基盤に用いた新たな研究テーマの模索と、その検証のための実験計画の立案",
        }),
      ],
      edges: [],
    };
    const result = layoutGraph(graph);
    const a = result.nodes.find((n) => n.id === "01A");
    const b = result.nodes.find((n) => n.id === "01B");
    expect(a?.style?.width).toBe(180);
    expect(Number(b?.style?.width)).toBeGreaterThan(180);
  });
});

describe("wrapLabelLines", () => {
  it("全角は 1em、半角は 0.56em として折り返す", () => {
    expect(textWidthEm("あい")).toBeCloseTo(2);
    expect(textWidthEm("ab")).toBeCloseTo(1.12);
    expect(wrapLabelLines("あいうえおかきくけこ", 5)).toEqual(["あいうえお", "かきくけこ"]);
  });

  it("行数に収まるならそのまま返す", () => {
    expect(wrapLabelLines("short", 10)).toEqual(["short"]);
  });
});
