import type { ReloadOutcome, SecretActionResult, SecretOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type { ReloadResult, SecretList, SecretPutResult } from "./types";

/**
 * `/accounts` の「API キー」節の一覧・追加/更新・削除の中継（ADR-0030 D3/D4、docs/taskd-api-v1.md §3.36〜3.38）。
 * GUI 側では検証しない: taskd が 404 / 409 / 422 / 401 を返したらその文言をそのまま画面に出す。**値は
 * どこにも保持・再表示しない**（フォームは `type="password"` の入力から直接 `PUT` に渡すだけ）。
 * 追加・更新・削除が 2xx なら続けて `POST /reload` を呼び、両方の結果を呼び出し側（action）に返す
 * （プロバイダ管理と同じ作り、ADR-GUI-0012 D2）。
 */

async function reloadAfterMutation(client: TaskdClient, signal?: AbortSignal): Promise<ReloadOutcome> {
  try {
    const result = await client.post<ReloadResult>("/reload", {}, { signal });
    return { ok: true, result };
  } catch (e) {
    return { ok: false, error: toActionError(e) };
  }
}

/** `GET /secrets`。値は含まれない。`items[]` はファイルとして存在する秘密だけ（未設定の id は `used_by` から推測する）。 */
export async function listSecrets(client: TaskdClient, signal?: AbortSignal): Promise<SecretList> {
  return client.get<SecretList>("/secrets", { signal });
}

/** `PUT /secrets/{id}`（作成・置き換えの両方）。空/空白の値・無効な id は taskd が 422/404 で拒否する。 */
export async function putSecret(
  client: TaskdClient,
  id: string,
  value: string,
  signal?: AbortSignal,
): Promise<SecretActionResult> {
  try {
    const secret = await client.put<SecretPutResult>(`/secrets/${encodeURIComponent(id)}`, { value }, { signal });
    const op: SecretOpOutcome = { ok: true, op: "put", id, secret };
    return { op, reload: await reloadAfterMutation(client, signal) };
  } catch (e) {
    return { op: { ok: false, op: "put", id, error: toActionError(e) } };
  }
}

/** `DELETE /secrets/{id}`。無ければ 404 `secret_not_found`。 */
export async function deleteSecret(client: TaskdClient, id: string, signal?: AbortSignal): Promise<SecretActionResult> {
  try {
    await client.delete<Record<string, never>>(`/secrets/${encodeURIComponent(id)}`, { signal });
    const op: SecretOpOutcome = { ok: true, op: "delete", id };
    return { op, reload: await reloadAfterMutation(client, signal) };
  } catch (e) {
    return { op: { ok: false, op: "delete", id, error: toActionError(e) } };
  }
}

/** フォームから `id` を読む（無ければ空文字。taskd の 404/422 に任せる。GUI 側で検証しない）。 */
export function readSecretId(form: FormData): string {
  return formString(form, "id") ?? "";
}

/** フォームから `value` を読む（無ければ空文字。空白だけの値は taskd が 422 で拒否する）。 */
export function readSecretValue(form: FormData): string {
  return formString(form, "value") ?? "";
}
