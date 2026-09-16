/** 色の役割（docs/adr/0011 D1）。バッジ・アラート・アイコンの下地で共通に使う。 */
export type Tone = "neutral" | "primary" | "success" | "warning" | "danger" | "info" | "teal";

export const TONE_SOFT: Record<Tone, string> = {
  neutral: "bg-neutral-soft text-neutral-soft-fg border-neutral-border",
  primary: "bg-primary-soft text-primary-soft-fg border-primary-border",
  success: "bg-success-soft text-success-soft-fg border-success-border",
  warning: "bg-warning-soft text-warning-soft-fg border-warning-border",
  danger: "bg-danger-soft text-danger-soft-fg border-danger-border",
  info: "bg-info-soft text-info-soft-fg border-info-border",
  teal: "bg-teal-soft text-teal-soft-fg border-teal-border",
};

export const TONE_ICON_WRAP: Record<Tone, string> = {
  neutral: "bg-neutral-soft text-neutral-soft-fg ring-1 ring-inset ring-neutral-border",
  primary: "bg-primary-soft text-primary-soft-fg ring-1 ring-inset ring-primary-border",
  success: "bg-success-soft text-success-soft-fg ring-1 ring-inset ring-success-border",
  warning: "bg-warning-soft text-warning-soft-fg ring-1 ring-inset ring-warning-border",
  danger: "bg-danger-soft text-danger-soft-fg ring-1 ring-inset ring-danger-border",
  info: "bg-info-soft text-info-soft-fg ring-1 ring-inset ring-info-border",
  teal: "bg-teal-soft text-teal-soft-fg ring-1 ring-inset ring-teal-border",
};

/** ドットや左の帯に使う濃い色 */
export const TONE_SOLID_BG: Record<Tone, string> = {
  neutral: "bg-fg-subtle",
  primary: "bg-primary",
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-danger",
  info: "bg-info",
  teal: "bg-teal",
};

/** 左端のアクセント（`border-l-*`） */
export const TONE_ACCENT_BORDER: Record<Tone, string> = {
  neutral: "border-l-fg-subtle",
  primary: "border-l-primary",
  success: "border-l-success",
  warning: "border-l-warning",
  danger: "border-l-danger",
  info: "border-l-info",
  teal: "border-l-teal",
};
