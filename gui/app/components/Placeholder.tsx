import type { IconName } from "~/components/ui/Icon";
import { EmptyState, PageHeader } from "~/components/ui/misc";

/**
 * G13b（対話・報告・認可・成果物。ADR-0033 D4/D3/D5、SPEC §4）で作る画面のプレースホルダ。
 * taskd に問い合わせない（loader 無し）。今回の Phase（G13a）の範囲外であることと、
 * SPEC の該当節の一文だけを出す。
 */
export function PlaceholderPage({
  icon,
  title,
  description,
  specRef,
  specQuote,
}: {
  icon: IconName;
  title: string;
  description: string;
  specRef: string;
  specQuote: string;
}) {
  return (
    <div className="space-y-6" data-testid="placeholder-section">
      <PageHeader icon={icon} title={title} description={description} />
      <EmptyState icon={icon} title="G13b で作ります">
        <p>
          <span className="font-mono text-xs text-fg-subtle">{specRef}</span>: 「{specQuote}」
        </p>
      </EmptyState>
    </div>
  );
}
