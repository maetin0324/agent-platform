import { formatDuration, secondsBetween } from "~/lib/time-delta";
import type { ArtifactView, OrgNode, ProjectTaskView, Status, TaskId, WorkspaceSpec } from "~/taskd/types";

/**
 * 「成果物」（SPEC §2.1・§2.2・§3.7・§4 の 6）の純粋関数。DOM を描画する unit テストが無い件（G10-U1）を
 * 踏まえ、判断・計算はここに集約する（`~/lib/reports.ts` / `~/lib/work-tree.ts` と同じ作り）。
 */

/** 案件を横断した成果物一覧の 1 行（`/artifacts` の一覧、`/projects/:id` の「成果物」節で共有）。 */
export interface ProjectArtifactRow {
  taskId: TaskId;
  taskTitle: string;
  taskStatus: Status;
  assigneeName: string | null;
  workspace: WorkspacePlace;
  artifact: ArtifactView;
}

/** タスク 1 件ぶんの成果物一覧 + 置き場所（`~/taskd/artifacts.server.ts` が taskd への問い合わせで組む）。 */
export interface TaskArtifactBundle {
  workspace: WorkspacePlace;
  artifacts: ArtifactView[];
}

/** 担当ノード名の解決（`assignee` → `GET /org` の `name`）。`~/lib/work-tree.ts::projectTasksToGraph` と同じ規則。 */
export function resolveAssigneeName(assignee: string | null | undefined, orgById: Map<string, OrgNode>): string | null {
  if (!assignee) return null;
  return orgById.get(assignee)?.name ?? assignee;
}

/**
 * 案件のタスクと、タスクごとに束ねた成果物（`TaskArtifactBundle`）から、横断一覧の行を組む純粋関数。
 * 新しい順（`artifact.ts` 降順）に並べる（`/reports` と同じ「新しい順」の density）。
 */
export function buildProjectArtifactRows(
  tasks: readonly Pick<ProjectTaskView, "id" | "title" | "status" | "assignee">[],
  bundles: Map<TaskId, TaskArtifactBundle>,
  orgById: Map<string, OrgNode>,
): ProjectArtifactRow[] {
  const rows: ProjectArtifactRow[] = [];
  for (const task of tasks) {
    const bundle = bundles.get(task.id);
    if (!bundle) continue;
    const assigneeName = resolveAssigneeName(task.assignee, orgById);
    for (const artifact of bundle.artifacts) {
      rows.push({
        taskId: task.id,
        taskTitle: task.title,
        taskStatus: task.status,
        assigneeName,
        workspace: bundle.workspace,
        artifact,
      });
    }
  }
  return rows.sort((a, b) => (a.artifact.ts < b.artifact.ts ? 1 : a.artifact.ts > b.artifact.ts ? -1 : 0));
}

/** 「置き場所」の表示（SPEC §3.7「コードは ~/workspace/… のリポジトリ」）。 */
export interface WorkspacePlace {
  text: string;
  /** ローカルのときだけ（コピー用のパスはリンクにできないが、`vscode://file/<path>` は開ける）。 */
  vscodeHref: string | null;
}

/**
 * `Task.workspace`（`WorkspaceSpec::Local{path}` / `Remote{cluster, path}`）を「置き場所」の表示に変える。
 * - Local: `workspace_dir`（taskd が絶対化した値、docs/gui/api.md §3.5）をそのまま出す。無ければ生の `path`。
 * - Remote: コードが実際にあるのはクラスタ側（`task.workspace.path`）。`workspace_dir` は手元の写しでしかない
 *   ので使わない（docs/gui/api.md §3.5「Remote{cluster, path} では手元の写し…クラスタ側のパスは task.workspace.path」）。
 *   ローカルのパスではないのでリンクにはしない。
 */
export function workspacePlace(workspace: WorkspaceSpec, workspaceDir: string | null | undefined): WorkspacePlace {
  if (workspace.kind === "remote") {
    return { text: `${workspace.cluster}:${workspace.path}`, vscodeHref: null };
  }
  const text = workspaceDir ?? workspace.path;
  // `vscode://file/<絶対パス>`。パスが `/` 始まりなら二重スラッシュ（`vscode://file//tmp/...`）になるので
  // 先頭の `/` を落としてから繋ぐ（監査 9）。相対パスはそのまま（VS Code 側が解釈する）。
  return { text, vscodeHref: `vscode://file/${text.replace(/^\/+/, "")}` };
}

/** `sources.json`（ADR-0031「見るべき関連研究へのリンク」）は、名前で判定する（Content-Type は他の JSON と同じ `application/json`）。 */
export const SOURCES_JSON_NAME = "sources.json";

export function isSourcesArtifact(name: string): boolean {
  return name === SOURCES_JSON_NAME;
}

/** `sources.json` の 1 件（docs/adr/0031-web-research-evidence-gate.md「`[{url, title, engine?, cited: bool}]`」）。 */
export interface SourceLink {
  url: string;
  title: string;
  cited: boolean;
  engine?: string | null;
}

/**
 * `sources.json` の中身を「リンク集」に変換する。形が想定と違えば `null`（GUI は落とさず、通常の JSON 表示に戻す）。
 */
export function parseSourcesJson(text: string): SourceLink[] | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!Array.isArray(value)) return null;
  const links: SourceLink[] = [];
  for (const item of value) {
    if (typeof item !== "object" || item === null) return null;
    const rec = item as Record<string, unknown>;
    if (typeof rec.url !== "string" || typeof rec.title !== "string") return null;
    links.push({
      url: rec.url,
      title: rec.title,
      cited: rec.cited === true,
      engine: typeof rec.engine === "string" ? rec.engine : null,
    });
  }
  return links;
}

/** 「作られた時刻」の相対表示（`~/lib/reports.ts::relativeTimeLabel` と同じ作り）。 */
export function artifactRelativeTime(ts: string, fetchedAtIso: string): string {
  return `${formatDuration(secondsBetween(ts, fetchedAtIso))} 前`;
}
