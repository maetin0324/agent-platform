import { Background, Controls, type NodeMouseHandler, ReactFlow } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { layoutGraph } from "~/lib/graph-layout";
import type { Graph } from "~/taskd/types";

/**
 * 案件の「仕事の木」（SPEC §3.3）。`/graph`（`app/routes/graph.tsx`）と同じ `layoutGraph` を使い、
 * ノードをクリックするとそのタスクの詳細（`/tasks/:id`）へ移る点だけが違う（`/graph` はクリックで移動しない）。
 * `@xyflow/react` は ResizeObserver 等のブラウザ API に依存するため、マウント後にだけ本体を描く
 * （`/graph` と同じ理由。ADR-0006 D5）。
 */
export function WorkTree({ graph, testId = "work-tree" }: { graph: Graph; testId?: string }) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  const navigate = useNavigate();
  const { nodes, edges } = useMemo(() => layoutGraph(graph), [graph]);

  const onNodeClick: NodeMouseHandler = (_event, node) => {
    if (node.type === "group") return;
    navigate(`/tasks/${node.id}`);
  };

  return (
    <div className="h-[28rem] w-full overflow-hidden rounded-xl border border-border" data-testid={testId}>
      {mounted ? (
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
      ) : (
        <div
          className="flex h-full items-center justify-center text-sm text-fg-subtle"
          data-testid={`${testId}-placeholder`}
        >
          読み込み中…
        </div>
      )}
    </div>
  );
}
