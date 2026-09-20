import type { ClusterConnectOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import { formString } from "./forms";
import type { ClusterConnectResult, ClusterConnectStart } from "./types";

/**
 * 「クラスタ」画面からの接続の中継（ADR-0032、docs/celeris-api-v1.md §3.39〜3.41）。
 * GUI 側では検証しない（`id` の有無だけ見る）。celeris の 404 / 409 / 422 / 502 / 401 の文言をそのまま画面に出す。
 * `accounts-admin.server.ts` のログイン中継と同じ作りだが、**`POST /reload` は呼ばない**（接続を張っても
 * `config.toml` の設定は変わらない。プロバイダや秘密の管理とはここが違う。`secrets-admin.server.ts` / `providers-admin.server.ts`
 * とは対照的に `reload` を呼ばないことをテストで確認する）。
 */

/** `POST /clusters/{id}/connect`（ADR-0032 D5 3.39）。 */
export async function startClusterConnect(
  client: CelerisClient,
  id: string,
  signal?: AbortSignal,
): Promise<ClusterConnectOutcome> {
  try {
    const start = await client.post<ClusterConnectStart>(`/clusters/${encodeURIComponent(id)}/connect`, {}, { signal });
    return { ok: true, op: "connect_start", id, start };
  } catch (e) {
    return { ok: false, op: "connect_start", id, error: toActionError(e) };
  }
}

/**
 * `POST /clusters/{id}/connect/code`（ADR-0032 D4/D5 3.40）。コードは受け取ってもこの関数の戻り値・
 * ログのどこにも残さない（`{code}` は要求本文にだけ現れる）。
 */
export async function submitClusterConnectCode(
  client: CelerisClient,
  id: string,
  code: string,
  signal?: AbortSignal,
): Promise<ClusterConnectOutcome> {
  try {
    const result = await client.post<ClusterConnectResult>(
      `/clusters/${encodeURIComponent(id)}/connect/code`,
      { code },
      { signal },
    );
    return { ok: true, op: "connect_code", id, result };
  } catch (e) {
    return { ok: false, op: "connect_code", id, error: toActionError(e) };
  }
}

/** `DELETE /clusters/{id}/connect`（3.41）。進行中のセッションの取り消し、または既に張った接続の切断の両方に使う。 */
export async function cancelClusterConnect(
  client: CelerisClient,
  id: string,
  signal?: AbortSignal,
): Promise<ClusterConnectOutcome> {
  try {
    await client.delete<Record<string, never>>(`/clusters/${encodeURIComponent(id)}/connect`, { signal });
    return { ok: true, op: "connect_cancel", id };
  } catch (e) {
    return { ok: false, op: "connect_cancel", id, error: toActionError(e) };
  }
}

/** フォームから `id` を読む（無ければ空文字。celeris の 404 に任せる。GUI 側で検証しない）。 */
export function readClusterId(form: FormData): string {
  return formString(form, "id") ?? "";
}

/** フォームから検証コードを読む。 */
export function readClusterConnectCode(form: FormData): string {
  return formString(form, "code") ?? "";
}
