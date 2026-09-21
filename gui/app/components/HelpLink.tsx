import { Link } from "react-router";
import { Icon } from "~/components/ui/Icon";

/**
 * 各画面の見出しの隣に置く `/help#<id>` へのリンク（`?`）。docs/DESIGN.md §10 Phase G6。
 */
export function HelpLink({ anchor, label }: { anchor: string; label: string }) {
  return (
    <Link
      to={`/help#${anchor}`}
      aria-label={`使い方: ${label}`}
      data-testid="help-link"
      // ADR-0055 D1-2: タップ領域 44×44 以上。デスクトップは `lg:size-6` で元の大きさに戻す。
      className="ml-2 inline-flex size-11 shrink-0 items-center justify-center rounded-full border border-border text-fg-subtle no-underline transition-colors hover:border-primary-border hover:bg-primary-soft hover:text-primary-soft-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring lg:size-6"
    >
      <Icon name="help" className="size-3.5" strokeWidth={2} />
    </Link>
  );
}
