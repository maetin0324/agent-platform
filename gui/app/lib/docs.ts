import type { DocItem } from "~/celeris/types";

/**
 * 「文書」タブ（ADR-0044 D7、docs/celeris-api-v1.md §3.92〜3.97。Phase 57 / G20）の**純粋な**表示ロジック。
 *
 * celeris が返すもの（`root` / `path` / `html` / `etag`）は一切作り直さない。ここでやるのは
 * 「フォルダごとに畳んで並べる」「相対リンクを GUI の URL に開く」「昇格の既定のパスを作る」だけで、
 * 判定（衝突・権限・存在）は全部 celeris 側にある。
 */

/** ツリーの 1 行（フォルダかページ）。`depth` は字下げの段数。 */
export interface DocTreeNode {
  kind: "folder" | "page";
  /** フォルダは根からのパス（`docs/research`）、ページは `DocItem.path`。 */
  path: string;
  /** 画面に出す名前（フォルダ名 / ページの題名）。 */
  label: string;
  depth: number;
  item?: DocItem;
}

/**
 * 平らなページの一覧を、フォルダの見出し付きの並びにする（文書の根の下だけを見る）。
 * 並びは celeris が返した順（= パスの昇順）のまま。フォルダは**最初にそのフォルダのページが出たところ**に挟む。
 */
export function docTree(items: DocItem[], root: string): DocTreeNode[] {
  const prefix = root && root !== "." ? `${root}/` : "";
  const out: DocTreeNode[] = [];
  const seen = new Set<string>();
  for (const item of items) {
    const relative = item.path.startsWith(prefix) ? item.path.slice(prefix.length) : item.path;
    const parts = relative.split("/");
    const folders = parts.slice(0, -1);
    let walked = prefix.replace(/\/$/, "");
    folders.forEach((folder, index) => {
      walked = walked ? `${walked}/${folder}` : folder;
      if (seen.has(walked)) return;
      seen.add(walked);
      out.push({ kind: "folder", path: walked, label: folder, depth: index });
    });
    out.push({
      kind: "page",
      path: item.path,
      label: item.title || parts[parts.length - 1],
      depth: folders.length,
      item,
    });
  }
  return out;
}

/** そのフォルダ（`folder`）の中にある行か（畳むときに使う）。 */
export function insideFolder(node: DocTreeNode, folder: string): boolean {
  return node.path !== folder && node.path.startsWith(`${folder}/`);
}

/** 「文書」タブの URL（ページを選ぶ・検索を保つ）。 */
export function docsHref(projectId: string, options: { path?: string | null; q?: string | null } = {}): string {
  const params = new URLSearchParams();
  if (options.path) params.set("path", options.path);
  if (options.q) params.set("q", options.q);
  const query = params.toString();
  return `/projects/${encodeURIComponent(projectId)}/docs${query ? `?${query}` : ""}`;
}

/** `docs/a/b.md` と `../c.md` → `docs/c.md`。根の外に出るものは `null`（リンクにしない）。 */
export function resolveDocPath(root: string, from: string, link: string): string | null {
  const parts = from.split("/").slice(0, -1);
  for (const part of link.trim().split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (parts.length === 0) return null;
      parts.pop();
      continue;
    }
    parts.push(part);
  }
  const joined = parts.join("/");
  const normalized = root && root !== "." ? root : "";
  if (!normalized) return joined;
  return joined === normalized || joined.startsWith(`${normalized}/`) ? joined : null;
}

/**
 * ページの Markdown を描く前の書き換え（ADR-0044 D7）:
 *
 * - front matter（先頭の `---` … `---`）を落とす（題名・タグ・タスクは celeris が構造で返している）
 * - `celeris:task/<ULID>` → `/tasks/<ULID>`
 * - `[[relative/path.md]]` / `[[relative/path.md|題名]]` → 「文書」タブへのリンク
 *
 * **生 HTML はそのまま残す**（描くのは `react-markdown` で、生 HTML は文字として出る。
 * DESIGN §8.3 / gui/CLAUDE.md の禁止事項: `dangerouslySetInnerHTML` は使わない）。
 */
export function prepareDocBody(raw: string, projectId: string, root: string, from: string): string {
  return expandWikiLinks(rewriteTaskLinks(stripFrontMatter(raw)), projectId, root, from);
}

/** 先頭の front matter を落とす（閉じていなければ何もしない）。 */
export function stripFrontMatter(raw: string): string {
  const body = raw.startsWith("﻿") ? raw.slice(1) : raw;
  if (!body.startsWith("---\n") && !body.startsWith("---\r\n")) return body;
  const rest = body.slice(body.indexOf("\n") + 1);
  const lines = rest.split("\n");
  const end = lines.findIndex((line) => line.trimEnd() === "---" || line.trimEnd() === "...");
  if (end < 0) return body;
  return lines.slice(end + 1).join("\n");
}

function rewriteTaskLinks(body: string): string {
  return body.replace(/\]\(\s*celeris:task\/([0-9A-Za-z]+)\s*\)/g, (_m, id: string) => `](/tasks/${id})`);
}

function expandWikiLinks(body: string, projectId: string, root: string, from: string): string {
  return body.replace(/\[\[([^\]\n|]+)(?:\|([^\]\n]+))?\]\]/g, (whole, target: string, label?: string) => {
    const resolved = resolveDocPath(root, from, target.trim());
    if (!resolved) return whole;
    const text = (label ?? target).trim().replace(/[[\]]/g, "");
    return `[${text}](${docsHref(projectId, { path: resolved })})`;
  });
}

/** 短い sha（履歴の 1 行）。 */
export function shortDocSha(sha: string): string {
  return sha.slice(0, 7);
}

/**
 * 成果物を昇格するときの既定のパス（ADR-0044 D7）: `<root>/<種類>/<題名の slug>.md`。
 * 種類はタスクの `category`（`research` などそのまま）、slug は ASCII だけ。作れなければ成果物の名前を使う。
 */
export function defaultPromotePath(
  root: string,
  category: string | null | undefined,
  title: string,
  name: string,
): string {
  const base = root && root !== "." ? root : "";
  const folder = (category ?? "research").trim() || "research";
  const slug = slugifyDoc(title) ?? slugifyDoc(name.replace(/\.md$/i, "")) ?? "page";
  const parts = [base, folder, `${slug}.md`].filter((part) => part.length > 0);
  return parts.join("/");
}

/** ASCII の slug（英数字以外は `-`。何も残らなければ `null`）。 */
export function slugifyDoc(raw: string): string | null {
  const slug = raw
    .replace(/[^0-9A-Za-z]+/g, "-")
    .toLowerCase()
    .replace(/^-+|-+$/g, "")
    .slice(0, 48)
    .replace(/^-+|-+$/g, "");
  return slug.length > 0 ? slug : null;
}

/** `.md` で終わるか（GUI は弾かない。ボタンの出し分けにだけ使う）。 */
export function isMarkdownName(name: string): boolean {
  return /\.md$/i.test(name.trim());
}
