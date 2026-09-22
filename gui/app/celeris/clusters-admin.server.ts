import type { ClusterConnectOutcome, ClusterSettingsOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import { formString } from "./forms";
import type { ClusterConnectResult, ClusterConnectStart, ClusterSettingsPutBody, ClusterSettingsView } from "./types";

/**
 * 「クラスタ」画面からの接続の中継（ADR-0032、docs/celeris-api-v1.md §3.39〜3.41）と、作業ディレクトリの
 * 登録・変更の中継（ADR-0059 D6、§3.107）。
 * GUI 側では検証しない（`id` の有無だけ見る）。celeris の 404 / 409 / 422 / 502 / 401 の文言をそのまま画面に出す。
 * `accounts-admin.server.ts` のログイン中継と同じ作りだが、**`POST /reload` は呼ばない**（接続を張っても・
 * `work_dir` を変えても `config.toml` の設定は変わらない。プロバイダや秘密の管理とはここが違う。
 * `secrets-admin.server.ts` / `providers-admin.server.ts` とは対照的に `reload` を呼ばないことをテストで確認する）。
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

/**
 * `PUT /clusters/{id}/settings`（ADR-0059 D6 §3.107）。`workDir` に `null` を渡すと DB の上書きを消す
 * （設定ファイルの値に戻る）。空文字は `null` にしない（celeris が 422 `validation` で拒む「それ以外・
 * 空文字」の経路をそのまま画面に出すため。「上書きを消す」ボタンだけが明示的に `null` を送る）。
 */
export async function putClusterSettings(
  client: CelerisClient,
  id: string,
  workDir: string | null,
  signal?: AbortSignal,
): Promise<ClusterSettingsOutcome> {
  try {
    const body: ClusterSettingsPutBody = { work_dir: workDir };
    const settings = await client.put<ClusterSettingsView>(`/clusters/${encodeURIComponent(id)}/settings`, body, {
      signal,
    });
    return { ok: true, op: "cluster_settings", id, settings };
  } catch (e) {
    return { ok: false, op: "cluster_settings", id, error: toActionError(e) };
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

/**
 * フォームから作業ディレクトリの入力欄を読む（「保存」ボタン用）。`formString` と違い**空文字を `null` に
 * しない**（空欄のまま保存を押したら celeris の 422 `validation` をそのまま見せるため。「消す」は別ボタン
 * が明示的に呼ぶ `putClusterSettings(client, id, null)`）。前後の空白だけ落とす。
 */
export function readClusterWorkDir(form: FormData): string {
  const v = form.get("work_dir");
  return typeof v === "string" ? v.trim() : "";
}
