import { cn } from "~/lib/utils";

/**
 * 読み込み中のプレースホルダ（Phase 77、ADR-0055 D3「体感速度」）。実際の中身と近い角丸・背景トーンにして、
 * 呼び出し側が `className` で高さ・幅を指定する（レイアウトのガタつき（CLS）を避けるため、実際の中身の
 * 概算の高さを予約する）。`app/app.css` の `@media (prefers-reduced-motion: reduce)` が `.animate-pulse` の
 * アニメーションを止める（他の `.animate-pulse-dot`/`.animate-fade-in` と同じ扱い）。装飾なので常に
 * `aria-hidden`（意味のある状態は呼び出し側が `aria-busy`/`role="status"` 等で別に伝える）。
 */
export function Skeleton({ className }: { className?: string }) {
  return <div aria-hidden="true" className={cn("animate-pulse rounded-md bg-surface-2", className)} />;
}
