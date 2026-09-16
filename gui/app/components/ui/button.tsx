import type { ButtonHTMLAttributes } from "react";
import { cn } from "~/lib/utils";

/** ボタンの見た目（docs/adr/0011 D2）。`<Link>` / `<a>` にも `buttonClass()` で同じ見た目を付けられる。 */
export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger" | "success" | "soft";
export type ButtonSize = "xs" | "sm" | "md";

const BASE =
  "inline-flex items-center justify-center gap-1.5 whitespace-nowrap rounded-lg font-medium no-underline transition-[background-color,border-color,color,box-shadow,transform] duration-150 select-none active:translate-y-px focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:pointer-events-none disabled:opacity-50 aria-disabled:pointer-events-none aria-disabled:opacity-50";

const VARIANTS: Record<ButtonVariant, string> = {
  primary:
    "bg-primary text-primary-fg shadow-sm hover:bg-primary-hover border border-transparent bg-linear-to-b from-white/10 to-transparent",
  secondary: "border border-border bg-surface text-fg shadow-xs hover:bg-surface-2 hover:border-border-strong",
  ghost: "border border-transparent text-fg-muted hover:bg-surface-2 hover:text-fg",
  danger:
    "border border-danger-border bg-danger-soft text-danger-soft-fg hover:bg-danger hover:text-white hover:border-danger",
  success:
    "border border-success-border bg-success-soft text-success-soft-fg hover:border-success hover:bg-success hover:text-white dark:hover:text-bg",
  soft: "border border-primary-border bg-primary-soft text-primary-soft-fg hover:border-primary",
};

const SIZES: Record<ButtonSize, string> = {
  xs: "h-7 px-2.5 text-xs [&_svg]:size-3.5",
  sm: "h-8 px-3 text-sm [&_svg]:size-4",
  md: "h-10 px-4 text-sm [&_svg]:size-4",
};

export function buttonClass({
  variant = "secondary",
  size = "sm",
  className,
}: {
  variant?: ButtonVariant;
  size?: ButtonSize;
  className?: string;
} = {}): string {
  return cn(BASE, VARIANTS[variant], SIZES[size], className);
}

export function Button({
  variant,
  size,
  className,
  type = "button",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: ButtonVariant; size?: ButtonSize }) {
  return <button type={type} className={buttonClass({ variant, size, className })} {...props} />;
}
