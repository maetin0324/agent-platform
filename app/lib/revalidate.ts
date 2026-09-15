import type { ShouldRevalidateFunctionArgs } from "react-router";

/**
 * action が 4xx / 5xx（409 `conflict` / `invalid_transition`、422 `validation`、503）を返した後も loader を再検証する
 * （docs/DESIGN.md §4.3「409 は『状態が変わりました』として再取得」、docs/adr/0005 D2）。
 * React Router の既定は「action が 4xx / 5xx を返したら再検証しない」なので、そのままだと 409 を受けた古いタブが
 * 古い状態（と押せない操作ボタン）を表示し続ける。それ以外は既定の判断に従う。
 */
export function revalidateAfterActionErrors(args: ShouldRevalidateFunctionArgs): boolean {
  if (args.actionStatus !== undefined && args.actionStatus >= 400) return true;
  return args.defaultShouldRevalidate;
}
