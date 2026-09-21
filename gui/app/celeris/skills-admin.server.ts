import type { OrgSkillMountOutcome, SkillOpOutcome } from "./action-types";
import type { CelerisClient } from "./client.server";
import { toActionError } from "./errors";
import { formString } from "./forms";
import type { OrgNode, SkillFileBody, SkillPutBody, SkillPutResult } from "./types";

/**
 * skills の変更系（ADR-0056 D3 続き、docs/celeris-api-v1.md §3.114〜3.117。**管理系**。Phase 82 / G35）:
 * skill を作る・更新する・消す、ノードに mount する・外す。
 *
 * 規律は他の `*-admin.server.ts` と同じ: **GUI 側で検証しない**（celeris が 422 `validation` を返す文言を
 * そのまま出す）。celeris のエラーは例外にせず `{ok:false, error}` にする。mount / unmount は
 * celeris-mcp の `org_mount_skill`/`org_unmount_skill` と**同じ** `task_ops::knowledge::set_skill_mount`
 * を celeris 側が呼ぶので、GUI 経由でも挙動は同一。
 */

/** `PUT /skills/{name}`（作成・更新）。 */
export async function putSkill(
  client: CelerisClient,
  name: string,
  body: SkillPutBody,
  signal?: AbortSignal,
): Promise<SkillOpOutcome> {
  try {
    const result = await client.put<SkillPutResult>(`/skills/${encodeURIComponent(name)}`, body, { signal });
    return { ok: true, op: "skill_put", name, result };
  } catch (e) {
    return { ok: false, op: "skill_put", name, error: toActionError(e) };
  }
}

/** `DELETE /skills/{name}`（mount されている間は celeris が 409 `skill_mounted` で断る）。 */
export async function deleteSkill(client: CelerisClient, name: string, signal?: AbortSignal): Promise<SkillOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/skills/${encodeURIComponent(name)}`, { signal });
    return { ok: true, op: "skill_delete", name };
  } catch (e) {
    return { ok: false, op: "skill_delete", name, error: toActionError(e) };
  }
}

/** `POST /org/{id}/skills`（mount）。 */
export async function mountSkill(
  client: CelerisClient,
  id: string,
  skill: string,
  signal?: AbortSignal,
): Promise<OrgSkillMountOutcome> {
  try {
    const node = await client.post<OrgNode>(`/org/${encodeURIComponent(id)}/skills`, { skill }, { signal });
    return { ok: true, op: "skill_mount", id, skill, node };
  } catch (e) {
    return { ok: false, op: "skill_mount", id, skill, error: toActionError(e) };
  }
}

/** `DELETE /org/{id}/skills/{skill}`（unmount）。 */
export async function unmountSkill(
  client: CelerisClient,
  id: string,
  skill: string,
  signal?: AbortSignal,
): Promise<OrgSkillMountOutcome> {
  try {
    const node = await client.delete<OrgNode>(`/org/${encodeURIComponent(id)}/skills/${encodeURIComponent(skill)}`, {
      signal,
    });
    return { ok: true, op: "skill_unmount", id, skill, node };
  } catch (e) {
    return { ok: false, op: "skill_unmount", id, skill, error: toActionError(e) };
  }
}

/** フォームから `PUT /skills/{name}` の本文を組む（`files` は `path`/`content` の並行配列。空行は落とす）。 */
export function readSkillPutBody(form: FormData): SkillPutBody {
  const body: SkillPutBody = { skill_md: form.get("skill_md") === null ? "" : String(form.get("skill_md")) };
  const files = readSkillFiles(form);
  if (files.length > 0) body.files = files;
  return body;
}

function readSkillFiles(form: FormData): SkillFileBody[] {
  const paths = form.getAll("file_path").map((v) => String(v));
  const contents = form.getAll("file_content").map((v) => String(v));
  const out: SkillFileBody[] = [];
  for (const [i, rawPath] of paths.entries()) {
    const path = rawPath.trim();
    if (path === "") continue;
    out.push({ path, content: contents[i] ?? "" });
  }
  return out;
}

/** フォームから skill 名を読む（作成フォームの `name` 欄。トリムだけ）。 */
export function readSkillName(form: FormData): string {
  return formString(form, "name")?.trim() ?? "";
}
