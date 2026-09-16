import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Markdown 成果物の表示。HTML パススルーのプラグインは付けない。
 * DESIGN §8.3: LLM が書いた信用できない内容なので、生の HTML タグは `react-markdown` の既定どおりテキストとしてエスケープされる。
 */
export function MarkdownViewer({ content }: { content: string }) {
  return (
    <div data-testid="markdown-viewer" className="markdown rounded-lg border border-border bg-surface p-4">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{content}</ReactMarkdown>
    </div>
  );
}
