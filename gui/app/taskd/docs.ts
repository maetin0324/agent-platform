import type { ActionError } from "./action-types";
import type { TaskdClient } from "./client.server";
import { toActionError } from "./errors";
import type { DocPage, DocsTree } from "./types";

/**
 * 案件の文書（ADR-0044 D7、docs/taskd-api-v1.md §3.92〜3.97。Phase 57 / G20）の読み取りの中継。
 * **正本は git**（taskd が案件の主なリポジトリの `docs/` を読む）ので、GUI は持たず・作らず・直さない。
 *
 * 文書リポジトリがまだ無い案件は taskd が 409 `docs_unavailable` を返す。ここでは例外にせず
 * `unavailable` として画面に載せ、「文書を用意する」ボタン（`POST /projects/{id}/docs/init`）を出す。
 */
export interface DocsQuery {
  /** 開いているページ（リポジトリ相対のパス）。無ければツリーだけ。 */
  path?: string | null;
  /** 検索（taskd 側は `git grep -il`）。 */
  q?: string | null;
  /** 編集の画面を開く。 */
  edit?: boolean;
}

export interface DocsData {
  projectId: string;
  projectTitle: string;
  /** ツリー（`docs_unavailable` のときは `null`）。 */
  tree: DocsTree | null;
  /** 開いているページ（選んでいない・読めないときは `null`）。 */
  page: DocPage | null;
  /** ツリーが読めない理由（409 `docs_unavailable` など）。 */
  treeError: ActionError | null;
  /** 選んだページが読めない理由（404 / 403 など。ツリーは見えていてほしいので data にする）。 */
  pageError: ActionError | null;
  path: string | null;
  q: string | null;
  edit: boolean;
}

export async function loadDocs(
  client: TaskdClient,
  projectId: string,
  projectTitle: string,
  query: DocsQuery = {},
  signal?: AbortSignal,
): Promise<DocsData> {
  const path = query.path || null;
  const q = query.q || null;
  const base = {
    projectId,
    projectTitle,
    path,
    q,
    edit: query.edit === true,
  };
  let tree: DocsTree;
  try {
    tree = await client.get<DocsTree>(`/projects/${encodeURIComponent(projectId)}/docs`, {
      query: { q: q ?? undefined },
      signal,
    });
  } catch (e) {
    return { ...base, tree: null, page: null, treeError: toActionError(e), pageError: null };
  }
  if (!path) return { ...base, tree, page: null, treeError: null, pageError: null };
  try {
    const page = await client.get<DocPage>(`/projects/${encodeURIComponent(projectId)}/docs/page`, {
      query: { path },
      signal,
    });
    return { ...base, tree, page, treeError: null, pageError: null };
  } catch (e) {
    return { ...base, tree, page: null, treeError: null, pageError: toActionError(e) };
  }
}

/** `Request` の URL（`?path=&q=&edit=`）から問い合わせを組む。 */
export function readDocsQuery(request: Request): DocsQuery {
  const url = new URL(request.url);
  return {
    path: url.searchParams.get("path"),
    q: url.searchParams.get("q"),
    edit: url.searchParams.get("edit") === "1",
  };
}
