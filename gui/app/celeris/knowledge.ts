import type { ActionError } from "./action-types";
import type { CelerisClient } from "./client.server";
import { toActionError } from "./errors";
import type { KnowledgeInbox, KnowledgePage, KnowledgeTree } from "./types";

/**
 * 知識ベース（ADR-0047 D5、docs/celeris-api-v1.md §3.98〜3.103。Phase 61 / G21）の読み取りの中継。
 * **正本は `[knowledge] root` の Markdown**（celeris が作業ツリーのファイルを読む）ので、GUI は
 * 持たず・作らず・直さない。
 *
 * `[knowledge] root` が無い設定では celeris が 409 `knowledge_unavailable` を返し、設定はあっても
 * ディレクトリがまだ無ければ読み取りは `initialized: false` を返す（何も作らない）。どちらも例外にせず
 * 画面に載せ、**用意するのは `celerisctl knowledge init` だけ**（GUI に作るボタンは出さない）。
 */
export interface KnowledgeQuery {
  /** 開いているページ（KB の根からの相対パス）。無ければツリーだけ。 */
  path?: string | null;
  /** 検索（celeris 側は索引の tags / title と本文の全文一致。ADR-0047 D3）。 */
  q?: string | null;
  /** 置き場の絞り込み（KB 相対の接頭辞か front matter の `scope`）。 */
  scope?: string | null;
  /** 編集の画面を開く。 */
  edit?: boolean;
}

export interface KnowledgeData {
  /** ツリー（409 `knowledge_unavailable` のときは `null`）。 */
  tree: KnowledgeTree | null;
  /** 開いているページ（選んでいない・読めないときは `null`）。 */
  page: KnowledgePage | null;
  /** ツリーが読めない理由（409 `knowledge_unavailable` など）。 */
  treeError: ActionError | null;
  /** 選んだページが読めない理由（404 / 403 など。ツリーは見えていてほしいので data にする）。 */
  pageError: ActionError | null;
  path: string | null;
  q: string | null;
  scope: string | null;
  edit: boolean;
}

export async function loadKnowledge(
  client: CelerisClient,
  query: KnowledgeQuery = {},
  signal?: AbortSignal,
): Promise<KnowledgeData> {
  const path = query.path || null;
  const q = query.q || null;
  const scope = query.scope || null;
  const base = { path, q, scope, edit: query.edit === true };
  let tree: KnowledgeTree;
  try {
    tree = await client.get<KnowledgeTree>("/knowledge/tree", {
      query: { q: q ?? undefined, scope: scope ?? undefined },
      signal,
    });
  } catch (e) {
    return { ...base, tree: null, page: null, treeError: toActionError(e), pageError: null };
  }
  if (!path) return { ...base, tree, page: null, treeError: null, pageError: null };
  try {
    const page = await client.get<KnowledgePage>("/knowledge/page", { query: { path }, signal });
    return { ...base, tree, page, treeError: null, pageError: null };
  } catch (e) {
    return { ...base, tree, page: null, treeError: null, pageError: toActionError(e) };
  }
}

export interface KnowledgeInboxData {
  inbox: KnowledgeInbox | null;
  error: ActionError | null;
}

/** `GET /knowledge/inbox`（`_inbox/` の候補。新しい順は celeris が決めている）。 */
export async function loadKnowledgeInbox(client: CelerisClient, signal?: AbortSignal): Promise<KnowledgeInboxData> {
  try {
    return { inbox: await client.get<KnowledgeInbox>("/knowledge/inbox", { signal }), error: null };
  } catch (e) {
    return { inbox: null, error: toActionError(e) };
  }
}

/** `Request` の URL（`?path=&q=&scope=&edit=`）から問い合わせを組む。 */
export function readKnowledgeQuery(request: Request): KnowledgeQuery {
  const url = new URL(request.url);
  return {
    path: url.searchParams.get("path"),
    q: url.searchParams.get("q"),
    scope: url.searchParams.get("scope"),
    edit: url.searchParams.get("edit") === "1",
  };
}
