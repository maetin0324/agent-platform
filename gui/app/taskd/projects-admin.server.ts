import { readWorkspaceFromForm } from "~/lib/workspace-form";
import type { CreateFailure, ProjectOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type {
  Milestone,
  MilestoneCreateBody,
  MilestoneDecideBody,
  MilestoneDecided,
  MilestonePatchBody,
  MilestoneStatus,
  Project,
  ProjectCreateBody,
  ProjectPatchBody,
  ProjectPlanAccepted,
  ProjectPlanBody,
  ProjectStatus,
  WorkspaceSpec,
} from "./types";

/**
 * 「案件」画面（`/projects`, `/projects/:id`）からの中継（ADR-0033 D2、docs/taskd-api-v1.md §3.46〜3.49）。
 * `POST /projects`（案件の作成）は Phase 27（M-4）で**管理系**になった（`token_file` 未設定でも 401。
 * v1 の破壊的変更、docs/gui/api.md 冒頭の変更点一覧）。`GET /projects` / `PATCH /projects/{id}` /
 * `POST /projects/{id}/milestones` / `PATCH /milestones/{id}` は引き続き通常の要求（トークンを設定した
 * taskd では他の全要求と同じくトークンが要る）。管理系かどうかで GUI 側の中継コードは変わらない
 * （`TaskdClient` はどちらも同じ `Authorization` ヘッダを付けるだけ。401 の案内文も `Flash.tsx` の
 * `error.code === "unauthorized"` の 1 か所に集約されているので、管理系になっても揃っている）。
 * GUI 側では検証しない: taskd が 401 / 404 / 422 / 400 を返したらその文言をそのまま画面に出す。
 */

/** `POST /projects`（管理系）。`title` / `request` は空でもそのまま送り、taskd の 422 文言を出す（ADR-0005 D5）。 */
export async function createProject(
  client: TaskdClient,
  input: ProjectCreateBody,
  signal?: AbortSignal,
): Promise<{ ok: true; project: Project } | CreateFailure> {
  try {
    const project = await client.post<Project>("/projects", input, { signal });
    return { ok: true, project };
  } catch (e) {
    return { ok: false, error: toActionError(e) };
  }
}

/**
 * フォーム（`title` / `request` / `workspace_kind` / `workspace_path` / `workspace_cluster`）から
 * `ProjectCreateBody` を読む。作業場所が「まだ決めない」（省略を含む）なら `workspace` キー自体を送らない
 * （docs/taskd-api-v1.md §3.46「省略すれば従来どおり作業場所なし」。ADR-0039 D1、Phase G13k）。
 */
export function readProjectCreateInput(form: FormData): ProjectCreateBody {
  const body: ProjectCreateBody = {
    title: formString(form, "title") ?? "",
    request: formString(form, "request") ?? "",
  };
  const workspace = readWorkspaceFromForm(form);
  if (workspace) body.workspace = workspace;
  return body;
}

/** `PATCH /projects/{id}`（案件の状態変更。ADR-0033 D2 の `proposed`/`active`/`paused`/`done`）。 */
export async function patchProjectStatus(
  client: TaskdClient,
  id: string,
  status: ProjectStatus,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const body: ProjectPatchBody = { status };
    const project = await client.patch<Project>(`/projects/${encodeURIComponent(id)}`, body, { signal });
    return { ok: true, op: "project_status", project };
  } catch (e) {
    return { ok: false, op: "project_status", error: toActionError(e) };
  }
}

/**
 * `PATCH /projects/{id}`（案件の作業場所だけを変える。ADR-0039 D1、Phase G13k）。`workspace = null` を
 * 明示すると「作業場所なし」に戻す（消去。docs/taskd-api-v1.md §3.48）。`status` は送らない（変えない）。
 */
export async function patchProjectWorkspace(
  client: TaskdClient,
  id: string,
  workspace: WorkspaceSpec | null,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const body: ProjectPatchBody = { workspace };
    const project = await client.patch<Project>(`/projects/${encodeURIComponent(id)}`, body, { signal });
    return { ok: true, op: "project_workspace", project };
  } catch (e) {
    return { ok: false, op: "project_workspace", error: toActionError(e) };
  }
}

/** `POST /projects/{id}/milestones`（途中目標を足す。`seq` はストアが採番する）。 */
export async function createMilestone(
  client: TaskdClient,
  projectId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const body: MilestoneCreateBody = { title: formString(form, "title") ?? "" };
    const description = formString(form, "description");
    if (description) body.description = description;
    const status = formString(form, "status");
    if (status) body.status = status as MilestoneStatus;
    const milestone = await client.post<Milestone>(`/projects/${encodeURIComponent(projectId)}/milestones`, body, {
      signal,
    });
    return { ok: true, op: "milestone_create", milestone };
  } catch (e) {
    return { ok: false, op: "milestone_create", error: toActionError(e) };
  }
}

/** `PATCH /milestones/{id}`（SPEC §7 のアジャイル: 達成ごとに Go か再設計かを人が判定する）。 */
export async function patchMilestoneStatus(
  client: TaskdClient,
  milestoneId: string,
  status: MilestoneStatus,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const body: MilestonePatchBody = { status };
    const milestone = await client.patch<Milestone>(`/milestones/${encodeURIComponent(milestoneId)}`, body, {
      signal,
    });
    return { ok: true, op: "milestone_status", milestone };
  } catch (e) {
    return { ok: false, op: "milestone_status", error: toActionError(e) };
  }
}

/**
 * `POST /milestones/{id}/decide`（**管理系**、202 `MilestoneDecided`。ADR-0038 D2、
 * docs/taskd-api-v1.md §3.63、Phase 41 / G13j）。人の 3 つの答え（`ok`/`discuss`/`ng`）をそのまま送るだけ
 * （GUI 側では自由記述の必須チェックを画面の入力の時点で行うが、ここでは検証しない。空でも taskd に送って
 * taskd の 422 文言をそのまま出す。SPEC の「秘書は達成と言えるかを提案するにとどまる」の裏付けとして、
 * 3 値を GUI が解釈することもしない）。
 */
export async function decideMilestone(
  client: TaskdClient,
  milestoneId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const decision = (formString(form, "decision") ?? "ok") as MilestoneDecideBody["decision"];
    const body: MilestoneDecideBody = { decision };
    const note = formString(form, "note");
    if (note) body.note = note;
    const decided = await client.post<MilestoneDecided>(`/milestones/${encodeURIComponent(milestoneId)}/decide`, body, {
      signal,
    });
    return { ok: true, op: "milestone_decide", decided };
  } catch (e) {
    return { ok: false, op: "milestone_decide", error: toActionError(e) };
  }
}

/**
 * `POST /projects/{id}/plan`（**管理系**、202 `{task_id}`。docs/taskd-api-v1.md §3.61、Phase 29）。
 * 案件の「この方針で進める」。案件の依頼文・途中目標・人の一言・秘書との直近のやり取りを taskd が 1 つの
 * `goal` にまとめ、秘書に `kind = "plan"` の仕事を 1 件作る（分解の起点）。GUI は待たない（202）ので、
 * 仕事の木が増えていくのは SSE の再検証で追う。
 * `milestone_id` / `note` は空なら送らない（taskd 側でどちらも省略可）。
 */
export async function startProjectPlan(
  client: TaskdClient,
  projectId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const body: ProjectPlanBody = {};
    const milestoneId = formString(form, "milestone_id");
    if (milestoneId) body.milestone_id = milestoneId;
    const note = formString(form, "note");
    if (note) body.note = note;
    const accepted = await client.post<ProjectPlanAccepted>(`/projects/${encodeURIComponent(projectId)}/plan`, body, {
      signal,
    });
    return { ok: true, op: "project_plan", accepted };
  } catch (e) {
    return { ok: false, op: "project_plan", error: toActionError(e) };
  }
}
