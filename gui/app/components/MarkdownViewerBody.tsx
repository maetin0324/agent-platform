import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Markdown 成果物の表示の本体（Phase 77、ADR-0055 性能予算）。`react-markdown`/`remark-gfm` は
 * gzip 前で 154KB あり、11 箇所（Console・報告・文書・知識ベース等ほぼ全画面）から使われるので、
 * 静的 import のままだとどの画面の初回 JS も確実に膨らむ。`~/components/MarkdownViewer.tsx`（薄い
 * `React.lazy` の窓口）からだけ読み込む。ここを直接 import しない。
 * DESIGN §8.3: LLM が書いた信用できない内容なので、生の HTML タグは `react-markdown` の既定どおりテキストとしてエスケープされる。
 */
export function MarkdownViewerBody({ content, live = false }: { content: string; live?: boolean }) {
  return (
    <div
      data-testid="markdown-viewer"
      aria-live={live ? "polite" : undefined}
      aria-atomic={live ? "false" : undefined}
      className="markdown rounded-lg border border-border bg-surface p-4"
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{content}</ReactMarkdown>
    </div>
  );
}
