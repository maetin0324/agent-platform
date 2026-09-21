/** フォーム部品のクラス（docs/adr/0011 D2）。要素・name・aria は各画面のまま、見た目だけを揃える。 */

const FIELD =
  "w-full rounded-lg border border-border bg-surface px-3 text-sm text-fg shadow-xs transition-[border-color,box-shadow] placeholder:text-fg-subtle hover:border-border-strong focus:border-primary focus:outline-none focus:ring-3 focus:ring-primary/20 disabled:cursor-not-allowed disabled:opacity-60 aria-invalid:border-danger aria-invalid:ring-danger/20";

// ADR-0055 D1-2: タップ領域 44×44 以上。モバイルは `h-11`、デスクトップは `lg:` で元の `h-9` に戻す。
export const inputClass = `${FIELD} h-11 lg:h-9`;
export const textareaClass = `${FIELD} py-2 leading-relaxed`;
export const selectClass = `${FIELD} h-11 lg:h-9 pr-8 appearance-none bg-[length:1rem] bg-[right_0.5rem_center] bg-no-repeat bg-[url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238a93a6' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='m6 9 6 6 6-6'/%3E%3C/svg%3E")]`;
/** 複数選択の `<select multiple>`（高さは行数に任せるので `h-9` も矢印も付けない）。 */
export const multiSelectClass = `${FIELD} py-1.5`;
export const labelClass = "text-sm font-medium text-fg";
// ADR-0055 D1-4: 本文 14px 以上。モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
export const hintClass = "text-sm text-fg-subtle lg:text-xs";
export const checkboxClass = "size-4 rounded border-border-strong";
/**
 * チェックボックス等を包む「チップ」型のラベル。押せる範囲は `<input>` 自身ではなくこの `<label>`
 * なので（`~/scripts/mobile-audit.mjs` の `checkTapTargets` もここを見る）、ADR-0055 D1-2 の
 * 44×44 はここで満たす（`min-h-11`。デスクトップは `lg:min-h-0` で元の高さに戻す）。
 */
export const chipLabelClass =
  "inline-flex min-h-11 cursor-pointer items-center gap-2 rounded-lg border border-border bg-surface px-2.5 py-1.5 text-sm text-fg-muted shadow-xs transition-colors hover:border-border-strong hover:text-fg has-[:checked]:border-primary-border has-[:checked]:bg-primary-soft has-[:checked]:text-primary-soft-fg lg:min-h-0";

/** 表 */
export const tableClass = "w-full border-collapse text-sm";
export const theadClass = "bg-surface-2/70 text-left text-xs font-medium text-fg-subtle";
export const thClass = "whitespace-nowrap px-3 py-2.5 font-medium first:pl-5 last:pr-5";
export const tdClass = "border-t border-border px-3 py-2.5 align-top text-fg first:pl-5 last:pr-5";
export const trHoverClass = "transition-colors hover:bg-surface-2/60";
