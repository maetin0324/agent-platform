import dagre from "@dagrejs/dagre";
import type { Edge, Node } from "@xyflow/react";
import type { Graph, GraphNode, Status } from "~/celeris/types";

/**
 * `/graph` の描画用レイアウト（docs/adr/0006-g3-decisions.md D5）。
 * dagre には depends_on の辺だけを渡してフラットに層状配置し（compound グラフにはしない。親子と依存が両方
 * 絡む compound レイアウトは dagre の挙動が読みにくく、G3（light）の範囲を超えるため）、
 * 親子（`parent_id`）は配置後に子のバウンディングボックスから group ノードを合成する。
 * 色・枠の意味づけは celeris の `Status` / `TaskKind` をそのまま使い、GUI 側で新しい分類は作らない。
 */

const GROUP_PADDING = 28;
const GROUP_LABEL_HEIGHT = 24;

/**
 * ノードの寸法（Phase G13f-1、監査 H4）。固定の 180×44 では日本語のタイトル + `[担当]` がはみ出していたので、
 * 中身に合わせて幅と高さを決める（測定は文字数からの近似。DOM に触らず純粋関数のままにするため）。
 * タイトルは最大 2 行で、収まらなければ末尾を `…` にする。
 */
const FONT_SIZE = 12;
const LINE_HEIGHT = 16;
const NODE_PADDING_X = 10;
const NODE_PADDING_Y = 8;
const NODE_MIN_WIDTH = 180;
const NODE_MAX_WIDTH = 320;
const NODE_MIN_HEIGHT = 44;
const MAX_TITLE_LINES = 2;

/** 全角（CJK・全角記号・かな）は 1em、それ以外は約 0.56em として文字列の幅（em）を見積もる。 */
export function textWidthEm(text: string): number {
  let em = 0;
  for (const ch of text) {
    const code = ch.codePointAt(0) ?? 0;
    em += isWideChar(code) ? 1 : 0.56;
  }
  return em;
}

function isWideChar(code: number): boolean {
  return (
    (code >= 0x1100 && code <= 0x115f) || // ハングル字母
    (code >= 0x2e80 && code <= 0x303e) || // CJK 部首・記号
    (code >= 0x3041 && code <= 0x33ff) || // かな・互換
    (code >= 0x3400 && code <= 0x4dbf) ||
    (code >= 0x4e00 && code <= 0x9fff) || // CJK 統合漢字
    (code >= 0xa000 && code <= 0xa4cf) ||
    (code >= 0xac00 && code <= 0xd7a3) ||
    (code >= 0xf900 && code <= 0xfaff) ||
    (code >= 0xfe30 && code <= 0xfe6f) ||
    (code >= 0xff00 && code <= 0xff60) || // 全角英数・記号
    (code >= 0xffe0 && code <= 0xffe6)
  );
}

/**
 * 1 行あたり `maxEm` に収まるよう、最大 `maxLines` 行に折り返す。入り切らなければ最後の行の末尾を `…` にする
 * （単語境界は見ない。日本語のタイトルが主で、境界で切ると却って幅が余るため）。
 */
export function wrapLabelLines(text: string, maxEm: number, maxLines = MAX_TITLE_LINES): string[] {
  const chars = Array.from(text);
  const lines: string[] = [];
  let current = "";
  let currentEm = 0;
  for (const ch of chars) {
    const em = textWidthEm(ch);
    if (currentEm + em > maxEm && current.length > 0) {
      lines.push(current);
      current = "";
      currentEm = 0;
      if (lines.length === maxLines) break;
    }
    current += ch;
    currentEm += em;
  }
  if (lines.length < maxLines && current.length > 0) lines.push(current);
  if (lines.length === 0) return [text];
  // 収まらなかったぶんがあれば、最後の行の末尾を `…` にする。
  const shown = lines.join("");
  if (shown.length < text.length) {
    const last = Array.from(lines[lines.length - 1]);
    while (last.length > 0 && textWidthEm(`${last.join("")}…`) > maxEm) last.pop();
    lines[lines.length - 1] = `${last.join("")}…`;
  }
  return lines;
}

/** ノード 1 つぶんのラベルと寸法（`layoutGraph` が dagre と React Flow の両方にこの値を渡す）。 */
export interface NodeBox {
  label: string;
  width: number;
  height: number;
}

/**
 * ノードのラベルと寸法: 1〜2 行目がタイトル、その下に担当と状態の補足（ADR-0023 D3。
 * 担当は `[名前]`）。中身の幅から `NODE_MIN_WIDTH`〜`NODE_MAX_WIDTH` の範囲で幅を決め、
 * 行数から高さを決める。
 */
export function nodeBox(node: GraphNode, awaitingChildren: boolean): NodeBox {
  const marks: string[] = [];
  if (node.role) marks.push(`[${node.role}]`);
  if (awaitingChildren) marks.push("部下待ち");
  const marksText = marks.join(" ");

  const titleEm = textWidthEm(node.title);
  const marksEm = marksText.length > 0 ? textWidthEm(marksText) : 0;
  // タイトルは 2 行に割れる前提で必要な幅を見積もり、補足の行はできるだけ 1 行に収める。
  const wantedEm = Math.max(titleEm / MAX_TITLE_LINES, marksEm);
  const width = clamp(Math.ceil(wantedEm * FONT_SIZE) + NODE_PADDING_X * 2, NODE_MIN_WIDTH, NODE_MAX_WIDTH);
  const contentEm = (width - NODE_PADDING_X * 2) / FONT_SIZE;

  const titleLines = wrapLabelLines(node.title, contentEm);
  const lines = marksText.length > 0 ? [...titleLines, marksText] : titleLines;
  const height = Math.max(NODE_MIN_HEIGHT, lines.length * LINE_HEIGHT + NODE_PADDING_Y * 2);
  return { label: lines.join("\n"), width, height };
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
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

/** `layoutGraph` に渡す、celeris のスナップショット由来の印（ADR-0023 D3）。 */
export interface LayoutMarks {
  /** `DaemonSnapshot.awaiting_children`（委譲した子を待っている親）。 */
  awaitingChildren?: string[];
}

/** `graph.nodes` / `graph.edges` から React Flow の nodes/edges を組み立てる（純粋関数、DOM に依存しない）。 */
export function layoutGraph(graph: Graph, marks: LayoutMarks = {}): LayoutResult {
  const awaitingChildren = new Set(marks.awaitingChildren ?? []);
  const byId = new Map<string, GraphNode>(graph.nodes.map((n) => [n.id, n]));

  // ノードごとの寸法を先に決める（中身に合わせる。監査 H4）。dagre にも React Flow にも同じ値を渡す。
  const boxes = new Map<string, NodeBox>(
    graph.nodes.map((node) => [node.id, nodeBox(node, awaitingChildren.has(node.id))]),
  );

  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: "LR", nodesep: 32, ranksep: 64 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const node of graph.nodes) {
    const box = boxes.get(node.id);
    if (box) g.setNode(node.id, { width: box.width, height: box.height });
  }
  for (const edge of graph.edges) {
    if (byId.has(edge.from) && byId.has(edge.to)) g.setEdge(edge.from, edge.to);
  }
  dagre.layout(g);

  // 子の絶対座標（矩形の左上）を先に確定する。
  const rects = new Map<string, { x: number; y: number; w: number; h: number }>();
  for (const node of graph.nodes) {
    const pos = g.node(node.id);
    const box = boxes.get(node.id);
    if (!pos || !box) continue;
    rects.set(node.id, { x: pos.x - box.width / 2, y: pos.y - box.height / 2, w: box.width, h: box.height });
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
    const box = boxes.get(node.id);
    if (!rect || !box) continue;
    const groupRect = node.parent_id ? groupRects.get(node.parent_id) : undefined;
    const position = groupRect ? { x: rect.x - groupRect.x, y: rect.y - groupRect.y } : { x: rect.x, y: rect.y };
    nodes.push({
      id: node.id,
      position,
      parentId: groupRect ? `group-${node.parent_id}` : undefined,
      extent: groupRect ? "parent" : undefined,
      // 役割（案件の仕事の木では担当の名前）はテキストのラベルとして最終行に出す（色分けはしない。
      // docs/DESIGN.md §10 Phase G7、celeris-requests R2）。「部下待ち」は celeris のスナップショットの値を
      // そのまま出す（ADR-0023 D3。GUI 側で判定しない）。
      data: { label: box.label },
      // 角丸・細い枠・左に status 色の帯（モダンな見た目）。kind=plan は枠を太くする意味づけを維持する
      // （docs/adr/0011 D4。色は STATUS_COLOR 経由で app.css のトークンに揃える）。
      style: {
        width: box.width,
        height: box.height,
        background: STATUS_COLOR[node.status].background,
        border:
          node.kind === "plan"
            ? `3px solid ${STATUS_COLOR[node.status].accent}`
            : `1px solid ${STATUS_COLOR[node.status].border}`,
        borderLeft: `4px solid ${STATUS_COLOR[node.status].accent}`,
        borderRadius: 10,
        boxShadow: "0 1px 2px hsl(var(--shadow-color) / 0.08)",
        fontSize: FONT_SIZE,
        lineHeight: `${LINE_HEIGHT}px`,
        padding: `${NODE_PADDING_Y}px ${NODE_PADDING_X}px`,
        boxSizing: "border-box",
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
