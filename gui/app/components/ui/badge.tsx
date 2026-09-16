import type { HTMLAttributes } from "react";
import { cn } from "~/lib/utils";
import { TONE_SOFT, TONE_SOLID_BG, type Tone } from "./tone";

export function Badge({
  tone = "neutral",
  dot = false,
  pulse = false,
  className,
  children,
  ...props
}: HTMLAttributes<HTMLSpanElement> & { tone?: Tone; dot?: boolean; pulse?: boolean }) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 whitespace-nowrap rounded-full border px-2 py-0.5 text-xs font-medium leading-4",
        TONE_SOFT[tone],
        className,
      )}
      {...props}
    >
      {dot && (
        <span
          aria-hidden="true"
          className={cn("size-1.5 shrink-0 rounded-full", TONE_SOLID_BG[tone], pulse && "animate-pulse-dot")}
        />
      )}
      {children}
    </span>
  );
}

/** status → 色（docs/adr/0011 D4）。文字列は status 名をそのまま出す（色だけに頼らない）。 */
export const STATUS_TONE: Record<string, Tone> = {
  draft: "neutral",
  ready: "info",
  running: "primary",
  blocked: "warning",
  reviewing: "teal",
  done: "success",
  failed: "danger",
  cancelled: "neutral",
};

export function statusTone(status: string): Tone {
  return STATUS_TONE[status] ?? "neutral";
}

export function StatusBadge({ status, className, ...props }: HTMLAttributes<HTMLSpanElement> & { status: string }) {
  return (
    <Badge
      tone={statusTone(status)}
      dot
      pulse={status === "running"}
      className={cn(status === "cancelled" && "opacity-80", className)}
      {...props}
    >
      {status}
    </Badge>
  );
}

/** kind（execute / plan / approval …）。色分けはせず等幅の中立ラベル。 */
export function KindBadge({ kind, className, ...props }: HTMLAttributes<HTMLSpanElement> & { kind: string }) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-md bg-surface-2 px-1.5 py-0.5 font-mono text-[0.7rem] font-medium text-fg-muted ring-1 ring-inset ring-border",
        className,
      )}
      {...props}
    >
      {kind}
    </span>
  );
}

/** 役割（role）。DESIGN §10 Phase G7「色分けはせず、テキストのラベル」— 全役割で同じ見た目。 */
export function RoleLabel({ role, className, ...props }: HTMLAttributes<HTMLSpanElement> & { role: string }) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-md border border-dashed border-border-strong px-1.5 py-0.5 text-[0.7rem] font-medium text-fg-muted",
        className,
      )}
      {...props}
    >
      {role}
    </span>
  );
}
