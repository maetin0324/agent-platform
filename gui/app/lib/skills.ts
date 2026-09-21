/**
 * 「skills」画面（ADR-0056 D3 続き、docs/celeris-api-v1.md §3.112〜3.117。Phase 82 / G35）の
 * **純粋な**表示ロジック。
 *
 * `~/lib/knowledge.ts` と同じ流儀: celeris が返すもの（`root` / `mounted_by` / 検証結果）は
 * 一切作り直さない。ここでやるのは「送る前に明らかに通らない frontmatter を止める」「雛形を出す」
 * 「継いだ mount と自分の mount を分ける（GUI は継承を再計算しないが、`own` と `inherited` に分けて
 * 見せるのは `node.profile.skills_mounts` と `effective.skills_mounts` の**差分を取るだけ**なので
 * 再計算ではない）」だけ。判定（frontmatter の検証・mount の可否）は celeris 側にある。
 */

/** skill 名の綴り（`task_core::knowledge::is_valid_skill_name` と同じ規則）。 */
export function isValidSkillName(name: string): boolean {
  return name.length > 0 && name.length <= 64 && /^[a-z0-9-]+$/.test(name);
}

/** 「skills」画面の URL（選んでいる skill・編集モードを保つ）。 */
export function skillsHref(options: { name?: string | null; edit?: boolean; create?: boolean } = {}): string {
  const params = new URLSearchParams();
  if (options.name) params.set("name", options.name);
  if (options.edit) params.set("edit", "1");
  if (options.create) params.set("create", "1");
  const query = params.toString();
  return `/knowledge/skills${query ? `?${query}` : ""}`;
}

/** 新しい skill の `SKILL.md` の雛形（Claude Code の skills 形式。frontmatter の `name`/`description` 必須）。 */
export function skillMarkdownTemplate(name: string): string {
  const skillName = name || "skill-name";
  return `---\nname: ${skillName}\ndescription: この skill が何をするかを 1〜2 文で\n---\n\n# ${skillName}\n\n手順や参照するべき情報をここに書きます。\n`;
}

/** `SKILL.md` の frontmatter から読み取れるもの（celeris が持つ `task_ops::knowledge::skill_frontmatter` と
 * 同じ「`---\nkey: value\n---`だけを読む最小パーサ」の TypeScript 版。プレビュー用で、正本は celeris が
 * `PUT /skills/{name}` で行う検証）。 */
export interface SkillFrontMatter {
  name: string | null;
  description: string | null;
  source: string | null;
}

export function parseSkillFrontMatter(raw: string): SkillFrontMatter {
  const out: SkillFrontMatter = { name: null, description: null, source: null };
  const text = raw.startsWith("﻿") ? raw.slice(1) : raw;
  if (!text.startsWith("---")) return out;
  const afterOpen = text.slice(3).replace(/^\n/, "");
  const closeAt = afterOpen.indexOf("\n---");
  if (closeAt < 0) return out;
  const body = afterOpen.slice(0, closeAt);
  for (const line of body.split("\n")) {
    const cut = line.indexOf(":");
    if (cut < 0) continue;
    const key = line.slice(0, cut).trim();
    const value = line.slice(cut + 1).trim();
    if (key === "name") out.name = value;
    else if (key === "description") out.description = value;
    else if (key === "source") out.source = value;
  }
  return out;
}

/** front matter を切り離す（先頭の `---` … `---`）。`~/lib/knowledge.ts::splitFrontMatter` と同じ考え方。 */
export function splitSkillFrontMatter(raw: string): { frontMatter: string; body: string } {
  const text = raw.startsWith("﻿") ? raw.slice(1) : raw;
  if (!text.startsWith("---")) return { frontMatter: "", body: text };
  const afterOpen = text.slice(3).replace(/^\n/, "");
  const closeAt = afterOpen.indexOf("\n---");
  if (closeAt < 0) return { frontMatter: "", body: text };
  const rest = afterOpen.slice(closeAt + 4);
  return { frontMatter: afterOpen.slice(0, closeAt), body: rest.startsWith("\n") ? rest.slice(1) : rest };
}

/**
 * SKILL.md を描く前の書き換え: front matter を落とし（構造では別に見せるため）、見出しを 1 段落とす
 * （`# <name>` から始まる本文が多く、カード自身の見出し（`<h2>`）と並ぶと「1 画面に h1 は 1 つ」
 * （ADR-0055 D1）が崩れるため。`~/lib/docs.ts::prepareDocBody` と同じ「表示のための書き換え」の考え方）。
 */
export function skillMarkdownBody(raw: string): string {
  const { body } = splitSkillFrontMatter(raw);
  return body
    .split("\n")
    .map((line) => (/^#{1,5} /.test(line) ? `#${line}` : line))
    .join("\n");
}

/**
 * 送る前の frontmatter の検査（celeris の 422 `validation` を先に人に見せるだけ。
 * **判定の正本は celeris**）。通るなら `null`、通らないなら理由。
 */
export function skillMarkdownProblem(name: string, skillMd: string): string | null {
  if (!isValidSkillName(name)) return "skill 名は英小文字・数字・ハイフンだけ（1〜64 文字）にしてください。";
  const front = parseSkillFrontMatter(skillMd);
  if (front.name === null) return "先頭に `---` で始まる frontmatter が要ります（`name` / `description`）。";
  if (front.name !== name) return `frontmatter の \`name: ${front.name ?? ""}\` が URL の skill 名と違います。`;
  if (!front.description || front.description.trim() === "") return "frontmatter の `description` を書いてください。";
  return null;
}

/**
 * ノードの mount を「自分で mount したもの」と「継いだもの（親から）」に分ける。
 * `own` は `node.profile.skills_mounts`、`effective` は `EffectiveProfile.skills_mounts`
 * （継いだ後、`GET /org` が返す）。`inherited` は `effective` にあって `own` に無いもの
 * （GUI は継承の計算をやり直さない。差分を取るだけ）。
 */
export interface MountedSkills {
  /** そのノード自身が mount しているもの（そこで unmount できる）。 */
  own: string[];
  /** 親から継いだもの（そのノードでは unmount できない。親で外す）。 */
  inherited: string[];
}

export function splitMountedSkills(own: readonly string[] = [], effective: readonly string[] = []): MountedSkills {
  const ownSet = new Set(own);
  return {
    own: [...own],
    inherited: effective.filter((skill) => !ownSet.has(skill)),
  };
}
