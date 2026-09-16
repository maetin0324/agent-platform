import { json } from "@codemirror/lang-json";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { useEffect, useRef, useState } from "react";

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
      extensions: [EditorState.readOnly.of(true), EditorView.editable.of(false), ...(isJson ? [json()] : [])],
      parent,
    });
    setMounted(true);
    return () => {
      view.destroy();
      setMounted(false);
    };
  }, [content, isJson]);

  return (
    <div data-testid="code-viewer">
      {!mounted && <pre>{content}</pre>}
      <div ref={mountRef} />
    </div>
  );
}
