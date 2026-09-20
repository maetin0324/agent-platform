/**
 * フォーム値の読み取り（純粋関数。サーバ・クライアントのどちらからも import できる）。
 * `.server` に置かないのは、ルートファイルの route 以外の export（`buildNewTaskSpec` 等）がこれを使うため
 * （React Router は `loader` / `action` 以外の export からは server-only モジュールへの参照を消せない）。
 */

/** FormData の文字列値（無ければ `null`、空文字は `null`）。 */
export function formString(form: FormData, name: string): string | null {
  const v = form.get(name);
  if (typeof v !== "string") return null;
  return v === "" ? null : v;
}
