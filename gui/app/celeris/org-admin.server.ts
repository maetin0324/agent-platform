import type { OrgOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import { formString } from "./forms";
import type {
  KnowledgeMount,
  MountKind,
  OrgCreateBody,
  OrgKind,
  OrgNode,
  OrgPatchBody,
  Profile,
  ProfileRun,
  Tier,
} from "./types";

/**
 * 「組織」画面（`/org`）からの追加・変更・削除（ADR-0033 D1、docs/celeris-api-v1.md §3.43〜3.45。**管理系**）。
 * GUI 側では検証しない: celeris が 404 / 409 / 422 / 401 を返したらその文言をそのまま画面に出す
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

/** 1 行 1 件のテキスト欄（`policy` / `permissions.approvals`）。空行は落とす。 */
function readLines(form: FormData, name: string): string[] {
  return (formString(form, name) ?? "")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

/** 空白かカンマ区切りの自由記述（`cluster:<id>` のように選択肢にできない道具）。 */
function readWords(form: FormData, name: string): string[] {
  return (formString(form, name) ?? "")
    .split(/[\s,]+/)
    .map((word) => word.trim())
    .filter((word) => word !== "");
}

/** 複数選択（`<select multiple>` / チップの hidden）。空の値（番兵）は落とし、重複も落とす。 */
function readList(form: FormData, name: string): string[] {
  return [
    ...new Set(
      form
        .getAll(name)
        .map((v) => String(v).trim())
        .filter((v) => v !== ""),
    ),
  ];
}

/**
 * 知識のマウント（`profile_knowledge_*` の 5 本の並行配列を行ごとに組み直す）。
 * `kind` が空の行は「まだ書いていない行」として落とす（`KnowledgeMount.kind` は必須）。
 */
function readKnowledge(form: FormData): KnowledgeMount[] {
  const kinds = form.getAll("profile_knowledge_kind").map((v) => String(v).trim());
  const cell = (name: string, i: number): string | undefined => {
    const v = form.getAll(name)[i];
    const text = typeof v === "string" ? v.trim() : "";
    return text === "" ? undefined : text;
  };
  const rows: KnowledgeMount[] = [];
  for (const [i, kind] of kinds.entries()) {
    if (kind === "") continue;
    const row: KnowledgeMount = { kind: kind as MountKind };
    const scope = cell("profile_knowledge_scope", i);
    if (scope !== undefined) row.scope = scope;
    const name = cell("profile_knowledge_name", i);
    if (name !== undefined) row.name = name;
    const path = cell("profile_knowledge_path", i);
    if (path !== undefined) row.path = path;
    const docs = cell("profile_knowledge_docs", i);
    if (docs !== undefined) row.docs = docs;
    rows.push(row);
  }
  return rows;
}

/**
 * profile の編集フォーム → `Profile`（ADR-0046 D1、docs/celeris-api-v1.md §3.43 / §3.44）。
 * **`profile` は丸ごと差し替え**（部分更新は無い）なので、フォームに出ている値だけで全体を組み直す。
 * 空の項目は**書かない**（`{}` = 空の profile ＝ 何も持たない担当）。値の検証は celeris がする
 * （知らない道具・ハーネス・段は 422 `validation`。GUI 側では弾かない）。
 */
export function buildProfileInput(form: FormData): Profile {
  const profile: Profile = {};

  // 能力タグは開いた語彙（celeris の設定に無い）なので、道具の `_extra` 欄と同じ空白/カンマ区切りの
  // 自由記述の 1 本の欄で受ける（チェックボックスにできる固定の選択肢が無いため）。
  const skills = readWords(form, "profile_skills");
  if (skills.length > 0) profile.skills = skills;

  const knowledge = readKnowledge(form);
  if (knowledge.length > 0) profile.knowledge = knowledge;

  const allowed = readList(form, "profile_harnesses_allowed");
  const harnessDefault = formString(form, "profile_harness_default");
  if (allowed.length > 0 || harnessDefault !== null) {
    profile.harnesses = {};
    if (allowed.length > 0) profile.harnesses.allowed = allowed;
    if (harnessDefault !== null) profile.harnesses.default = harnessDefault;
  }

  const tools = [...new Set([...readList(form, "profile_tools"), ...readWords(form, "profile_tools_extra")])];
  if (tools.length > 0) profile.tools = tools;
  const denyTools = [
    ...new Set([...readList(form, "profile_deny_tools"), ...readWords(form, "profile_deny_tools_extra")]),
  ];
  if (denyTools.length > 0) profile.deny_tools = denyTools;

  const run = formString(form, "profile_run");
  if (run !== null) profile.run = run as ProfileRun;

  const tier = formString(form, "profile_model_tier");
  const allowedTiers = readList(form, "profile_model_allowed_tiers");
  if (tier !== null || allowedTiers.length > 0) {
    profile.model = {};
    if (tier !== null) profile.model.tier = tier as Tier;
    if (allowedTiers.length > 0) profile.model.allowed_tiers = allowedTiers as Tier[];
  }

  const policy = readLines(form, "profile_policy");
  if (policy.length > 0) profile.policy = policy;

  const reviewHarness = formString(form, "profile_review_harness");
  const reviewTier = formString(form, "profile_review_tier");
  if (reviewHarness !== null || reviewTier !== null) {
    profile.review = {};
    if (reviewHarness !== null) profile.review.harness = reviewHarness;
    if (reviewTier !== null) profile.review.tier = reviewTier as Tier;
  }

  const approvals = readLines(form, "profile_approvals");
  if (approvals.length > 0) profile.permissions = { approvals };

  return profile;
}

/**
 * 編集フォーム（既存の値をフィールドに事前入力してある前提）。`genre` は空の選択肢（`""`）を選べば
 * 明示的に `null`（分野なし）を送り、それ以外は選んだ値を送る（3.44 の `Option<Option<String>>`。
 * `docs/gui/api.md` §3.44）。
 *
 * **フォームに出ていない項目は本文に入れない**（3.44 は「書いた項目だけ」を変える）。名前・種類・一言の
 * フォームと profile のフォームは別々に送れるので、片方を送ったときにもう片方を書き換えてしまわないため
 * （`kind` は既定が `section` なので、書かずに送ると部や秘書を課に変えてしまう）。
 * `profile` は hidden の `profile_present` が付いているときだけ組み立てる（§3.44 の「丸ごと差し替え」）。
 */
export function buildOrgPatchInput(form: FormData): OrgPatchBody {
  const body: OrgPatchBody = {};
  if (form.has("name")) body.name = formString(form, "name") ?? undefined;
  if (form.has("kind")) body.kind = readOrgKind(form);
  if (form.has("brief")) body.brief = formString(form, "brief") ?? "";
  if (form.has("parent_id")) {
    const parentId = form.get("parent_id");
    body.parent_id = parentId === "" ? null : (formString(form, "parent_id") ?? undefined);
  }
  if (form.has("genre")) {
    const genre = form.get("genre");
    body.genre = genre === "" ? null : (formString(form, "genre") ?? undefined);
  }
  const position = readPosition(form);
  if (position !== undefined) body.position = position;
  if (form.has("profile_present")) body.profile = buildProfileInput(form);
  return body;
}

export async function createOrgNode(
  client: CelerisClient,
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
  client: CelerisClient,
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

export async function deleteOrgNode(client: CelerisClient, id: string, signal?: AbortSignal): Promise<OrgOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/org/${encodeURIComponent(id)}`, { signal });
    return { ok: true, op: "delete", id };
  } catch (e) {
    return { ok: false, op: "delete", id, error: toActionError(e) };
  }
}
