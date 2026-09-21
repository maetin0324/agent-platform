import { Background, Controls, type NodeMouseHandler, ReactFlow } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import type { Graph } from "~/celeris/types";
import { layoutGraph } from "~/lib/graph-layout";

/**
 * 案件の「仕事の木」の本体（Phase 77、ADR-0055 性能予算）。`@xyflow/react` + `~/lib/graph-layout.ts`
 * （`@dagrejs/dagre`）は gzip 前で 228KB あり、`/projects/:id` の初回 JS を確実に膨らませるので、
 * `~/components/WorkTree.tsx`（薄い `React.lazy` の窓口）からだけ読み込む。ここを直接 import しない。
 * `/graph`（`app/routes/graph.tsx`）と同じ `layoutGraph` を使い、ノードをクリックするとそのタスクの詳細
 * （`/tasks/:id`）へ移る点だけが違う（`/graph` はクリックで移動しない）。
 * `@xyflow/react` は ResizeObserver 等のブラウザ API に依存するため、マウント後にだけ本体を描く
 * （`/graph` と同じ理由。ADR-0006 D5。`React.lazy` で遅延しても、SSR はこのモジュールの `import()` 自体は
 * 解決するので、この mounted ガードが無いと SSR で `ResizeObserver` 無しの環境に当たって落ちる）。
 */
export function WorkTreeGraph({ graph, testId = "work-tree" }: { graph: Graph; testId?: string }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  const navigate = useNavigate();
  const { nodes, edges } = useMemo(() => layoutGraph(graph), [graph]);

  const onNodeClick: NodeMouseHandler = (_event, node) => {
    if (node.type === "group") return;
    navigate(`/tasks/${node.id}`);
  };

  // 外枠（高さ・角丸・`data-testid`）は `~/components/WorkTree.tsx` が持つ（`Suspense` のフォールバックと
  // 同じ枠を使い回して、チャンク読み込み中 → マウント前 → マウント後でサイズが変わらないようにするため）。
  if (!mounted) {
    return (
      <div
        className="flex h-full items-center justify-center text-sm text-fg-subtle"
        data-testid={`${testId}-placeholder`}
      >
        読み込み中…
      </div>
    );
  }
  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      fitView
      proOptions={{ hideAttribution: true }}
      nodesConnectable={false}
      colorMode="system"
      onNodeClick={onNodeClick}
    >
      <Background gap={18} size={1} />
      <Controls />
    </ReactFlow>
  );
}
