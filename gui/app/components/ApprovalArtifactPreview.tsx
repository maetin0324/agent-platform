import { useEffect, useState } from "react";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { buttonClass } from "~/components/ui/button";
import { Icon } from "~/components/ui/Icon";
import { isMarkdownName } from "~/lib/docs";

/**
 * 承認画面（受信箱・タスク詳細の人レビューパネル）で、成果物のうち Markdown のものをその場で
 * 描画する折りたたみ（ADR-0067 D4）。`gui/app/routes/tasks.$id.tsx` の `ArtifactRow` と同じ
 * 「開く」トグル + fetch のパターンを再利用する。本文は既存の `GET /files/tasks/:id/artifacts/:idx`
 * （celeris の `GET /tasks/{id}/artifacts/{idx}` への中継）から取る。Markdown 以外の成果物は何も出さない
 * （名前の一覧は呼び出し側が既に出している）。
 */
export function ApprovalArtifactPreview({
  taskId,
  idx,
  name,
}: {
  /** 判定の材料を持つタスク（承認タスク自身ではなく、レビュー対象の親）。 */
  taskId: string;
  idx: number;
  name: string;
}) {
  const [open, setOpen] = useState(false);
  const [content, setContent] = useState<string | null>(null);
  const href = `/files/tasks/${taskId}/artifacts/${idx}`;

  useEffect(() => {
    if (!open || content !== null) return;
    let cancelled = false;
    (async () => {
      const res = await fetch(href);
      const text = await res.text();
      if (!cancelled) setContent(text);
    })();
    return () => {
      cancelled = true;
    };
  }, [open, content, href]);

  if (!isMarkdownName(name)) return null;

  return (
    <div className="mt-1">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        data-testid="approval-artifact-toggle"
        className={buttonClass({ variant: "ghost", size: "xs" })}
      >
        <Icon name={open ? "chevronDown" : "chevronRight"} />
        {open ? "本文を閉じる" : "本文をここで見る"}
      </button>
      {open && content !== null && (
        <div
          className="mt-2 max-h-96 overflow-y-auto rounded-lg border border-border p-3"
          data-testid="approval-artifact-body"
        >
          <MarkdownViewer content={content} />
        </div>
      )}
    </div>
  );
}
