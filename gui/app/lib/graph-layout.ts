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

/** ノードのラベル: 1 行目がタイトル、2 行目に役割と状態の補足（ADR-0023 D3）。 */
function nodeLabel(node: GraphNode, awaitingChildren: boolean): string {
  const marks: string[] = [];
  if (node.role) marks.push(`[${node.role}]`);
  if (awaitingChildren) marks.push("部下待ち");
  return marks.length > 0 ? `${node.title}\n${marks.join(" ")}` : node.title;
}

// status ごとの帯・枠の色は app.css のトークン（docs/adr/0011 D4）に揃える。CSS 変数文字列をそのまま
// インラインスタイルに使う（bg-* 等の Tailwind クラスは React Flow の style prop には効かないため）。
const STATUS_COLOR: Record<Status, { accent: string; border: string; background: string }> = {
  draft: { accent: "var(--neutral-soft-fg)", border: "var(--neutral-border)", background: "var(--neutral-soft)" },
  ready: { accent: "var(--info)", border: "var(--info-border)", background: "var(--info-soft)" },
  running: { accent: "var(--primary)", border: "var(--primary-border)", background: "var(--primary-soft)" },
  blocked: { accent: "var(--warning)", border: "var(--warning-border)", background: "var(--warning-soft)" },
  reviewing: { accent: "var(--teal)", border: "var(--teal-border)", background: "var(--teal-soft)" },
  done: { accent: "var(--success)", border: "var(--success-border)", background: "var(--success-soft)" },
  failed: { accent: "var(--danger)", border: "var(--danger-border)", background: "var(--danger-soft)" },
  cancelled: { accent: "var(--neutral-soft-fg)", border: "var(--neutral-border)", background: "var(--neutral-soft)" },
};

export interface LayoutResult {
  nodes: Node[];
  edges: Edge[];
}

/** `layoutGraph` に渡す、taskd のスナップショット由来の印（ADR-0023 D3）。 */
export interface LayoutMarks {
  /** `DaemonSnapshot.awaiting_children`（委譲した子を待っている親）。 */
  awaitingChildren?: string[];
}

/** `graph.nodes` / `graph.edges` から React Flow の nodes/edges を組み立てる（純粋関数、DOM に依存しない）。 */
export function layoutGraph(graph: Graph, marks: LayoutMarks = {}): LayoutResult {
  const awaitingChildren = new Set(marks.awaitingChildren ?? []);
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
        border: "1px dashed var(--border-strong)",
        borderRadius: 12,
        background: "color-mix(in srgb, var(--primary) 6%, transparent)",
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
      // 「部下待ち」は taskd のスナップショットの値をそのまま出す（ADR-0023 D3。GUI 側で判定しない）。
      data: { label: nodeLabel(node, awaitingChildren.has(node.id)) },
      // 角丸・細い枠・左に status 色の帯（モダンな見た目）。kind=plan は枠を太くする意味づけを維持する
      // （docs/adr/0011 D4。色は STATUS_COLOR 経由で app.css のトークンに揃える）。
      style: {
        width: NODE_WIDTH,
        height: NODE_HEIGHT,
        background: STATUS_COLOR[node.status].background,
        border:
          node.kind === "plan"
            ? `3px solid ${STATUS_COLOR[node.status].accent}`
            : `1px solid ${STATUS_COLOR[node.status].border}`,
        borderLeft: `4px solid ${STATUS_COLOR[node.status].accent}`,
        borderRadius: 10,
        boxShadow: "0 1px 2px hsl(var(--shadow-color) / 0.08)",
        fontSize: 12,
        padding: "4px 8px",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        textAlign: "center",
        flexDirection: "column",
        whiteSpace: "pre-line",
        color: "var(--fg)",
      },
    });
  }

  const edges: Edge[] = graph.edges
    .filter((e) => byId.has(e.from) && byId.has(e.to))
    .map((e) => ({ id: `${e.from}-${e.to}`, source: e.from, target: e.to }));

  return { nodes, edges };
}
