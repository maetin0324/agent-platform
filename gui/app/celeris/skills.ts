import type { ActionError } from "./action-types";
import type { CelerisClient } from "./client.server";
import { toActionError } from "./errors";
import type { SkillDetailView, SkillList } from "./types";

/**
 * skills の読み取りの中継（ADR-0056 D3 続き、docs/celeris-api-v1.md §3.112〜3.113。Phase 82 / G35）。
 * **正本は `[knowledge] root` の `skills/<name>/SKILL.md`**（celeris が KB のファイルを読む）ので、
 * GUI は持たず・作らず・直さない（`~/celeris/knowledge.ts` と同じ流儀）。
 */
export interface SkillsQuery {
  /** 開いている skill（無ければ一覧だけ）。 */
  name?: string | null;
  edit?: boolean;
  create?: boolean;
}

export interface SkillsData {
  /** `GET /skills`（409 `knowledge_unavailable` のときは `null`）。 */
  list: SkillList | null;
  /** 開いている skill（選んでいない・読めないときは `null`）。 */
  detail: SkillDetailView | null;
  listError: ActionError | null;
  detailError: ActionError | null;
  name: string | null;
  edit: boolean;
  create: boolean;
}

export async function loadSkills(
  client: CelerisClient,
  query: SkillsQuery = {},
  signal?: AbortSignal,
): Promise<SkillsData> {
  const name = query.name || null;
  const base = { name, edit: query.edit === true, create: query.create === true };
  let list: SkillList;
  try {
    list = await client.get<SkillList>("/skills", { signal });
  } catch (e) {
    return { ...base, list: null, detail: null, listError: toActionError(e), detailError: null };
  }
  if (!name) return { ...base, list, detail: null, listError: null, detailError: null };
  try {
    const detail = await client.get<SkillDetailView>(`/skills/${encodeURIComponent(name)}`, { signal });
    return { ...base, list, detail, listError: null, detailError: null };
  } catch (e) {
    return { ...base, list, detail: null, listError: null, detailError: toActionError(e) };
  }
}

/** `Request` の URL（`?name=&edit=&create=`）から問い合わせを組む。 */
export function readSkillsQuery(request: Request): SkillsQuery {
  const url = new URL(request.url);
  return {
    name: url.searchParams.get("name"),
    edit: url.searchParams.get("edit") === "1",
    create: url.searchParams.get("create") === "1",
  };
}
