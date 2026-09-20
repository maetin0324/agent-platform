import type { ProviderActionResult, ProviderCheckOutcome, ProviderOpOutcome, ReloadOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import { formString } from "./forms";
import type { ProviderCheckResponse, ProviderConfigView1, ReloadResult, Tier } from "./types";

/**
 * `/providers` の追加・編集・削除・疎通確認（ADR-GUI-0012 D2、docs/celeris-api-v1.md §3.24〜3.28）。
 * GUI 側では検証しない: celeris が 404 / 409 / 422 を返したらその文言をそのまま画面に出す（フィールドの有無だけ見る）。
 * 追加・変更・削除が 2xx なら続けて `POST /reload` を呼び、両方の結果を呼び出し側（action）に返す。
 */

const TIERS: readonly Tier[] = ["frontier", "standard", "cheap"];

export interface ProviderCreateInput {
  id: string;
  adapter: string;
  tiers?: Tier[];
  concurrency?: number;
  model?: string;
  account_id?: string;
  credential_refs?: Record<string, string>;
  tier_models?: Partial<Record<Tier, { name: string; model_id: string | null; unavailable_reason: string | null }>>;
  account_pool?: boolean;
  env?: Record<string, string>;
}

export interface ProviderPatchInput {
  tiers?: Tier[];
  concurrency?: number;
  model?: string;
  account_id?: string;
  credential_refs?: Record<string, string>;
  tier_models?: Partial<Record<Tier, { name: string; model_id: string | null; unavailable_reason: string | null }>>;
  account_pool?: boolean;
  env?: Record<string, string>;
}

/** `KEY=VALUE` の行（1 行 1 エントリ）を `Record<string,string>` にする。空行は無視。`=` が無い行はキーだけ・値は空文字。 */
export function parseEnvText(text: string): Record<string, string> | undefined {
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l !== "");
  if (lines.length === 0) return undefined;
  const env: Record<string, string> = {};
  for (const line of lines) {
    const eq = line.indexOf("=");
    if (eq === -1) env[line] = "";
    else env[line.slice(0, eq)] = line.slice(eq + 1);
  }
  return env;
}

function readTiers(form: FormData): Tier[] {
  const values = form.getAll("tiers").map((v) => String(v));
  return TIERS.filter((t) => values.includes(t));
}

function readNumber(form: FormData, name: string): number | undefined {
  const v = formString(form, name);
  if (v === null) return undefined;
  const n = Number(v);
  return Number.isNaN(n) ? undefined : n;
}

/** 新規プロバイダのフォーム（`id` / `adapter` / `tiers` / `concurrency` / `model` / `account_pool` / `env`）を組み立てる。 */
export function buildProviderCreateInput(form: FormData): ProviderCreateInput {
  const input: ProviderCreateInput = {
    id: formString(form, "id") ?? "",
    adapter: formString(form, "adapter") ?? "",
  };
  const tiers = readTiers(form);
  if (tiers.length > 0) input.tiers = tiers;
  const concurrency = readNumber(form, "concurrency");
  if (concurrency !== undefined) input.concurrency = concurrency;
  const model = formString(form, "model");
  if (model) input.model = model;
  if (form.get("account_pool") != null) input.account_pool = true;
  const env = parseEnvText(formString(form, "env") ?? "");
  if (env) input.env = env;
  if (form.has("routing_form")) {
    const credentialKey = formString(form, "credential_key");
    const credentialRef = formString(form, "credential_ref");
    // Only modify this credential when explicitly requested, preserving other legacy refs.
    if (form.has("update_credential_ref") && credentialKey)
      input.credential_refs = credentialRef ? { [credentialKey]: credentialRef } : {};

    input.account_id = formString(form, "account_id") ?? "";
    input.tier_models = form.has("tier_models_enabled")
      ? Object.fromEntries(
          TIERS.map((tier) => [
            tier,
            {
              name: formString(form, `name_${tier}`) ?? tier,
              model_id: formString(form, `model_${tier}`) || null,
              unavailable_reason: formString(form, `reason_${tier}`) || null,
            },
          ]),
        )
      : {};
    if (!input.account_id) delete input.account_id;
  }
  return input;
}

/**
 * 編集フォーム（既存の値をフィールドに事前入力してある前提で `tiers` / `concurrency` / `model` / `account_pool` は
 * 常に送る）。`env` だけ「空欄なら変更しない」（ADR-GUI-0012 D2）: 1 行も無ければ本文から省く。
 */
export function buildProviderPatchInput(form: FormData): ProviderPatchInput {
  const input: ProviderPatchInput = { tiers: readTiers(form) };
  const concurrency = readNumber(form, "concurrency");
  if (concurrency !== undefined) input.concurrency = concurrency;
  input.model = formString(form, "model") ?? "";
  if (form.get("account_pool") != null) input.account_pool = true;
  else input.account_pool = false;
  const env = parseEnvText(formString(form, "env") ?? "");
  if (env) input.env = env;
  if (form.has("routing_form")) {
    const credentialKey = formString(form, "credential_key");
    const credentialRef = formString(form, "credential_ref");
    // Only modify this credential when explicitly requested, preserving other legacy refs.
    if (form.has("update_credential_ref") && credentialKey)
      input.credential_refs = credentialRef ? { [credentialKey]: credentialRef } : {};

    input.account_id = formString(form, "account_id") ?? "";
    input.tier_models = form.has("tier_models_enabled")
      ? Object.fromEntries(
          TIERS.map((tier) => [
            tier,
            {
              name: formString(form, `name_${tier}`) ?? tier,
              model_id: formString(form, `model_${tier}`) || null,
              unavailable_reason: formString(form, `reason_${tier}`) || null,
            },
          ]),
        )
      : {};
  }
  return input;
}

async function reloadAfterMutation(client: CelerisClient, signal?: AbortSignal): Promise<ReloadOutcome> {
  try {
    const result = await client.post<ReloadResult>("/reload", {}, { signal });
    return { ok: true, result };
  } catch (e) {
    return { ok: false, error: toActionError(e) };
  }
}

export async function createProvider(
  client: CelerisClient,
  input: ProviderCreateInput,
  signal?: AbortSignal,
): Promise<ProviderActionResult> {
  try {
    const provider = await client.post<ProviderConfigView1>("/providers", input, { signal });
    const op: ProviderOpOutcome = { ok: true, op: "create", id: input.id, provider };
    return { op, reload: await reloadAfterMutation(client, signal) };
  } catch (e) {
    return { op: { ok: false, op: "create", id: input.id, error: toActionError(e) } };
  }
}

export async function patchProvider(
  client: CelerisClient,
  id: string,
  input: ProviderPatchInput,
  signal?: AbortSignal,
): Promise<ProviderActionResult> {
  try {
    const provider = await client.patch<ProviderConfigView1>(`/providers/${encodeURIComponent(id)}`, input, {
      signal,
    });
    const op: ProviderOpOutcome = { ok: true, op: "patch", id, provider };
    return { op, reload: await reloadAfterMutation(client, signal) };
  } catch (e) {
    return { op: { ok: false, op: "patch", id, error: toActionError(e) } };
  }
}

export async function deleteProvider(
  client: CelerisClient,
  id: string,
  signal?: AbortSignal,
): Promise<ProviderActionResult> {
  try {
    await client.delete<Record<string, never>>(`/providers/${encodeURIComponent(id)}`, { signal });
    const op: ProviderOpOutcome = { ok: true, op: "delete", id };
    return { op, reload: await reloadAfterMutation(client, signal) };
  } catch (e) {
    return { op: { ok: false, op: "delete", id, error: toActionError(e) } };
  }
}

/** `POST /providers/{id}/check`。reload は行わない（観測値を作るだけで設定は変わらない）。 */
export async function checkProvider(
  client: CelerisClient,
  id: string,
  signal?: AbortSignal,
): Promise<ProviderActionResult> {
  try {
    const result = await client.post<ProviderCheckResponse>(
      `/providers/${encodeURIComponent(id)}/check`,
      {},
      {
        signal,
      },
    );
    const op: ProviderCheckOutcome = { ok: true, op: "check", id, result };
    return { op };
  } catch (e) {
    return { op: { ok: false, op: "check", id, error: toActionError(e) } };
  }
}
