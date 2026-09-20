import type { AccountAdapter, AccountOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import { formString } from "./forms";
import type { AccountCheckResponse, AccountLoginResult, AccountLoginStart, AccountView } from "./types";

/**
 * `/accounts` の追加・削除・確認・ログイン中継（ADR-GUI-0012 D3、docs/celeris-api-v1.md §3.29〜3.35、ADR-0025 D5/D6）。
 * GUI 側では検証しない（`id` の有無だけ見る）。celeris の 404 / 409 / 422 / 502 の文言をそのまま画面に出す。
 * `adapter` はどの呼び出しにも渡す（省略時 celeris 側の既定は `claude-code`。ADR-0025 D6）。
 */

export async function createAccount(
  client: CelerisClient,
  id: string,
  adapter: AccountAdapter,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    const account = await client.post<AccountView>("/accounts", { id, adapter }, { signal });
    return { ok: true, op: "create", id, adapter, account };
  } catch (e) {
    return { ok: false, op: "create", id, adapter, error: toActionError(e) };
  }
}

export async function deleteAccount(
  client: CelerisClient,
  id: string,
  adapter: AccountAdapter,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/accounts/${encodeURIComponent(id)}`, { signal, query: { adapter } });
    return { ok: true, op: "delete", id, adapter };
  } catch (e) {
    return { ok: false, op: "delete", id, adapter, error: toActionError(e) };
  }
}

export async function checkAccount(
  client: CelerisClient,
  id: string,
  adapter: AccountAdapter,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    const result = await client.post<AccountCheckResponse>(
      `/accounts/${encodeURIComponent(id)}/check`,
      {},
      { signal, query: { adapter } },
    );
    return { ok: true, op: "check", id, adapter, result };
  } catch (e) {
    return { ok: false, op: "check", id, adapter, error: toActionError(e) };
  }
}

/**
 * `POST /accounts/{id}/login`。URL・`user_code` は戻り値（action の `data`）でだけ保持し、cookie/storage には置かない
 * （ADR-0025 D5: `user_code` もログには出ない）。`kind` で claude-code（`paste_code`）と codex（`device_code`）が分岐する。
 */
export async function startAccountLogin(
  client: CelerisClient,
  id: string,
  adapter: AccountAdapter,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    const login = await client.post<AccountLoginStart>(
      `/accounts/${encodeURIComponent(id)}/login`,
      {},
      { signal, query: { adapter } },
    );
    return { ok: true, op: "login_start", id, adapter, login };
  } catch (e) {
    return { ok: false, op: "login_start", id, adapter, error: toActionError(e) };
  }
}

/**
 * `POST /accounts/{id}/login/code`。コードはログにも応答にも出ない（celeris 側）。claude-code のみ有効
 * （codex は 409 `login_code_not_supported`、ADR-0025 D5。呼び出し自体は行い、celeris の応答をそのまま返す）。
 */
export async function submitAccountLoginCode(
  client: CelerisClient,
  id: string,
  adapter: AccountAdapter,
  code: string,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    const result = await client.post<AccountLoginResult>(
      `/accounts/${encodeURIComponent(id)}/login/code`,
      { code },
      { signal, query: { adapter } },
    );
    return { ok: true, op: "login_code", id, adapter, result };
  } catch (e) {
    return { ok: false, op: "login_code", id, adapter, error: toActionError(e) };
  }
}

/** `DELETE /accounts/{id}/login`。進行中のログインの中止。 */
export async function cancelAccountLogin(
  client: CelerisClient,
  id: string,
  adapter: AccountAdapter,
  signal?: AbortSignal,
): Promise<AccountOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/accounts/${encodeURIComponent(id)}/login`, {
      signal,
      query: { adapter },
    });
    return { ok: true, op: "login_cancel", id, adapter };
  } catch (e) {
    return { ok: false, op: "login_cancel", id, adapter, error: toActionError(e) };
  }
}

/** フォームから `id` を読む（無ければ 空文字。celeris の 422/400 に任せる。GUI 側で検証しない）。 */
export function readAccountId(form: FormData): string {
  return formString(form, "id") ?? "";
}

/** フォームから `adapter` を読む（`"codex"` 以外は `"claude-code"` にする。celeris 既定と同じ、ADR-0025 D6）。 */
export function readAccountAdapter(form: FormData): AccountAdapter {
  return formString(form, "adapter") === "codex" ? "codex" : "claude-code";
}

/** フォームからログインコードを読む。 */
export function readLoginCode(form: FormData): string {
  return formString(form, "code") ?? "";
}
