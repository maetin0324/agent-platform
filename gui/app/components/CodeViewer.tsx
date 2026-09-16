import { json } from "@codemirror/lang-json";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { useEffect, useRef, useState } from "react";

/**
 * ダークでもライトでも読めるように、背景・文字色・ガター等をトークンの CSS 変数で塗る
 * （docs/adr/0011 D1。`@codemirror/view` 同梱の `EditorView.theme` だけを使い、新しい依存は足さない）。
 * 構文ハイライト用の `highlightStyle` は付けていないので、色は素のテキストと選択範囲だけ。
 */
const editorTheme = EditorView.theme({
  "&": {
    backgroundColor: "var(--surface-2)",
    color: "var(--fg)",
  },
  ".cm-content": {
    caretColor: "var(--fg)",
    fontFamily: "var(--font-mono, ui-monospace, SFMono-Regular, Menlo, monospace)",
    fontSize: "0.8125rem",
    padding: "0.75rem 0",
  },
  ".cm-scroller": {
    fontFamily: "inherit",
    lineHeight: "1.6",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-gutters": {
    backgroundColor: "var(--surface-2)",
    color: "var(--fg-subtle)",
    border: "none",
  },
  ".cm-activeLine": { backgroundColor: "color-mix(in srgb, var(--fg) 6%, transparent)" },
  ".cm-activeLineGutter": { backgroundColor: "transparent" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
    backgroundColor: "color-mix(in srgb, var(--primary) 25%, transparent) !important",
  },
});

/**
 * ログ・成果物の生テキストを読み取り専用の CodeMirror 6 で表示する。
 * docs/adr/0006 D3: 読み取り専用は `EditorState.readOnly` と `EditorView.editable` の両方で保証する（DESIGN §8.3）。
 * CodeMirror はクライアント専用（`window` に依存）なので `useEffect` 内でのみ mount し、SSR/mount 前は `<pre>` の
 * 簡易フォールバックを出す。
 */
export function CodeViewer({ content, json: isJson }: { content: string; json?: boolean }) {
  const mountRef = useRef<HTMLDivElement>(null);
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    const parent = mountRef.current;
    if (!parent) return;
    const view = new EditorView({
      doc: content,
      extensions: [
        EditorState.readOnly.of(true),
        EditorView.editable.of(false),
        editorTheme,
        ...(isJson ? [json()] : []),
      ],
      parent,
    });
    setMounted(true);
    return () => {
      view.destroy();
      setMounted(false);
    };
  }, [content, isJson]);

  return (
    <div data-testid="code-viewer" className="overflow-hidden rounded-lg border border-border">
      {!mounted && <pre className="overflow-x-auto bg-surface-2 p-3 font-mono text-xs text-fg">{content}</pre>}
      <div ref={mountRef} />
    </div>
  );
}
