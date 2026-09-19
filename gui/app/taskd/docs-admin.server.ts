import type { DocsOpOutcome } from "./action-types";
import type { TaskdClient } from "./client.server";
import { toActionError } from "./errors";
import { formString } from "./forms";
import type { ArtifactPromoteBody, DocPagePutBody, DocPageResult, DocsInitResult } from "./types";

/**
 * 案件の文書の変更系（ADR-0044 D7、docs/taskd-api-v1.md §3.86〜3.89。**管理系**。Phase 57 / G19）:
 * 文書リポジトリを用意する・ページを書く・消す・成果物を昇格する。
 *
 * 規律は他の `*-admin.server.ts` と同じ: **GUI 側で検証しない**（空欄はキーごと送らない）、
 * taskd のエラーは例外にせず `{ok:false, error}` にして文言をそのまま出す。衝突（409 `etag_mismatch` /
 * `default_branch_busy` / `page_exists`）も同じ形で返るので、画面が `error.code` を見て案内する。
 */

/** `POST /projects/{id}/docs/init`（文書リポジトリを用意する）。 */
export async function initDocs(client: TaskdClient, projectId: string, signal?: AbortSignal): Promise<DocsOpOutcome> {
  try {
    const result = await client.post<DocsInitResult>(
      `/projects/${encodeURIComponent(projectId)}/docs/init`,
      {},
      { signal },
    );
    return { ok: true, op: "docs_init", result };
  } catch (e) {
    return { ok: false, op: "docs_init", error: toActionError(e) };
  }
}

/** `PUT /projects/{id}/docs/page`（default_branch に直接コミットする）。 */
export async function putDocPage(
  client: TaskdClient,
  projectId: string,
  body: DocPagePutBody,
  signal?: AbortSignal,
): Promise<DocsOpOutcome> {
  try {
    const result = await client.put<DocPageResult>(`/projects/${encodeURIComponent(projectId)}/docs/page`, body, {
      signal,
    });
    return { ok: true, op: "docs_put", result };
  } catch (e) {
    return { ok: false, op: "docs_put", error: toActionError(e) };
  }
}

/** `DELETE /projects/{id}/docs/page?path=&etag=`。 */
export async function deleteDocPage(
  client: TaskdClient,
  projectId: string,
  path: string,
  etag: string | null,
  signal?: AbortSignal,
): Promise<DocsOpOutcome> {
  try {
    const result = await client.delete<DocPageResult>(`/projects/${encodeURIComponent(projectId)}/docs/page`, {
      query: { path, etag: etag ?? undefined },
      signal,
    });
    return { ok: true, op: "docs_delete", result };
  } catch (e) {
    return { ok: false, op: "docs_delete", error: toActionError(e) };
  }
}

/** `POST /tasks/{id}/artifacts/promote`（成果物をページにする）。 */
export async function promoteArtifact(
  client: TaskdClient,
  taskId: string,
  body: ArtifactPromoteBody,
  signal?: AbortSignal,
): Promise<DocsOpOutcome> {
  try {
    const result = await client.post<DocPageResult>(`/tasks/${encodeURIComponent(taskId)}/artifacts/promote`, body, {
      signal,
    });
    return { ok: true, op: "docs_promote", result };
  } catch (e) {
    return { ok: false, op: "docs_promote", error: toActionError(e) };
  }
}

/** フォームから `PUT` の本文を組む（空欄はキーごと送らない）。 */
export function readDocPagePutBody(form: FormData): DocPagePutBody {
  const body: DocPagePutBody = {
    path: formString(form, "path") ?? "",
    body: form.get("body") === null ? "" : String(form.get("body")),
  };
  const etag = formString(form, "etag");
  if (etag) body.etag = etag;
  const message = formString(form, "message");
  if (message) body.message = message;
  return body;
}

/** フォームから昇格の本文を組む。 */
export function readArtifactPromoteBody(form: FormData): ArtifactPromoteBody {
  const body: ArtifactPromoteBody = {
    name: formString(form, "name") ?? "",
    path: formString(form, "path") ?? "",
  };
  const title = formString(form, "title");
  if (title) body.title = title;
  if (formString(form, "overwrite") === "1") body.overwrite = true;
  return body;
}
