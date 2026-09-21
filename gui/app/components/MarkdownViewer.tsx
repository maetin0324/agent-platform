import { lazy, Suspense } from "react";
import { Skeleton } from "~/components/ui/skeleton";

const MarkdownViewerBody = lazy(() => import("./MarkdownViewerBody").then((m) => ({ default: m.MarkdownViewerBody })));

/**
 * `~/components/MarkdownViewerBody.tsx`（`react-markdown`/`remark-gfm` 本体）への薄い窓口
 * （Phase 77、ADR-0055 性能予算）。11 箇所（Console・報告・文書・知識ベース等）から使われる一番重い
 * 依存（gzip 前 154KB）なので、`React.lazy` でチャンクを分ける。SSR はサーバ側で `import()` を解決して
 * 本文入りの HTML を返す（ADR-0002 D2 の SSR はそのまま）ので、最初の応答には中身がそのまま乗る。
 * クライアントの再水和がこのチャンクを取りに行く一瞬だけ、下の `MarkdownSkeleton` に差し替わりうる
 * （SSR からの遷移では通常見えない。クライアント遷移直後の初回だけ見えることがある）。
 */
export function MarkdownViewer({ content, live = false }: { content: string; live?: boolean }) {
  return (
    <Suspense fallback={<MarkdownSkeleton />}>
      <MarkdownViewerBody content={content} live={live} />
    </Suspense>
  );
}

/** 段落 3 行ぶんの高さを予約するだけの簡易プレースホルダ（レイアウトのガタつきを避ける）。 */
function MarkdownSkeleton() {
  return (
    <div className="space-y-2 rounded-lg border border-border bg-surface p-4" data-testid="markdown-viewer-skeleton">
      <Skeleton className="h-3.5 w-11/12" />
      <Skeleton className="h-3.5 w-full" />
      <Skeleton className="h-3.5 w-2/3" />
    </div>
  );
}
