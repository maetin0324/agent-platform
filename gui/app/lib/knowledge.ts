import type { Confidence, KnowledgeItem } from "~/celeris/types";

/**
 * 「知識」画面（ADR-0047 D5、docs/celeris-api-v1.md §3.98〜3.103。Phase 61 / G21）の**純粋な**表示ロジック。
 *
 * 案件の文書（`~/lib/docs.ts`）と同じ流儀: celeris が返すもの（`root` / `path` / `etag` / 並び順）は
 * 一切作り直さない。ここでやるのは「置き場ごとに束ねて並べる」「相対リンクを GUI の URL に開く」
 * 「出典を出し先に開く」「送る前に明らかに通らないパスを止める」だけで、判定（衝突・権限・存在）は
 * 全部 celeris 側にある。
 */

/** 「知識」画面の URL（ページ・検索・置き場の絞り込みを保つ）。 */
export function knowledgeHref(
  options: { path?: string | null; q?: string | null; scope?: string | null; edit?: boolean } = {},
): string {
  const params = new URLSearchParams();
  if (options.path) params.set("path", options.path);
  if (options.q) params.set("q", options.q);
  if (options.scope) params.set("scope", options.scope);
  if (options.edit) params.set("edit", "1");
  const query = params.toString();
  return `/knowledge${query ? `?${query}` : ""}`;
}

/** `_inbox`（候補）の画面の URL。 */
export function knowledgeInboxHref(): string {
  return "/knowledge/inbox";
}

/** KB 相対パスの置き場（`environment/clusters/pegasus.md` → `environment/clusters`。根直下は `""`）。 */
export function knowledgeDir(path: string): string {
  const cut = path.lastIndexOf("/");
  return cut < 0 ? "" : path.slice(0, cut);
}

/** ツリーの 1 束（置き場の見出し + その中のページ）。 */
export interface KnowledgeGroup {
  /** KB 相対のディレクトリ（根直下は `""`）。 */
  scope: string;
  /** 見出しに出す名前（根直下は `（根）`）。 */
  label: string;
  items: KnowledgeItem[];
}

/**
 * 平らなページの一覧を置き場ごとに束ねる。
 *
 * 束の順は **celeris が返した `scopes` の順**を先に使い、そこに無い置き場は `items` に出てきた順で後ろに足す
 * （`scopes` にあってページが 1 件も無い置き場は出さない）。束の中の並びは `items` のまま
 * （`?q=` のときの順位は celeris が決めている。ADR-0047 D3）。
 */
export function knowledgeGroups(items: KnowledgeItem[], scopes: readonly string[] = []): KnowledgeGroup[] {
  const byDir = new Map<string, KnowledgeItem[]>();
  const order: string[] = [];
  for (const item of items) {
    const dir = knowledgeDir(item.path);
    const bucket = byDir.get(dir);
    if (bucket) {
      bucket.push(item);
      continue;
    }
    byDir.set(dir, [item]);
    order.push(dir);
  }
  const seen = new Set<string>();
  const out: KnowledgeGroup[] = [];
  const push = (dir: string) => {
    if (seen.has(dir)) return;
    const group = byDir.get(dir);
    if (!group) return;
    seen.add(dir);
    out.push({ scope: dir, label: dir === "" ? "（根）" : dir, items: group });
  };
  for (const scope of scopes) push(scope);
  for (const dir of order) push(dir);
  return out;
}

/**
 * 送る前のパスの検査（celeris の 403 `path_forbidden` / 422 `validation` を先に人に見せるだけ）。
 * **判定の正本は celeris**（ここを通っても向こうが弾くことはある）。通るなら `null`、通らないなら理由。
 */
export function knowledgePathProblem(path: string): string | null {
  const trimmed = path.trim();
  if (trimmed === "") return "パスを入れてください。";
  if (trimmed.startsWith("/") || /^[A-Za-z]:[\\/]/.test(trimmed)) {
    return "絶対パスは使えません（KB の根からの相対パスにしてください）。";
  }
  if (trimmed.includes("\\")) return "`\\` は使えません（区切りは `/` です）。";
  const segments = trimmed.split("/");
  if (segments.some((segment) => segment === "..")) return "`..` は使えません（KB の根の外は触れません）。";
  if (segments.some((segment) => segment === "")) return "`//` のような空の区切りは使えません。";
  if (segments[0] === "_inbox") return "`_inbox/` は候補の置き場です（「候補」の画面で取り込むか捨ててください）。";
  if (!/\.md$/i.test(trimmed)) return "`.md` で終わるパスにしてください。";
  return null;
}

/** front matter を切り離す（先頭の `---` … `---`）。閉じていなければ全部が本文。 */
export function splitFrontMatter(raw: string): { frontMatter: string; body: string } {
  const text = raw.startsWith("﻿") ? raw.slice(1) : raw;
  if (!text.startsWith("---\n") && !text.startsWith("---\r\n")) return { frontMatter: "", body: text };
  const rest = text.slice(text.indexOf("\n") + 1);
  const lines = rest.split("\n");
  const end = lines.findIndex((line) => line.trimEnd() === "---" || line.trimEnd() === "...");
  if (end < 0) return { frontMatter: "", body: text };
  return { frontMatter: lines.slice(0, end).join("\n"), body: lines.slice(end + 1).join("\n") };
}

/** front matter から読み取る項目（celeris が `KnowledgePage` で返すものと同じ顔ぶれ）。 */
export interface KnowledgeFrontMatter {
  title: string | null;
  tags: string[];
  scope: string | null;
  sources: string[];
  confidence: Confidence | null;
  updated: string | null;
  /** 候補（`_inbox/`）だけが持つ取り込み先の鍵。 */
  path: string | null;
}

const CONFIDENCE_VALUES: readonly string[] = ["high", "medium", "low"];

/**
 * front matter の**ごく狭い YAML**（`key: value` と `key: [a, b]` と `key:` + `- a`）だけを読む。
 * これは**編集中の本文を人に見せ返すため**のもので、正本は celeris が返す `KnowledgePage` の側
 * （保存すれば celeris が読み直す）。読めないものは素直に `null` / `[]` にする。
 */
export function parseKnowledgeFrontMatter(raw: string): KnowledgeFrontMatter {
  const out: KnowledgeFrontMatter = {
    title: null,
    tags: [],
    scope: null,
    sources: [],
    confidence: null,
    updated: null,
    path: null,
  };
  const { frontMatter } = splitFrontMatter(raw);
  if (frontMatter === "") return out;
  const lines = frontMatter.split("\n");
  const lists: Record<string, string[]> = {};
  const scalars: Record<string, string> = {};
  let listKey: string | null = null;
  for (const line of lines) {
    const bullet = /^\s*-\s+(.*)$/.exec(line);
    if (bullet && listKey) {
      const value = unquoteYaml(bullet[1]);
      if (value !== "") lists[listKey].push(value);
      continue;
    }
    const entry = /^([A-Za-z_][A-Za-z0-9_-]*)\s*:\s*(.*)$/.exec(line);
    if (!entry) continue;
    const key = entry[1];
    const rest = entry[2].trim();
    if (rest === "") {
      listKey = key;
      lists[key] ??= [];
      continue;
    }
    listKey = null;
    if (rest.startsWith("[") && rest.endsWith("]")) {
      lists[key] = rest
        .slice(1, -1)
        .split(",")
        .map((part) => unquoteYaml(part))
        .filter((part) => part !== "");
      continue;
    }
    scalars[key] = unquoteYaml(rest);
  }
  out.title = scalars.title ?? null;
  out.scope = scalars.scope ?? null;
  out.updated = scalars.updated ?? null;
  out.path = scalars.path ?? null;
  out.tags = lists.tags ?? (scalars.tags ? [scalars.tags] : []);
  out.sources = lists.sources ?? (scalars.sources ? [scalars.sources] : []);
  const confidence = scalars.confidence?.toLowerCase();
  out.confidence = confidence && CONFIDENCE_VALUES.includes(confidence) ? (confidence as Confidence) : null;
  return out;
}

/** `"a"` / `'a'` / `a  # コメントではない` → `a`（引用と前後の空白だけ落とす）。 */
function unquoteYaml(raw: string): string {
  const value = raw.trim();
  if (
    value.length >= 2 &&
    ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'")))
  ) {
    return value.slice(1, -1).trim();
  }
  return value;
}

/**
 * 出典（`sources[]`）1 件の出し方（ADR-0047 D1）。
 * `task:<ULID>` はタスクへ、`url:<…>` は外（`http`/`https` だけ）へ、それ以外は文字のまま。
 */
export type KnowledgeSource =
  | { kind: "task"; label: string; href: string }
  | { kind: "url"; label: string; href: string }
  | { kind: "message"; label: string }
  | { kind: "human"; label: string }
  | { kind: "other"; label: string };

export function knowledgeSource(raw: string): KnowledgeSource {
  const source = raw.trim();
  if (source === "human") return { kind: "human", label: "人" };
  const task = /^task:(.+)$/.exec(source);
  if (task) return { kind: "task", label: `タスク ${task[1]}`, href: `/tasks/${encodeURIComponent(task[1])}` };
  const message = /^message:(.+)$/.exec(source);
  if (message) return { kind: "message", label: `対話 ${message[1]}` };
  const url = /^url:(.+)$/.exec(source);
  if (url) {
    const href = url[1].trim();
    // `javascript:` 等を踏ませない。http / https 以外は文字のまま出す。
    if (/^https?:\/\//i.test(href)) return { kind: "url", label: href, href };
    return { kind: "other", label: source };
  }
  return { kind: "other", label: source };
}

/** 確度（ADR-0047 D1 / D4）の日本語。知らない値はそのまま出す。 */
export function confidenceLabel(confidence: Confidence | null | undefined): string | null {
  switch (confidence) {
    case "high":
      return "確度 高";
    case "medium":
      return "確度 中";
    case "low":
      return "確度 低";
    default:
      return null;
  }
}

/** 確度の色（`Badge` の tone）。 */
export function confidenceTone(confidence: Confidence | null | undefined): "success" | "warning" | "neutral" {
  if (confidence === "high") return "success";
  if (confidence === "low") return "warning";
  return "neutral";
}

/**
 * 候補の `op`（ADR-0047 D4、Phase 62）の色。`retire`/`merge` は既存ページに手を入れる操作なので
 * 目立たせる（accept すると対象ページが動く・上書きされる）。
 */
export function knowledgeOpTone(op: string | null | undefined): "neutral" | "info" | "warning" | "danger" {
  switch (op) {
    case "create":
      return "info";
    case "update":
      return "neutral";
    case "merge":
      return "warning";
    case "retire":
      return "danger";
    default:
      return "neutral";
  }
}

/**
 * ADR-0052 D2（Phase 64）: 知識整理 run の `via`。`"langmem"` は従来どおり Qwen で抽出したもの、
 * `"fallback:<adapter>"` は Qwen に届かず tier cheap の汎用ハーネスで抽出したもの。
 */
export function isKnowledgeFallback(via: string | null | undefined): boolean {
  return typeof via === "string" && via.startsWith("fallback:");
}

/** `op` ごとの、accept したときに何が起きるかの短い説明（Phase 62。GUI のヒント用）。 */
export function knowledgeOpHint(op: string | null | undefined): string | null {
  switch (op) {
    case "merge":
      return "取り込むと、この本文で取り込み先のページを上書きします。";
    case "retire":
      return "取り込むと、取り込み先のページを `_retired/` へ動かします（本文は使いません）。";
    default:
      return null;
  }
}

/** `environment/clusters/a.md` と `../b.md` → `environment/b.md`。KB の根の外に出るものは `null`。 */
export function resolveKnowledgePath(from: string, link: string): string | null {
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
  return joined === "" ? null : joined;
}

/**
 * ページの Markdown を描く前の書き換え（`~/lib/docs.ts` の `prepareDocBody` と同じ考え方）:
 *
 * - front matter を落とす（題名・タグ・出典・確度は celeris が構造で返している）
 * - `celeris:task/<ULID>` → `/tasks/<ULID>`
 * - `[[相対パス.md]]` / `[[相対パス.md|題名]]` → `/knowledge?path=…`
 *
 * **生 HTML はそのまま残す**（描くのは `react-markdown` なので文字として出る。celeris が返す `html` は
 * 使わない = `dangerouslySetInnerHTML` を使わない。gui/CLAUDE.md の禁止事項）。
 */
export function prepareKnowledgeBody(raw: string, from: string): string {
  return expandKnowledgeWikiLinks(rewriteKnowledgeTaskLinks(splitFrontMatter(raw).body), from);
}

function rewriteKnowledgeTaskLinks(body: string): string {
  return body.replace(/\]\(\s*celeris:task\/([0-9A-Za-z]+)\s*\)/g, (_m, id: string) => `](/tasks/${id})`);
}

function expandKnowledgeWikiLinks(body: string, from: string): string {
  return body.replace(/\[\[([^\]\n|]+)(?:\|([^\]\n]+))?\]\]/g, (whole, target: string, label?: string) => {
    const resolved = resolveKnowledgePath(from, target.trim());
    if (!resolved) return whole;
    const text = (label ?? target).trim().replace(/[[\]]/g, "");
    return `[${text}](${knowledgeHref({ path: resolved })})`;
  });
}
