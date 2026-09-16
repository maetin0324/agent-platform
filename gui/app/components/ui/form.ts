/** フォーム部品のクラス（docs/adr/0011 D2）。要素・name・aria は各画面のまま、見た目だけを揃える。 */

const FIELD =
  "w-full rounded-lg border border-border bg-surface px-3 text-sm text-fg shadow-xs transition-[border-color,box-shadow] placeholder:text-fg-subtle hover:border-border-strong focus:border-primary focus:outline-none focus:ring-3 focus:ring-primary/20 disabled:cursor-not-allowed disabled:opacity-60 aria-invalid:border-danger aria-invalid:ring-danger/20";

export const inputClass = `${FIELD} h-9`;
export const textareaClass = `${FIELD} py-2 leading-relaxed`;
export const selectClass = `${FIELD} h-9 pr-8 appearance-none bg-[length:1rem] bg-[right_0.5rem_center] bg-no-repeat bg-[url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238a93a6' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='m6 9 6 6 6-6'/%3E%3C/svg%3E")]`;
export const labelClass = "text-sm font-medium text-fg";
export const hintClass = "text-xs text-fg-subtle";
export const checkboxClass = "size-4 rounded border-border-strong";
/** チェックボックス等を包む「チップ」型のラベル */
export const chipLabelClass =
  "inline-flex cursor-pointer items-center gap-2 rounded-lg border border-border bg-surface px-2.5 py-1.5 text-sm text-fg-muted shadow-xs transition-colors hover:border-border-strong hover:text-fg has-[:checked]:border-primary-border has-[:checked]:bg-primary-soft has-[:checked]:text-primary-soft-fg";

/** 表 */
export const tableClass = "w-full border-collapse text-sm";
export const theadClass = "bg-surface-2/70 text-left text-xs font-medium text-fg-subtle";
export const thClass = "whitespace-nowrap px-3 py-2.5 font-medium first:pl-5 last:pr-5";
export const tdClass = "border-t border-border px-3 py-2.5 align-top text-fg first:pl-5 last:pr-5";
export const trHoverClass = "transition-colors hover:bg-surface-2/60";
