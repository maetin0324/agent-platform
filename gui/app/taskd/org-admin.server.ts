import type { OrgOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type { OrgCreateBody, OrgKind, OrgNode, OrgPatchBody } from "./types";

/**
 * 「組織」画面（`/org`）からの追加・変更・削除（ADR-0033 D1、docs/taskd-api-v1.md §3.43〜3.45。**管理系**）。
 * GUI 側では検証しない: taskd が 404 / 409 / 422 / 401 を返したらその文言をそのまま画面に出す
 * （`providers-admin.server.ts` と同じ作りだが、組織は DB が正なので `POST /reload` は呼ばない
 * — `clusters-admin.server.ts` と同じ対比）。
 */

const ORG_KINDS: readonly OrgKind[] = ["secretary", "department", "section"];

function readOrgKind(form: FormData): OrgKind {
  const v = formString(form, "kind");
  return v && (ORG_KINDS as readonly string[]).includes(v) ? (v as OrgKind) : "section";
}

function readPosition(form: FormData): number | undefined {
  const v = formString(form, "position");
  if (v === null) return undefined;
  const n = Number(v);
  return Number.isNaN(n) ? undefined : n;
}

/** 追加フォーム（`id` / `name` / `kind` / `parent_id` / `genre` / `brief` / `position`）を組み立てる。 */
export function buildOrgCreateInput(form: FormData): OrgCreateBody {
  const body: OrgCreateBody = {
    id: formString(form, "id") ?? "",
    name: formString(form, "name") ?? "",
    kind: readOrgKind(form),
  };
  const parentId = formString(form, "parent_id");
  if (parentId) body.parent_id = parentId;
  const genre = formString(form, "genre");
  if (genre) body.genre = genre;
  const brief = formString(form, "brief");
  if (brief) body.brief = brief;
  const position = readPosition(form);
  if (position !== undefined) body.position = position;
  return body;
}

/**
 * 編集フォーム（既存の値をフィールドに事前入力してある前提）。`name` / `kind` / `parent_id` / `brief` /
 * `position` は常に送る（`providers-admin.server.ts` の編集フォームと同じ扱い）。`genre` は
 * 空の選択肢（`""`）を選べば明示的に `null`（分野なし）を送り、それ以外は選んだ値を送る（3.44 の
 * `Option<Option<String>>`。`docs/gui/api.md` §3.44）。
 */
export function buildOrgPatchInput(form: FormData): OrgPatchBody {
  const body: OrgPatchBody = {
    name: formString(form, "name") ?? undefined,
    kind: readOrgKind(form),
    brief: formString(form, "brief") ?? "",
  };
  const parentId = form.get("parent_id");
  body.parent_id = parentId === "" ? null : (formString(form, "parent_id") ?? undefined);
  const genre = form.get("genre");
  body.genre = genre === "" ? null : (formString(form, "genre") ?? undefined);
  const position = readPosition(form);
  if (position !== undefined) body.position = position;
  return body;
}

export async function createOrgNode(
  client: TaskdClient,
  input: OrgCreateBody,
  signal?: AbortSignal,
): Promise<OrgOpOutcome> {
  try {
    const node = await client.post<OrgNode>("/org", input, { signal });
    return { ok: true, op: "create", id: node.id, node };
  } catch (e) {
    return { ok: false, op: "create", id: input.id, error: toActionError(e) };
  }
}

export async function patchOrgNode(
  client: TaskdClient,
  id: string,
  input: OrgPatchBody,
  signal?: AbortSignal,
): Promise<OrgOpOutcome> {
  try {
    const node = await client.patch<OrgNode>(`/org/${encodeURIComponent(id)}`, input, { signal });
    return { ok: true, op: "patch", id, node };
  } catch (e) {
    return { ok: false, op: "patch", id, error: toActionError(e) };
  }
}

export async function deleteOrgNode(client: TaskdClient, id: string, signal?: AbortSignal): Promise<OrgOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/org/${encodeURIComponent(id)}`, { signal });
    return { ok: true, op: "delete", id };
  } catch (e) {
    return { ok: false, op: "delete", id, error: toActionError(e) };
  }
}
