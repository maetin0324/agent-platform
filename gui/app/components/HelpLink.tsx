import { Link } from "react-router";

/**
 * 各画面の見出しの隣に置く `/help#<id>` へのリンク（`?`）。docs/DESIGN.md §10 Phase G6。
 */
export function HelpLink({ anchor, label }: { anchor: string; label: string }) {
  return (
    <Link
      to={`/help#${anchor}`}
      aria-label={`使い方: ${label}`}
      data-testid="help-link"
      className="ml-2 rounded-full border px-2 text-xs font-normal text-gray-500 no-underline hover:bg-gray-100"
    >
      ?
    </Link>
  );
}
