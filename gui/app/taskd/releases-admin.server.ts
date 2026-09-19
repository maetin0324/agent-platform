import type { ReleasePromoteOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type { ReleasePromoteAccepted } from "./types";

/**
 * 「リリース」画面からの昇格の中継（ADR-0040 D6、docs/taskd-api-v1.md §3.67。**管理系**、
 * `token_file` 未設定でも 401）。
 *
 * GUI 側では検証しない（`sha12` の有無だけ見る）: 昇格できるかどうかは taskd と `promote.sh` が
 * `verify.json.ok` で決める（`--force` は無い。ADR-0040 D2）。404 / 409 / 401 の文言はそのまま画面に出す。
 * `clusters-admin.server.ts` と同じく **`POST /reload` は呼ばない**（設定は変わらない）。
 *
 * **押すのは人だけ**（ADR-0040 D5）。この関数を自動で呼ぶ経路は GUI のどこにも作らない。
 */
export async function promoteRelease(
  client: TaskdClient,
  sha12: string,
  signal?: AbortSignal,
): Promise<ReleasePromoteOutcome> {
  try {
    const accepted = await client.post<ReleasePromoteAccepted>(
      `/releases/${encodeURIComponent(sha12)}/promote`,
      {},
      { signal },
    );
    return { ok: true, op: "release_promote", sha12, accepted };
  } catch (e) {
    return { ok: false, op: "release_promote", sha12, error: toActionError(e) };
  }
}

/** フォームから `sha12` を読む（無ければ空文字。taskd の 404 に任せる）。 */
export function readReleaseSha12(form: FormData): string {
  return formString(form, "sha12") ?? "";
}
