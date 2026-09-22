import { lazy, Suspense } from "react";
import type { Graph } from "~/celeris/types";
import { cn } from "~/lib/utils";

const WorkTreeGraph = lazy(() => import("./WorkTreeGraph").then((m) => ({ default: m.WorkTreeGraph })));

/**
 * `~/components/WorkTreeGraph.tsx`（`@xyflow/react` + dagre 本体）への薄い窓口（Phase 77、ADR-0055 性能予算）。
 * フォールバックは本体側がマウント前に出していたのと同じ「読み込み中…」（見た目は変えない）。
 *
 * Phase 95（ADR-0055 ラウンド 19、目視点検の所見）: 高さは固定 28rem だったため、ノードが 1〜2 個しか
 * 無い小さな案件でも常に画面の大半を占める空白の多いキャンバスになっていた（`/projects/:id` の
 * 「仕事の木」）。ノード数に応じて 3 段階（少ない / 普通 / 多い）で高さを変える。`overflow-hidden` の箱の
 * 内側だけの変更で `@xyflow/react`（`fitView`）は箱の高さに追随するため、レイアウトの計算方法は変えない。
 */
export function WorkTree({ graph, testId = "work-tree" }: { graph: Graph; testId?: string }) {
  const heightClass = graph.nodes.length <= 2 ? "h-40" : graph.nodes.length <= 5 ? "h-64" : "h-[28rem]";
  return (
    <div className={cn("w-full overflow-hidden rounded-xl border border-border", heightClass)} data-testid={testId}>
      <Suspense
        fallback={
          <div
            className="flex h-full items-center justify-center text-sm text-fg-subtle"
            data-testid={`${testId}-placeholder`}
          >
            読み込み中…
          </div>
        }
      >
        <WorkTreeGraph graph={graph} testId={testId} />
      </Suspense>
    </div>
  );
}
