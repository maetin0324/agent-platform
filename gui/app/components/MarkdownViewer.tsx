import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Markdown 成果物の表示。HTML パススルーのプラグインは付けない。
 * DESIGN §8.3: LLM が書いた信用できない内容なので、生の HTML タグは `react-markdown` の既定どおりテキストとしてエスケープされる。
 *
 * `live`（Phase 76、ADR-0055 D1 拡張）: Console の育つ返事（`~/components/ConsoleBlockItem.tsx::ReplyBlockView`）
 * のように、この中身が run 中に伸びていく箱でだけ `true` にする。それ以外（報告・途中目標のレビュー・
 * 文書ページ等、開いたあと中身が変わらない箱）は既定 `false`（`aria-live` を付けない。無関係なページ全部を
 * ライブリージョンにすると意味が薄れるため）。
 */
export function MarkdownViewer({ content, live = false }: { content: string; live?: boolean }) {
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
