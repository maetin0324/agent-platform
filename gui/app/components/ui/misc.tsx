import { type HTMLAttributes, type ReactNode, useState } from "react";
import { cn } from "~/lib/utils";
import { Button } from "./button";
import { Icon, type IconName } from "./Icon";
import { TONE_ICON_WRAP, TONE_SOFT, type Tone } from "./tone";

/**
 * 画面の見出し（docs/adr/0011 D2）。見出しのレベルは画面ごとに従来のまま（`as`）。
 * `title` の中に HelpLink 等を入れてよい。`titleProps` は見出し要素に data-testid 等を付けるため。
 */
export function PageHeader({
  as: Heading = "h1",
  icon,
  eyebrow,
  title,
  description,
  actions,
  titleProps,
  className,
}: {
  as?: "h1" | "h2";
  icon?: IconName;
  eyebrow?: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  titleProps?: HTMLAttributes<HTMLHeadingElement> & Record<`data-${string}`, string>;
  className?: string;
}) {
  return (
    <div className={cn("flex flex-wrap items-end justify-between gap-4 pb-2", className)}>
      <div className="flex min-w-0 items-start gap-3.5">
        {icon && (
          <span className="mt-0.5 hidden size-11 shrink-0 place-items-center rounded-xl bg-linear-to-br from-primary to-teal text-white shadow-md sm:grid dark:text-bg">
            <Icon name={icon} className="size-5" strokeWidth={2} />
          </span>
        )}
        <div className="min-w-0">
          {eyebrow && (
            // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
            <div className="mb-0.5 text-sm font-semibold uppercase tracking-wider text-primary lg:text-xs">
              {eyebrow}
            </div>
          )}
          <Heading
            {...titleProps}
            className={cn(
              "flex flex-wrap items-center gap-x-1 text-2xl font-bold tracking-tight text-fg",
              titleProps?.className,
            )}
          >
            {title}
          </Heading>
          {description && <div className="mt-1 max-w-3xl text-sm text-fg-muted">{description}</div>}
        </div>
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
    </div>
  );
}

/** カードの外に置く節見出し（h2）。件数はピルで出す。 */
export function SectionTitle({
  icon,
  tone = "primary",
  count,
  children,
  className,
  ...props
}: HTMLAttributes<HTMLHeadingElement> & { icon?: IconName; tone?: Tone; count?: number }) {
  return (
    <h2 className={cn("flex items-center gap-2.5 text-base font-semibold text-fg", className)} {...props}>
      {icon && (
        <span className={cn("grid size-7 place-items-center rounded-lg", TONE_ICON_WRAP[tone])}>
          <Icon name={icon} className="size-3.5" strokeWidth={2.2} />
        </span>
      )}
      {children}
      {count !== undefined && (
        <span
          aria-hidden="true"
          className={cn(
            // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
            "rounded-full px-2 py-0.5 text-sm font-semibold tabular-nums lg:text-xs",
            count > 0 ? TONE_SOFT[tone] : "bg-surface-2 text-fg-subtle",
          )}
        >
          {count}
        </span>
      )}
    </h2>
  );
}

export function EmptyState({
  icon = "checkCircle",
  title,
  children,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement> & { icon?: IconName; title?: ReactNode }) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center gap-2 rounded-xl border border-dashed border-border-strong bg-surface/50 px-6 py-8 text-center",
        className,
      )}
      {...props}
    >
      <span className="grid size-10 place-items-center rounded-full bg-surface-2 text-fg-subtle ring-1 ring-border">
        <Icon name={icon} className="size-5" />
      </span>
      {title && <div className="text-sm font-medium text-fg">{title}</div>}
      {children && <div className="max-w-md text-sm text-fg-muted">{children}</div>}
    </div>
  );
}

const ALERT_ICON: Record<Tone, IconName> = {
  neutral: "info",
  primary: "info",
  info: "info",
  teal: "info",
  success: "checkCircle",
  warning: "alert",
  danger: "xCircle",
};

/** 通知の帯。`role` 等は呼び出し側で付ける（意味付けは従来どおり）。 */
export function Alert({
  tone = "info",
  icon,
  title,
  children,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement> & { tone?: Tone; icon?: IconName; title?: ReactNode }) {
  return (
    <div
      className={cn(
        "flex gap-3 rounded-xl border px-4 py-3 text-sm shadow-xs animate-fade-in",
        TONE_SOFT[tone],
        className,
      )}
      {...props}
    >
      <Icon name={icon ?? ALERT_ICON[tone]} className="mt-0.5 size-4.5" strokeWidth={2} />
      <div className="min-w-0 flex-1 space-y-1 [&_a]:underline [&_a]:underline-offset-2">
        {title && <div className="font-semibold">{title}</div>}
        {children}
      </div>
    </div>
  );
}

export function StatCard({
  label,
  value,
  icon,
  tone = "primary",
  hint,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement> & {
  label: ReactNode;
  value: ReactNode;
  icon?: IconName;
  tone?: Tone;
  hint?: ReactNode;
}) {
  return (
    <div
      className={cn("relative overflow-hidden rounded-xl border border-border bg-surface p-4 shadow-sm", className)}
      {...props}
    >
      <div className="flex items-center justify-between gap-2">
        <div className="text-xs font-medium text-fg-muted">{label}</div>
        {icon && (
          <span className={cn("grid size-7 place-items-center rounded-lg", TONE_ICON_WRAP[tone])}>
            <Icon name={icon} className="size-3.5" strokeWidth={2.2} />
          </span>
        )}
      </div>
      <div className="mt-2 text-2xl font-bold tabular-nums tracking-tight text-fg">{value}</div>
      {hint && <div className="mt-1 text-xs text-fg-subtle">{hint}</div>}
    </div>
  );
}

/** key / value の一覧（`<dl>`）。 */
export function DataList({ className, ...props }: HTMLAttributes<HTMLDListElement>) {
  return <dl className={cn("grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-4", className)} {...props} />;
}

export function DataItem({
  label,
  children,
  className,
  wide = false,
}: {
  label: ReactNode;
  children: ReactNode;
  className?: string;
  wide?: boolean;
}) {
  return (
    <div className={cn("min-w-0", wide && "sm:col-span-2", className)}>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは元の text-xs のまま。フェーズ 71: ラベルに
          API のフィールド名そのまま（`consecutive_reviewer_requeues` 等、区切りの無い長い 1 語）を渡す
          画面があり、2 列の狭い列幅では折り返せずに横はみ出しを起こしていた。`break-words` で単語の
          途中でも折り返せるようにする（id/sha は別途 `font-mono break-all` で扱うのでここでは変えない）。 */}
      <dt className="break-words text-sm font-medium text-fg-subtle lg:text-xs">{label}</dt>
      <dd className="mt-1 break-words text-sm text-fg">{children}</dd>
    </div>
  );
}

/** ID 等の等幅表示 */
export function Mono({ className, ...props }: HTMLAttributes<HTMLSpanElement>) {
  return <span className={cn("font-mono text-[0.8em] text-fg-muted", className)} {...props} />;
}

/**
 * クリップボードへコピーするボタン（Phase 84、`/accounts` の MCP 接続 URL ヒント用）。
 * **トークン等の秘密は絶対にここへ渡さない**（呼び出し側の規律。このコンポーネント自体は `value` を
 * そのままコピーするだけで中身を検査しない）。Clipboard API が使えない・拒否された環境では静かに諦める
 * （例外を投げない。`navigator.clipboard` はブラウザだけの API なので、クリックハンドラの中でだけ触る —
 * SSR では呼ばれない）。
 */
export function CopyButton({
  value,
  label = "コピー",
  className,
}: {
  value: string;
  label?: string;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);

  async function onClick() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard API が無い・許可が無い等。何もしない。
    }
  }

  return (
    <Button
      type="button"
      variant="ghost"
      size="xs"
      onClick={onClick}
      className={className}
      aria-label={copied ? "コピーしました" : `${label}をコピー`}
      data-testid="copy-button"
    >
      <Icon name={copied ? "check" : "copy"} />
      {copied ? "コピーしました" : label}
    </Button>
  );
}
