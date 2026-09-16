import dagre from "@dagrejs/dagre";
import type { Edge, Node } from "@xyflow/react";
import type { Graph, GraphNode, Status } from "~/taskd/types";

/**
 * `/graph` の描画用レイアウト（docs/adr/0006-g3-decisions.md D5）。
 * dagre には depends_on の辺だけを渡してフラットに層状配置し（compound グラフにはしない。親子と依存が両方
 * 絡む compound レイアウトは dagre の挙動が読みにくく、G3（light）の範囲を超えるため）、
 * 親子（`parent_id`）は配置後に子のバウンディングボックスから group ノードを合成する。
 * 色・枠の意味づけは taskd の `Status` / `TaskKind` をそのまま使い、GUI 側で新しい分類は作らない。
 */

const NODE_WIDTH = 180;
const NODE_HEIGHT = 44;
const GROUP_PADDING = 28;
const GROUP_LABEL_HEIGHT = 24;

const STATUS_COLOR: Record<Status, string> = {
  draft: "#e5e7eb",
  ready: "#fde68a",
  running: "#93c5fd",
  blocked: "#fde68a",
  reviewing: "#93c5fd",
  done: "#86efac",
  failed: "#fca5a5",
  cancelled: "#d1d5db",
};

export interface LayoutResult {
  nodes: Node[];
  edges: Edge[];
}

/** `graph.nodes` / `graph.edges` から React Flow の nodes/edges を組み立てる（純粋関数、DOM に依存しない）。 */
export function layoutGraph(graph: Graph): LayoutResult {
  const byId = new Map<string, GraphNode>(graph.nodes.map((n) => [n.id, n]));

  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: "LR", nodesep: 32, ranksep: 64 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const node of graph.nodes) {
    g.setNode(node.id, { width: NODE_WIDTH, height: NODE_HEIGHT });
  }
  for (const edge of graph.edges) {
    if (byId.has(edge.from) && byId.has(edge.to)) g.setEdge(edge.from, edge.to);
  }
  dagre.layout(g);

  // 子の絶対座標（矩形の左上）を先に確定する。
  const rects = new Map<string, { x: number; y: number; w: number; h: number }>();
  for (const node of graph.nodes) {
    const pos = g.node(node.id);
    rects.set(node.id, { x: pos.x - NODE_WIDTH / 2, y: pos.y - NODE_HEIGHT / 2, w: NODE_WIDTH, h: NODE_HEIGHT });
  }

  // parent_id ごとに子のバウンディングボックスから group を合成する（子の親自身が nodes に含まれるとは限らない）。
  const childrenByParent = new Map<string, string[]>();
  for (const node of graph.nodes) {
    if (!node.parent_id || !rects.has(node.id)) continue;
    const list = childrenByParent.get(node.parent_id) ?? [];
    list.push(node.id);
    childrenByParent.set(node.parent_id, list);
  }

  const groupRects = new Map<string, { x: number; y: number; w: number; h: number }>();
  for (const [parentId, childIds] of childrenByParent) {
    const childRects = childIds.map((id) => rects.get(id)).filter((r): r is NonNullable<typeof r> => r !== undefined);
    if (childRects.length === 0) continue;
    const minX = Math.min(...childRects.map((r) => r.x)) - GROUP_PADDING;
    const minY = Math.min(...childRects.map((r) => r.y)) - GROUP_PADDING - GROUP_LABEL_HEIGHT;
    const maxX = Math.max(...childRects.map((r) => r.x + r.w)) + GROUP_PADDING;
    const maxY = Math.max(...childRects.map((r) => r.y + r.h)) + GROUP_PADDING;
    groupRects.set(parentId, { x: minX, y: minY, w: maxX - minX, h: maxY - minY });
  }

  const nodes: Node[] = [];

  // group ノードは子より前に置く（React Flow の要件）。
  for (const [parentId, rect] of groupRects) {
    nodes.push({
      id: `group-${parentId}`,
      type: "group",
      position: { x: rect.x, y: rect.y },
      style: {
        width: rect.w,
        height: rect.h,
        border: "1px dashed #9ca3af",
        borderRadius: 8,
        background: "rgba(0,0,0,0.02)",
      },
      data: { label: "" },
      selectable: false,
      draggable: false,
    });
  }

  for (const node of graph.nodes) {
    const rect = rects.get(node.id);
    if (!rect) continue;
    const groupRect = node.parent_id ? groupRects.get(node.parent_id) : undefined;
    const position = groupRect ? { x: rect.x - groupRect.x, y: rect.y - groupRect.y } : { x: rect.x, y: rect.y };
    nodes.push({
      id: node.id,
      position,
      parentId: groupRect ? `group-${node.parent_id}` : undefined,
      extent: groupRect ? "parent" : undefined,
      // 役割はテキストのラベルとして 2 行目に出す（色分けはしない。docs/DESIGN.md §10 Phase G7、taskd-requests R2）。
      data: { label: node.role ? `${node.title}\n[${node.role}]` : node.title },
      style: {
        width: NODE_WIDTH,
        height: NODE_HEIGHT,
        background: STATUS_COLOR[node.status],
        border: node.kind === "plan" ? "3px solid #1f2937" : "1px solid #1f2937",
        borderRadius: 6,
        fontSize: 12,
        padding: 4,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        textAlign: "center",
        flexDirection: "column",
        whiteSpace: "pre-line",
      },
    });
  }

  const edges: Edge[] = graph.edges
    .filter((e) => byId.has(e.from) && byId.has(e.to))
    .map((e) => ({ id: `${e.from}-${e.to}`, source: e.from, target: e.to }));

  return { nodes, edges };
}
