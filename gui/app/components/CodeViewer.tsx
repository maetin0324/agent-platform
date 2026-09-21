import { lazy, Suspense } from "react";

const CodeViewerEditor = lazy(() => import("./CodeViewerEditor").then((m) => ({ default: m.CodeViewerEditor })));

/**
 * `~/components/CodeViewerEditor.tsx`（CodeMirror 本体）への薄い窓口（Phase 77、ADR-0055 性能予算）。
 * `@codemirror/*` は gzip 前で 260KB 超あり、これを毎回静的 import すると
 * `/tasks/:id`・`/projects/:id`・`~/components/ArtifactsList.tsx`・`~/components/task-files.tsx` を持つ画面の
 * 初回 JS が軒並み予算（350KB）を超える。ここでは `React.lazy` で本体のチャンクを分け、Suspense の間は
 * 本体と同じ `<pre>` フォールバック（読み取り中のテキストがそのまま読める。中身は変わらない）を出す。
 * SSR はサーバ側で `import()` を解決して本文入りの HTML を返す（ADR-0002 D2 の SSR はそのまま）ので、
 * JS が届く前でも中身は見える。高さは中身依存のまま（`<pre>` はいまの実装と同じ見た目なのでズレない）。
 */
export function CodeViewer({ content, json: isJson }: { content: string; json?: boolean }) {
  return (
    <Suspense
      fallback={
        <div data-testid="code-viewer" className="overflow-hidden rounded-lg border border-border">
          <pre className="overflow-x-auto bg-surface-2 p-3 font-mono text-xs text-fg">{content}</pre>
        </div>
      }
    >
      <CodeViewerEditor content={content} json={isJson} />
    </Suspense>
  );
}
