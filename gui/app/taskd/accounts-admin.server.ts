import type { AccountOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type { AccountCheckResponse, AccountLoginResult, AccountLoginStart, AccountView } from "./types";

/**
 * `/accounts` の追加・削除・確認・ログイン中継（ADR-GUI-0012 D3、docs/taskd-api-v1.md §3.29〜3.35）。
 * GUI 側では検証しない（`id` の有無だけ見る）。taskd の 404 / 409 / 422 / 502 の文言をそのまま画面に出す。
 */

export async function createAccount(client: TaskdClient, id: string, signal?: AbortSignal): Promise<AccountOpOutcome> {
  try {
    const account = await client.post<AccountView>("/accounts", { id }, { signal });
    return { ok: true, op: "create", id, account };
  } catch (e) {
    return { ok: false, op: "create", id, error: toActionError(e) };
  }
}

export async function deleteAccount(client: TaskdClient, id: string, signal?: AbortSignal): Promise<AccountOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/accounts/${encodeURIComponent(id)}`, { signal });
    return { ok: true, op: "delete", id };
  } catch (e) {
    return { ok: false, op: "delete", id, error: toActionError(e) };
  }
}

export async function checkAccount(client: TaskdClient, id: string, signal?: AbortSignal): Promise<AccountOpOutcome> {
  try {
    const result = await client.post<AccountCheckResponse>(
      `/accounts/${encodeURIComponent(id)}/check`,
      {},
      {
        signal,
      },
    );
    return { ok: true, op: "check", id, result };
  } catch (e) {
    return { ok: false, op: "check", id, error: toActionError(e) };
  }
}

/** `POST /accounts/{id}/login`。URL は戻り値（action の `data`）でだけ保持し、cookie/storage には置かない。 */
export async function startAccountLogin(
  client: TaskdClient,
  id: string,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    const login = await client.post<AccountLoginStart>(`/accounts/${encodeURIComponent(id)}/login`, {}, { signal });
    return { ok: true, op: "login_start", id, login };
  } catch (e) {
    return { ok: false, op: "login_start", id, error: toActionError(e) };
  }
}

/** `POST /accounts/{id}/login/code`。コードはログにも応答にも出ない（taskd 側）。 */
export async function submitAccountLoginCode(
  client: TaskdClient,
  id: string,
  code: string,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    const result = await client.post<AccountLoginResult>(
      `/accounts/${encodeURIComponent(id)}/login/code`,
      { code },
      { signal },
    );
    return { ok: true, op: "login_code", id, result };
  } catch (e) {
    return { ok: false, op: "login_code", id, error: toActionError(e) };
  }
}

/** `DELETE /accounts/{id}/login`。進行中のログインの中止。 */
export async function cancelAccountLogin(
  client: TaskdClient,
  id: string,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/accounts/${encodeURIComponent(id)}/login`, { signal });
    return { ok: true, op: "login_cancel", id };
  } catch (e) {
    return { ok: false, op: "login_cancel", id, error: toActionError(e) };
  }
}

/** フォームから `id` を読む（無ければ 空文字。taskd の 422/400 に任せる。GUI 側で検証しない）。 */
export function readAccountId(form: FormData): string {
  return formString(form, "id") ?? "";
}

/** フォームからログインコードを読む。 */
export function readLoginCode(form: FormData): string {
  return formString(form, "code") ?? "";
}
