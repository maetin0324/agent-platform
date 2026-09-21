import type { HTMLAttributes, ReactNode } from "react";
import { cn } from "~/lib/utils";
import { Icon, type IconName } from "./Icon";
import { TONE_ICON_WRAP, type Tone } from "./tone";

/** 面（カード）。docs/adr/0011 D2。 */
export function Card({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("rounded-xl border border-border bg-surface shadow-sm transition-shadow", className)}
      {...props}
    />
  );
}

/**
 * カードの見出し行。`title` は呼び出し側が見出し要素（`<h2>` 等、id・testid 付き）を渡してもよい。
 */
export function CardHeader({
  icon,
  tone = "primary",
  title,
  description,
  actions,
  className,
}: {
  icon?: IconName;
  tone?: Tone;
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    // `flex-wrap`（幅は常に流体。ADR-0055 D2）: 狙いは横はみ出しを起こさないこと。`actions` は狙って
    // `w-full` にしているので、収まらないときだけ見出しの下の行に折り返す（`sm` 以上は従来どおり 1 行）。
    <div className={cn("flex flex-wrap items-start gap-3 border-b border-border px-5 py-4", className)}>
      {icon && (
        <span className={cn("mt-0.5 grid size-8 place-items-center rounded-lg", TONE_ICON_WRAP[tone])}>
          <Icon name={icon} className="size-4" />
        </span>
      )}
      <div className="min-w-0 flex-1">
        <div className="text-[0.95rem] font-semibold leading-6 text-fg">{title}</div>
        {description && <div className="mt-0.5 text-sm text-fg-muted">{description}</div>}
      </div>
      {actions && <div className="flex w-full flex-wrap items-center gap-2 sm:w-auto sm:shrink-0">{actions}</div>}
    </div>
  );
}

export function CardBody({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("px-5 py-4", className)} {...props} />;
}
