import { lazy, Suspense } from "react";
import type { Graph } from "~/celeris/types";

const WorkTreeGraph = lazy(() => import("./WorkTreeGraph").then((m) => ({ default: m.WorkTreeGraph })));

/**
 * `~/components/WorkTreeGraph.tsx`（`@xyflow/react` + dagre 本体）への薄い窓口（Phase 77、ADR-0055 性能予算）。
 * フォールバックは本体側がマウント前に出していたのと同じ「読み込み中…」（見た目は変えない）。
 */
export function WorkTree({ graph, testId = "work-tree" }: { graph: Graph; testId?: string }) {
  return (
    <div className="h-[28rem] w-full overflow-hidden rounded-xl border border-border" data-testid={testId}>
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
