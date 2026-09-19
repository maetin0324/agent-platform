import type { ActionError } from "./action-types";
import type { TaskdClient } from "./client.server";
import { toActionError } from "./errors";
import type { TreeFileView, TreeView } from "./types";

/**
 * タスクの作業ツリーの閲覧（ADR-0043 D6、docs/taskd-api-v1.md §3.72〜3.73。Phase 52 / G16）の中継。
 * **読み取りで、トークンは要らない**が、他の読み取りと同じく `TaskdClient` を通す
 * （ブラウザから taskd を直接呼ばない。DESIGN §8.1）。
 *
 * 境界（`..`・絶対パス・シンボリックリンクでの脱出）は taskd が 403 `path_forbidden` で弾き、
 * 作業ツリーを持たないタスクは 404 `file_not_found`。GUI では判定も回避もせず、文言をそのまま出す。
 * 一覧（`GET /tasks/{id}/tree`）が落ちたらページ自体が出せないので例外のまま投げ、
 * 選んだファイル（`GET /tasks/{id}/tree/file`）の失敗だけは `fileError` として画面に載せる
 * （403 / 404 でも一覧は見えていてほしいため）。
 */
export interface TaskFilesQuery {
  /** 省略すると taskd が先頭のリポジトリ（＝ワーカーのカレントディレクトリ）を選ぶ。 */
  repo?: string | null;
  /** 見ているディレクトリ（作業ツリーからの相対パス。省略・空は根）。 */
  path?: string | null;
  /** 選んだファイル（同じく相対パス）。無ければ本文は取りに行かない。 */
  file?: string | null;
}

export interface TaskFilesData {
  taskId: string;
  tree: TreeView;
  /** 選んだファイルの本文（`binary` / `too_large` のときは `text` が無い）。 */
  file: TreeFileView | null;
  /** 選んだファイルの取得に失敗したときの taskd の文言（403 / 404 など）。 */
  fileError: ActionError | null;
  /** いま選んでいるファイルのパス（失敗していても画面に出すため別に持つ）。 */
  filePath: string | null;
}

export async function loadTaskFiles(
  client: TaskdClient,
  taskId: string,
  query: TaskFilesQuery = {},
  signal?: AbortSignal,
): Promise<TaskFilesData> {
  const repo = query.repo || null;
  const path = query.path || "";
  const filePath = query.file || null;
  const tree = await client.get<TreeView>(`/tasks/${encodeURIComponent(taskId)}/tree`, {
    query: { repo: repo ?? undefined, path: path || undefined },
    signal,
  });
  if (!filePath) return { taskId, tree, file: null, fileError: null, filePath: null };
  try {
    const file = await client.get<TreeFileView>(`/tasks/${encodeURIComponent(taskId)}/tree/file`, {
      // `repo` は一覧が返したものに揃える（省略時に taskd が選んだ先頭のリポジトリと同じものを読む）。
      query: { repo: tree.repo, path: filePath },
      signal,
    });
    return { taskId, tree, file, fileError: null, filePath };
  } catch (e) {
    return { taskId, tree, file: null, fileError: toActionError(e), filePath };
  }
}

/** `Request` の URL（`?repo=&path=&file=`）から問い合わせを組む。 */
export function readTaskFilesQuery(request: Request): TaskFilesQuery {
  const url = new URL(request.url);
  return {
    repo: url.searchParams.get("repo"),
    path: url.searchParams.get("path"),
    file: url.searchParams.get("file"),
  };
}
