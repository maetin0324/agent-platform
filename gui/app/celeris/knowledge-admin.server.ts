import type { KnowledgeOpOutcome } from "./action-types";
import type { CelerisClient } from "./client.server";
import { toActionError } from "./errors";
import { formString } from "./forms";
import type { KnowledgeAcceptBody, KnowledgePagePutBody, KnowledgePageResult, KnowledgeRejectResult } from "./types";

/**
 * 知識ベースの変更系（ADR-0047 D5、docs/celeris-api-v1.md §3.100 / §3.102 / §3.103。**管理系**。
 * Phase 61 / G21）: ページを書く・候補を取り込む・候補を捨てる。
 *
 * 規律は他の `*-admin.server.ts` と同じ: **GUI 側で検証しない**（空欄はキーごと送らない）、
 * celeris のエラーは例外にせず `{ok:false, error}` にして文言をそのまま出す。衝突
 * （409 `etag_mismatch` / `page_exists` / `knowledge_unavailable`、403 `path_forbidden`）も同じ形で
 * 返るので、画面が `error.code` を見て案内する。**用意する経路は無い**（`celerisctl knowledge init` だけ）。
 */

/** `PUT /knowledge/page`（作業ツリーに書いて、そのパスだけを 1 コミット）。 */
export async function putKnowledgePage(
  client: CelerisClient,
  body: KnowledgePagePutBody,
  signal?: AbortSignal,
): Promise<KnowledgeOpOutcome> {
  try {
    const result = await client.put<KnowledgePageResult>("/knowledge/page", body, { signal });
    return { ok: true, op: "knowledge_put", result };
  } catch (e) {
    return { ok: false, op: "knowledge_put", error: toActionError(e) };
  }
}

/** `POST /knowledge/inbox/{id}/accept`（候補を KB に取り込む）。 */
export async function acceptKnowledgeCandidate(
  client: CelerisClient,
  id: string,
  body: KnowledgeAcceptBody,
  signal?: AbortSignal,
): Promise<KnowledgeOpOutcome> {
  try {
    const result = await client.post<KnowledgePageResult>(`/knowledge/inbox/${encodeURIComponent(id)}/accept`, body, {
      signal,
    });
    return { ok: true, op: "knowledge_accept", id, result };
  } catch (e) {
    return { ok: false, op: "knowledge_accept", id, error: toActionError(e) };
  }
}

/** `POST /knowledge/inbox/{id}/reject`（候補を消してコミットする。履歴には残る）。 */
export async function rejectKnowledgeCandidate(
  client: CelerisClient,
  id: string,
  signal?: AbortSignal,
): Promise<KnowledgeOpOutcome> {
  try {
    const result = await client.post<KnowledgeRejectResult>(
      `/knowledge/inbox/${encodeURIComponent(id)}/reject`,
      {},
      { signal },
    );
    return { ok: true, op: "knowledge_reject", id, result };
  } catch (e) {
    return { ok: false, op: "knowledge_reject", id, error: toActionError(e) };
  }
}

/** フォームから `PUT /knowledge/page` の本文を組む（空欄はキーごと送らない）。 */
export function readKnowledgePagePutBody(form: FormData): KnowledgePagePutBody {
  const body: KnowledgePagePutBody = {
    path: formString(form, "path") ?? "",
    body: form.get("body") === null ? "" : String(form.get("body")),
  };
  const etag = formString(form, "etag");
  if (etag) body.etag = etag;
  const message = formString(form, "message");
  if (message) body.message = message;
  return body;
}

/** フォームから accept の本文を組む（`path` が空なら候補の `target` に任せる = キーごと送らない）。 */
export function readKnowledgeAcceptBody(form: FormData): KnowledgeAcceptBody {
  const body: KnowledgeAcceptBody = {};
  const path = formString(form, "path");
  if (path) body.path = path;
  if (formString(form, "overwrite") === "1") body.overwrite = true;
  return body;
}
