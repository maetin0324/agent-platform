/** フォーム部品のクラス（docs/adr/0011 D2）。要素・name・aria は各画面のまま、見た目だけを揃える。 */

// Phase 76（ADR-0055 D1 拡張、フォーカスの可視性）: 以前は `focus:ring-3 focus:ring-primary/20` だけで、
// 不透明度 20% の box-shadow リングはトークンの背景（surface/bg）に対して実測 1.3:1 前後しか無く
// WCAG の 3:1 を満たさなかった（`docs/PROGRESS.md` Phase 76 参照、Python で実測）。`~/components/ui/button.tsx`
// と同じ `focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring`
// （不透明な `--ring` トークン、light 4:1 以上・dark 5.7:1 以上）に揃える。`focus:border-primary` は
// マウスでのクリック時も含めて枠線の色が変わる見た目としてそのまま残す（アクセシビリティ上の問題では無い）。
const FIELD =
  "w-full rounded-lg border border-border bg-surface px-3 text-sm text-fg shadow-xs transition-[border-color,box-shadow] placeholder:text-fg-subtle hover:border-border-strong focus:border-primary focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring disabled:cursor-not-allowed disabled:opacity-60 aria-invalid:border-danger aria-invalid:ring-danger/20";

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

/**
 * ADR-0055 D1-2（ラウンド 2）: 文中に単独で置かれたリンク（「同じ行の隣接リンク群」の例外
 * `data-touch-ok` とは違い、孤立した 1 本のリンク）の当たり判定を 44×44 に広げる。上下は
 * `-my-2.5`/`py-2.5` で相殺して見た目の行間は変えず、横幅が足りない語（2〜3 文字）は `min-w-11` で
 * 確保する（見た目の幅は文字のまま。当たり判定だけが見えない分だけ広がる）。
 */
export const touchLinkClass = "-my-2.5 inline-flex min-h-11 min-w-11 items-center py-2.5";

/** 表 */
export const tableClass = "w-full border-collapse text-sm";
// ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
export const theadClass = "bg-surface-2/70 text-left text-sm font-medium text-fg-subtle lg:text-xs";
export const thClass = "whitespace-nowrap px-3 py-2.5 font-medium first:pl-5 last:pr-5";
export const tdClass = "border-t border-border px-3 py-2.5 align-top text-fg first:pl-5 last:pr-5";
export const trHoverClass = "transition-colors hover:bg-surface-2/60";
