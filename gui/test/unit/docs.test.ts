import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  defaultPromotePath,
  docsHref,
  docTree,
  insideFolder,
  isMarkdownName,
  prepareDocBody,
  resolveDocPath,
  shortDocSha,
  slugifyDoc,
  stripFrontMatter,
} from "~/lib/docs";
import { DOCS_TAB_LABEL, docsErrorHint, timelineKindLabel } from "~/lib/labels";
import { TaskdClient } from "~/taskd/client.server";
import { loadDocs, readDocsQuery } from "~/taskd/docs";
import {
  deleteDocPage,
  initDocs,
  promoteArtifact,
  putDocPage,
  readArtifactPromoteBody,
  readDocPagePutBody,
} from "~/taskd/docs-admin.server";
import { docPage, docPageResult, docsInitResult, docsTree } from "../mock-taskd/fixtures";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * 文書（ADR-0044 D7、taskd Phase 57 / G20。**正本は git**）。DOM を描画する unit テストが無い（G10-U1）ので、
 * 表示の判断は `~/lib/docs.ts` / `~/lib/labels.ts` の純粋関数、取得と送信は `~/taskd/docs*.ts` で見る。
 */

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("文書のツリー（純粋関数）", () => {
  it("フォルダの見出しを挟んで並べる", () => {
    const nodes = docTree(docsTree().items, "docs");
    expect(nodes.map((n) => `${n.kind}:${n.path}:${n.depth}`)).toEqual([
      "page:docs/README.md:0",
      "folder:docs/research:0",
      "page:docs/research/fs.md:1",
    ]);
    expect(nodes[0].label).toBe("案件のあらまし");
    expect(nodes[1].label).toBe("research");
  });

  it("畳んだフォルダの中かどうかが分かる", () => {
    expect(insideFolder({ kind: "page", path: "docs/research/fs.md", label: "", depth: 1 }, "docs/research")).toBe(
      true,
    );
    expect(insideFolder({ kind: "page", path: "docs/README.md", label: "", depth: 0 }, "docs/research")).toBe(false);
    // フォルダ自身は「中」ではない（見出しは残す）。
    expect(insideFolder({ kind: "folder", path: "docs/research", label: "", depth: 0 }, "docs/research")).toBe(false);
  });

  it("URL は path と q を保つ", () => {
    expect(docsHref("01P")).toBe("/projects/01P/docs");
    expect(docsHref("01P", { path: "docs/a.md" })).toBe("/projects/01P/docs?path=docs%2Fa.md");
    expect(docsHref("01P", { path: "docs/a.md", q: "調査" })).toContain("q=");
  });
});

describe("ページの中身（純粋関数）", () => {
  it("front matter を落とす（閉じていなければ落とさない）", () => {
    expect(stripFrontMatter("---\ntitle: x\n---\n\n# 本文\n")).toBe("\n# 本文\n");
    expect(stripFrontMatter("---\ntitle: x\n# 本文\n")).toBe("---\ntitle: x\n# 本文\n");
    expect(stripFrontMatter("# 本文\n")).toBe("# 本文\n");
  });

  it("`celeris:task/<id>` と `[[相対パス]]` をリンクに開く（根の外は開かない）", () => {
    const body = prepareDocBody(
      "---\ntitle: x\n---\n[担当](celeris:task/01TASK) と [[../README.md]] と [[../../etc.md]] と [[a.md|別名]]\n",
      "01P",
      "docs",
      "docs/research/fs.md",
    );
    expect(body).toContain("[担当](/tasks/01TASK)");
    expect(body).toContain("[../README.md](/projects/01P/docs?path=docs%2FREADME.md)");
    // 根の外に出るものは文字のまま（リンクにしない）。
    expect(body).toContain("[[../../etc.md]]");
    // `|` の後ろは表示名。
    expect(body).toContain("[別名](/projects/01P/docs?path=docs%2Fresearch%2Fa.md)");
    // front matter は本文に出さない。
    expect(body).not.toContain("title: x");
  });

  it("相対パスは文書の根の中だけ", () => {
    expect(resolveDocPath("docs", "docs/a/b.md", "../c.md")).toBe("docs/c.md");
    expect(resolveDocPath("docs", "docs/a.md", "b.md")).toBe("docs/b.md");
    expect(resolveDocPath("docs", "docs/a.md", "../out.md")).toBeNull();
    expect(resolveDocPath("docs", "docs/a.md", "../../../etc/passwd.md")).toBeNull();
  });

  it("sha は 7 桁で出す", () => {
    expect(shortDocSha("1234567890abcdef")).toBe("1234567");
  });
});

describe("昇格の既定のパス（純粋関数）", () => {
  it("`<根>/<種類>/<題名の slug>.md`", () => {
    expect(defaultPromotePath("docs", "research", "Pluvio の調査", "answer.md")).toBe("docs/research/pluvio.md");
    // 種類が無ければ research。
    expect(defaultPromotePath("docs", null, "Read Path", "answer.md")).toBe("docs/research/read-path.md");
    // 題名が ASCII にならなければ成果物の名前を使う。
    expect(defaultPromotePath("docs", "docs", "調査", "answer.md")).toBe("docs/docs/answer.md");
    // それも無理なら `page`。
    expect(defaultPromotePath("", "research", "調査", "答え.md")).toBe("research/page.md");
    expect(slugifyDoc("調査")).toBeNull();
  });

  it("`.md` の成果物だけ昇格できる（ボタンの出し分け）", () => {
    expect(isMarkdownName("answer.md")).toBe(true);
    expect(isMarkdownName("ANSWER.MD")).toBe(true);
    expect(isMarkdownName("result.json")).toBe(false);
  });
});

describe("言葉", () => {
  it("タイムラインの `doc` と文書のエラーの案内", () => {
    expect(timelineKindLabel("doc")).toBe("文書");
    expect(DOCS_TAB_LABEL).toBe("文書");
    expect(docsErrorHint("etag_mismatch")).toContain("再読み込み");
    expect(docsErrorHint("default_branch_busy")).toContain("編集中");
    expect(docsErrorHint("page_exists")).toContain("上書き");
    expect(docsErrorHint("unknown_code")).toBeNull();
  });
});

describe("taskd の中継", () => {
  it("ツリーと選んだページを読む（`?q=` はそのまま渡す）", async () => {
    mock.on("GET", "/api/v1/projects/01P/docs", (_req, res) => sendJson(res, 200, docsTree()));
    mock.on("GET", "/api/v1/projects/01P/docs/page", (_req, res) => sendJson(res, 200, docPage()));
    const data = await loadDocs(client, "01P", "Pluvio PoC", { path: "docs/research/fs.md", q: "調べ" });
    expect(data.tree?.items).toHaveLength(2);
    expect(data.page?.title).toBe("調べたこと");
    expect(data.treeError).toBeNull();
    expect(data.pageError).toBeNull();
    expect(mock.requests[0].url).toContain("q=");
  });

  it("文書リポジトリが無い案件は `docs_unavailable` を data にする", async () => {
    mock.on("GET", "/api/v1/projects/01P/docs", (_req, res) =>
      sendProblem(res, { status: 409, code: "docs_unavailable", detail: "この案件にはまだ文書リポジトリがありません" }),
    );
    const data = await loadDocs(client, "01P", "Pluvio PoC", {});
    expect(data.tree).toBeNull();
    expect(data.treeError?.code).toBe("docs_unavailable");
  });

  it("選んだページだけ読めなくてもツリーは出す", async () => {
    mock.on("GET", "/api/v1/projects/01P/docs", (_req, res) => sendJson(res, 200, docsTree()));
    mock.on("GET", "/api/v1/projects/01P/docs/page", (_req, res) =>
      sendProblem(res, { status: 404, code: "page_not_found", detail: "page not found: docs/none.md" }),
    );
    const data = await loadDocs(client, "01P", "Pluvio PoC", { path: "docs/none.md" });
    expect(data.tree).not.toBeNull();
    expect(data.pageError?.code).toBe("page_not_found");
  });

  it("`?path=&q=&edit=1` を読む", () => {
    const query = readDocsQuery(new Request("http://gui.test/projects/01P/docs?path=docs/a.md&q=x&edit=1"));
    expect(query).toEqual({ path: "docs/a.md", q: "x", edit: true });
  });

  it("用意する・保存・削除・昇格（管理系）", async () => {
    mock.on("POST", "/api/v1/projects/01P/docs/init", (_req, res) => sendJson(res, 200, docsInitResult()));
    mock.on("PUT", "/api/v1/projects/01P/docs/page", (_req, res) => sendJson(res, 200, docPageResult()));
    mock.on("DELETE", "/api/v1/projects/01P/docs/page", (_req, res) =>
      sendJson(res, 200, docPageResult({ deleted: true, etag: null })),
    );
    mock.on("POST", "/api/v1/tasks/01TASK/artifacts/promote", (_req, res) => sendJson(res, 200, docPageResult()));

    const init = await initDocs(client, "01P");
    expect(init.ok && init.op === "docs_init" && init.result.created).toBe(true);

    const saved = await putDocPage(client, "01P", { path: "docs/a.md", body: "# A\n", etag: "abc" });
    expect(saved.ok && saved.op === "docs_put").toBe(true);
    expect(JSON.parse(mock.requests[1].body)).toEqual({ path: "docs/a.md", body: "# A\n", etag: "abc" });

    const deleted = await deleteDocPage(client, "01P", "docs/a.md", "abc");
    expect(deleted.ok && deleted.op === "docs_delete" && deleted.result.deleted).toBe(true);
    expect(mock.requests[2].url).toContain("etag=abc");

    const promoted = await promoteArtifact(client, "01TASK", { name: "answer.md", path: "docs/research/x.md" });
    expect(promoted.ok && promoted.op === "docs_promote").toBe(true);
  });

  it("衝突（409）は例外にせず `{ok:false, error}` にする", async () => {
    mock.on("PUT", "/api/v1/projects/01P/docs/page", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "etag_mismatch",
        detail: "the page changed since it was read",
        extra: { etag: "9999" },
      }),
    );
    const outcome = await putDocPage(client, "01P", { path: "docs/a.md", body: "x" });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.error.status).toBe(409);
      expect(outcome.error.code).toBe("etag_mismatch");
      expect(docsErrorHint(outcome.error.code)).toContain("再読み込み");
    }
  });

  it("フォームの読み取りは空欄をキーごと送らない", () => {
    const form = new FormData();
    form.set("path", "docs/a.md");
    form.set("body", "# A\n");
    form.set("etag", "");
    expect(readDocPagePutBody(form)).toEqual({ path: "docs/a.md", body: "# A\n" });

    const promote = new FormData();
    promote.set("name", "answer.md");
    promote.set("path", "docs/research/x.md");
    promote.set("title", "");
    expect(readArtifactPromoteBody(promote)).toEqual({ name: "answer.md", path: "docs/research/x.md" });
    promote.set("overwrite", "1");
    expect(readArtifactPromoteBody(promote).overwrite).toBe(true);
  });
});
