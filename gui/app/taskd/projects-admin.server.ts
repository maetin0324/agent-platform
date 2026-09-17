import type { CreateFailure, ProjectOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type {
  Milestone,
  MilestoneCreateBody,
  MilestonePatchBody,
  MilestoneStatus,
  Project,
  ProjectCreateBody,
  ProjectPatchBody,
  ProjectStatus,
} from "./types";

/**
 * 「案件」画面（`/projects`, `/projects/:id`）からの中継（ADR-0033 D2、docs/taskd-api-v1.md §3.46〜3.49）。
 * 組織の編集とは違い**管理系ではない**（通常の要求。トークンを設定した taskd では他の全要求と同じくトークンが要る）。
 * GUI 側では検証しない: taskd が 404 / 422 / 400 を返したらその文言をそのまま画面に出す。
 */

/** `POST /projects`。`title` / `request` は空でもそのまま送り、taskd の 422 文言を出す（ADR-0005 D5）。 */
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

/** フォーム（`title` / `request`）から `ProjectCreateBody` を読む。 */
export function readProjectCreateInput(form: FormData): ProjectCreateBody {
  return {
    title: formString(form, "title") ?? "",
    request: formString(form, "request") ?? "",
  };
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
