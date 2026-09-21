import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { loadSkills, readSkillsQuery } from "~/celeris/skills";
import {
  deleteSkill,
  mountSkill,
  putSkill,
  readSkillName,
  readSkillPutBody,
  unmountSkill,
} from "~/celeris/skills-admin.server";
import {
  isValidSkillName,
  type MountedSkills,
  parseSkillFrontMatter,
  skillBodyProblem,
  skillFilePathProblem,
  skillMarkdownBody,
  skillMarkdownProblem,
  skillMarkdownTemplate,
  skillNameProblem,
  skillsHref,
  splitMountedSkills,
  splitSkillFrontMatter,
} from "~/lib/skills";
import { orgList, skillDetail, skillList, skillListEmpty, skillPutResult } from "../mock-celeris/fixtures";
import {
  type MockCeleris,
  sendProblem,
  serveOrgSkillMount,
  serveSkills,
  startMockCeleris,
} from "../mock-celeris/server";

/**
 * skills（ADR-0056 D3 続き、celeris Phase 82 / G35。**正本は KB の `skills/<name>/SKILL.md`**）。
 * DOM を描画する unit テストが無い（G10-U1）ので、表示の判断は `~/lib/skills.ts` の純粋関数、
 * 取得と送信は `~/celeris/skills*.ts` で見る。
 */

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("skill 名の綴り（純粋関数）", () => {
  it("[a-z0-9-]{1,64} だけを通す", () => {
    expect(isValidSkillName("rust-review")).toBe(true);
    expect(isValidSkillName("a")).toBe(true);
    expect(isValidSkillName("")).toBe(false);
    expect(isValidSkillName("Rust-Review")).toBe(false);
    expect(isValidSkillName("rust_review")).toBe(false);
    expect(isValidSkillName("a".repeat(65))).toBe(false);
  });
});

describe("URL の組み立て（純粋関数）", () => {
  it("選んでいる skill・編集・作成モードを保つ", () => {
    expect(skillsHref()).toBe("/knowledge/skills");
    expect(skillsHref({ name: "rust-review" })).toBe("/knowledge/skills?name=rust-review");
    expect(skillsHref({ name: "rust-review", edit: true })).toBe("/knowledge/skills?name=rust-review&edit=1");
    expect(skillsHref({ create: true })).toBe("/knowledge/skills?create=1");
  });
});

describe("SKILL.md の frontmatter（純粋関数）", () => {
  it("雛形は name / description を持つ", () => {
    const template = skillMarkdownTemplate("rust-review");
    expect(template).toContain("name: rust-review");
    expect(template).toContain("description:");
  });

  it("frontmatter の name / description / source を読む", () => {
    const front = parseSkillFrontMatter(
      "---\nname: rust-review\ndescription: 手順\nsource: mcp:chatgpt\n---\n\n本文\n",
    );
    expect(front).toEqual({ name: "rust-review", description: "手順", source: "mcp:chatgpt" });
  });

  it("frontmatter が無ければ全部 null", () => {
    expect(parseSkillFrontMatter("本文だけ\n")).toEqual({ name: null, description: null, source: null });
  });

  it("送る前の検査: 名前の形・frontmatter の有無・name の一致・description の有無", () => {
    expect(skillMarkdownProblem("Bad Name", "---\nname: x\ndescription: y\n---\n")).toMatch(/英小文字/);
    expect(skillMarkdownProblem("rust-review", "本文だけ")).toMatch(/frontmatter/);
    expect(skillMarkdownProblem("rust-review", "---\nname: other\ndescription: y\n---\n")).toMatch(/違います/);
    expect(skillMarkdownProblem("rust-review", "---\nname: rust-review\ndescription: \n---\n")).toMatch(/description/);
    expect(skillMarkdownProblem("rust-review", "---\nname: rust-review\ndescription: 手順\n---\n")).toBeNull();
  });
});

// Phase 84: フィールドごとのインラインエラー表示のため `skillMarkdownProblem` を 2 つに割った
// （`skillNameProblem`/`skillBodyProblem`）。`skillMarkdownProblem` はこの 2 つを順に見るだけの合成。
describe("skillNameProblem / skillBodyProblem（フィールドごとの検査。Phase 84）", () => {
  it("skillNameProblem は名前の形だけを見る", () => {
    expect(skillNameProblem("rust-review")).toBeNull();
    expect(skillNameProblem("Bad Name")).toMatch(/英小文字/);
    expect(skillNameProblem("")).toMatch(/英小文字/);
  });

  it("skillBodyProblem は frontmatter の有無・name の一致・description の有無だけを見る（名前の形は見ない）", () => {
    expect(skillBodyProblem("rust-review", "本文だけ")).toMatch(/frontmatter/);
    expect(skillBodyProblem("rust-review", "---\nname: other\ndescription: y\n---\n")).toMatch(/違います/);
    expect(skillBodyProblem("rust-review", "---\nname: rust-review\ndescription: \n---\n")).toMatch(/description/);
    expect(skillBodyProblem("rust-review", "---\nname: rust-review\ndescription: 手順\n---\n")).toBeNull();
  });

  it("skillMarkdownProblem は skillNameProblem を先に見る（名前が不正なら本文は見ない）", () => {
    expect(skillMarkdownProblem("Bad Name", "本文だけ")).toBe(skillNameProblem("Bad Name"));
  });
});

describe("skillFilePathProblem（付属ファイルのパス検査。Phase 84）", () => {
  it("相対パスなら通す", () => {
    expect(skillFilePathProblem("checklist.md")).toBeNull();
    expect(skillFilePathProblem("refs/checklist.md")).toBeNull();
  });

  it("空文字はまだ入力していないだけ（エラーにしない。送信時に行ごと落とされる）", () => {
    expect(skillFilePathProblem("")).toBeNull();
    expect(skillFilePathProblem("   ")).toBeNull();
  });

  it("絶対パス・`\\`・`..`・SKILL.md 自身は拒否する", () => {
    expect(skillFilePathProblem("/etc/passwd")).toMatch(/絶対パス/);
    expect(skillFilePathProblem("a\\b")).toMatch(/\\/);
    expect(skillFilePathProblem("../secret")).toMatch(/\.\./);
    expect(skillFilePathProblem("a//b")).toMatch(/\.\./);
    expect(skillFilePathProblem("SKILL.md")).toMatch(/SKILL\.md/);
  });
});

describe("SKILL.md を描く前の書き換え（純粋関数）", () => {
  it("front matter を切り離す", () => {
    expect(splitSkillFrontMatter("---\nname: x\n---\n\n本文\n")).toEqual({
      frontMatter: "name: x",
      body: "\n本文\n",
    });
    expect(splitSkillFrontMatter("本文だけ\n")).toEqual({ frontMatter: "", body: "本文だけ\n" });
  });

  it("front matter を落とし、見出しを 1 段落として『1 画面に h1 は 1 つ』を保つ（ADR-0055 D1）", () => {
    const body = skillMarkdownBody(
      "---\nname: rust-review\ndescription: 手順\n---\n\n# rust-review\n\n## 手順\n\n本文\n",
    );
    expect(body).not.toContain("---");
    expect(body).toContain("## rust-review");
    expect(body).toContain("### 手順");
    expect(body).not.toMatch(/^# /m);
  });
});

describe("own / inherited の分割（純粋関数）", () => {
  it("effective にあって own に無いものが inherited", () => {
    const split: MountedSkills = splitMountedSkills(["a"], ["a", "b"]);
    expect(split.own).toEqual(["a"]);
    expect(split.inherited).toEqual(["b"]);
  });

  it("own だけなら inherited は空、両方省略しても壊れない", () => {
    expect(splitMountedSkills(["a"])).toEqual({ own: ["a"], inherited: [] });
    expect(splitMountedSkills()).toEqual({ own: [], inherited: [] });
  });
});

describe("読み取り（GET /skills、GET /skills/{name}）", () => {
  it("一覧と、選んだ skill の詳細を返す", async () => {
    serveSkills(mock, { list: skillList() });
    const data = await loadSkills(client, { name: "rust-review" });
    expect(data.list?.items.map((i) => i.name)).toEqual(["rust-review"]);
    expect(data.detail?.name).toBe("rust-review");
    expect(data.listError).toBeNull();
    expect(data.detailError).toBeNull();
  });

  it("skills/ が空でも initialized: true のまま", async () => {
    serveSkills(mock, { list: skillListEmpty() });
    const data = await loadSkills(client);
    expect(data.list?.initialized).toBe(true);
    expect(data.list?.items).toEqual([]);
  });

  it("`[knowledge] root` 未設定（409）は `list: null` で、画面に案内を出せる形にする", async () => {
    mock.on("GET", "/api/v1/skills", (_req, res) => {
      sendProblem(res, { status: 409, code: "knowledge_unavailable", detail: "not configured" });
    });
    const data = await loadSkills(client);
    expect(data.list).toBeNull();
    expect(data.listError?.code).toBe("knowledge_unavailable");
  });

  it("知らない skill は 404 `skill_not_found`（一覧は見えたまま）", async () => {
    serveSkills(mock, { list: skillList() });
    mock.on("GET", "/api/v1/skills/does-not-exist", (_req, res) => {
      sendProblem(res, { status: 404, code: "skill_not_found", detail: "not found" });
    });
    const data = await loadSkills(client, { name: "does-not-exist" });
    expect(data.list).not.toBeNull();
    expect(data.detail).toBeNull();
    expect(data.detailError?.code).toBe("skill_not_found");
  });

  it("`?name=&edit=&create=` を読む", () => {
    const q = readSkillsQuery(new Request("http://x/knowledge/skills?name=rust-review&edit=1"));
    expect(q).toEqual({ name: "rust-review", edit: true, create: false });
  });
});

describe("書き込み（PUT /skills/{name}、DELETE /skills/{name}）", () => {
  it("PUT は作成・更新の結果をそのまま返す", async () => {
    serveSkills(mock, { list: skillList(), put: skillPutResult() });
    const outcome = await putSkill(client, "rust-review", { skill_md: "---\nname: rust-review\n---\n" });
    expect(outcome).toEqual({
      ok: true,
      op: "skill_put",
      name: "rust-review",
      result: skillPutResult(),
    });
  });

  it("PUT の 422 validation は例外にせず ok:false で返す", async () => {
    mock.on("PUT", "/api/v1/skills/rust-review", (_req, res) => {
      sendProblem(res, { status: 422, code: "validation", detail: "description must not be blank" });
    });
    const outcome = await putSkill(client, "rust-review", { skill_md: "---\nname: rust-review\n---\n" });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.code).toBe("validation");
  });

  it("DELETE は mount されていなければ 204 で消える", async () => {
    serveSkills(mock, { list: skillList({ items: [{ name: "rust-review", description: "手順", mounted_by: [] }] }) });
    const outcome = await deleteSkill(client, "rust-review");
    expect(outcome).toEqual({ ok: true, op: "skill_delete", name: "rust-review" });
  });

  it("DELETE は mount されていれば 409 `skill_mounted`", async () => {
    serveSkills(mock, { list: skillList(), mountedNames: ["rust-review"] });
    const outcome = await deleteSkill(client, "rust-review");
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.code).toBe("skill_mounted");
  });

  it("フォームから PUT の本文を組む（files は path/content の並行配列、空行は落とす）", () => {
    const form = new FormData();
    form.set("skill_md", "---\nname: rust-review\n---\n");
    form.append("file_path", "checklist.md");
    form.append("file_content", "- fmt\n");
    form.append("file_path", "");
    form.append("file_content", "捨てる");
    expect(readSkillPutBody(form)).toEqual({
      skill_md: "---\nname: rust-review\n---\n",
      files: [{ path: "checklist.md", content: "- fmt\n" }],
    });

    const withoutFiles = new FormData();
    withoutFiles.set("skill_md", "本文");
    expect(readSkillPutBody(withoutFiles)).toEqual({ skill_md: "本文" });
  });

  it("フォームから skill 名をトリムして読む", () => {
    const form = new FormData();
    form.set("name", "  rust-review  ");
    expect(readSkillName(form)).toBe("rust-review");
  });
});

describe("mount / unmount（POST /org/{id}/skills、DELETE /org/{id}/skills/{skill}）", () => {
  it("mount は更新後の OrgNode を返す", async () => {
    const node = orgList().items.find((n) => n.id === "coding");
    if (!node) throw new Error("fixture must have coding");
    serveOrgSkillMount(mock, "coding", "rust-review", node);
    const outcome = await mountSkill(client, "coding", "rust-review");
    expect(outcome).toEqual({ ok: true, op: "skill_mount", id: "coding", skill: "rust-review", node });
  });

  it("unmount は更新後の OrgNode を返す", async () => {
    const node = { ...orgList().items[0], profile: undefined };
    serveOrgSkillMount(mock, "coding", "rust-review", node);
    const outcome = await unmountSkill(client, "coding", "rust-review");
    expect(outcome).toEqual({ ok: true, op: "skill_unmount", id: "coding", skill: "rust-review", node });
  });

  it("知らないノードは 404 `org_node_not_found`（例外にせず ok:false）", async () => {
    mock.on("POST", "/api/v1/org/does-not-exist/skills", (_req, res) => {
      sendProblem(res, { status: 404, code: "org_node_not_found", detail: "not found" });
    });
    const outcome = await mountSkill(client, "does-not-exist", "rust-review");
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.code).toBe("org_node_not_found");
  });
});

describe("skill 詳細の fixture が壊れていない", () => {
  it("skillDetail() は skills_put が書く frontmatter の形と揃う", () => {
    const detail = skillDetail();
    expect(detail.skill_md).toContain("name: rust-review");
    expect(detail.skill_md).toContain("description:");
  });
});
