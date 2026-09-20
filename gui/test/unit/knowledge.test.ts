import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { loadKnowledge, loadKnowledgeInbox, readKnowledgeQuery } from "~/celeris/knowledge";
import {
  acceptKnowledgeCandidate,
  putKnowledgePage,
  readKnowledgeAcceptBody,
  readKnowledgePagePutBody,
  rejectKnowledgeCandidate,
} from "~/celeris/knowledge-admin.server";
import {
  confidenceLabel,
  confidenceTone,
  knowledgeDir,
  knowledgeGroups,
  knowledgeHref,
  knowledgeInboxHref,
  knowledgeOpHint,
  knowledgeOpTone,
  knowledgePathProblem,
  knowledgeSource,
  parseKnowledgeFrontMatter,
  prepareKnowledgeBody,
  resolveKnowledgePath,
  splitFrontMatter,
} from "~/lib/knowledge";
import { KNOWLEDGE_NAV_LABEL, knowledgeErrorHint } from "~/lib/labels";
import {
  knowledgeCandidate,
  knowledgeInbox,
  knowledgePage,
  knowledgePageResult,
  knowledgeRejectResult,
  knowledgeTree,
  knowledgeTreeUninitialized,
} from "../mock-celeris/fixtures";
import { type MockCeleris, sendJson, sendProblem, serveKnowledge, startMockCeleris } from "../mock-celeris/server";

/**
 * 知識ベース（ADR-0047 D5、celeris Phase 61 / G21。**正本は `[knowledge] root` の Markdown**）。
 * DOM を描画する unit テストが無い（G10-U1）ので、表示の判断は `~/lib/knowledge.ts` /
 * `~/lib/labels.ts` の純粋関数、取得と送信は `~/celeris/knowledge*.ts` で見る。
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

describe("置き場ごとの束ね方（純粋関数）", () => {
  it("ページの置き場（ディレクトリ）を取り出す", () => {
    expect(knowledgeDir("environment/clusters/pegasus.md")).toBe("environment/clusters");
    expect(knowledgeDir("user/profile.md")).toBe("user");
    // 根直下は空文字（見出しは「（根）」）。
    expect(knowledgeDir("README.md")).toBe("");
  });

  it("`scopes` の順で束ね、ページの無い置き場は出さない", () => {
    const tree = knowledgeTree();
    const groups = knowledgeGroups(tree.items, tree.scopes ?? []);
    expect(groups.map((g) => g.scope)).toEqual(["environment/clusters", "user"]);
    expect(groups.map((g) => g.items.length)).toEqual([1, 1]);
    expect(groups[0].items[0].path).toBe("environment/clusters/pegasus.md");
    // `scopes` にあってもページが 1 件も無ければ束にしない。
    expect(knowledgeGroups(tree.items, ["projects/pluvio", "user"]).map((g) => g.scope)).toEqual([
      "user",
      "environment/clusters",
    ]);
  });

  it("`scopes` に無い置き場は `items` に出てきた順で後ろに足す（根直下は「（根）」）", () => {
    const groups = knowledgeGroups(
      [
        { path: "README.md", title: "はじめに" },
        { path: "user/profile.md", title: "人のこと" },
        { path: "user/prefs.md", title: "好み" },
      ],
      [],
    );
    expect(groups.map((g) => `${g.scope}:${g.label}:${g.items.length}`)).toEqual([":（根）:1", "user:user:2"]);
  });

  it("URL は path / q / scope / edit を保つ", () => {
    expect(knowledgeHref()).toBe("/knowledge");
    expect(knowledgeHref({ path: "user/profile.md" })).toBe("/knowledge?path=user%2Fprofile.md");
    const href = knowledgeHref({ path: "user/profile.md", q: "pegasus", scope: "user", edit: true });
    expect(href).toContain("q=pegasus");
    expect(href).toContain("scope=user");
    expect(href).toContain("edit=1");
    expect(knowledgeInboxHref()).toBe("/knowledge/inbox");
  });
});

describe("パスの検査（純粋関数。celeris の 403 / 422 を先に見せるだけ）", () => {
  it("通るパス", () => {
    expect(knowledgePathProblem("environment/clusters/pegasus.md")).toBeNull();
    expect(knowledgePathProblem("README.md")).toBeNull();
    // 大文字の拡張子も通す（celeris の規則は `.md` で終わること）。
    expect(knowledgePathProblem("user/Profile.MD")).toBeNull();
  });

  it("`..` と絶対パスは止める（403 `path_forbidden` と同じ理由）", () => {
    expect(knowledgePathProblem("../etc/passwd.md")).toContain("`..`");
    expect(knowledgePathProblem("user/../../secret.md")).toContain("`..`");
    expect(knowledgePathProblem("/etc/passwd.md")).toContain("絶対パス");
    expect(knowledgePathProblem("C:/tmp/x.md")).toContain("絶対パス");
    expect(knowledgePathProblem("user\\profile.md")).toContain("`/`");
  });

  it("`.md` で終わらないパスは止める（422 `validation` と同じ理由）", () => {
    expect(knowledgePathProblem("user/profile")).toContain("`.md`");
    expect(knowledgePathProblem("user/profile.txt")).toContain("`.md`");
  });

  it("`_inbox/` は候補の置き場なので止める（PUT は 403 `path_forbidden`）", () => {
    expect(knowledgePathProblem("_inbox/20260920T010000-pegasus.md")).toContain("_inbox");
  });

  it("空・空の区切りも止める", () => {
    expect(knowledgePathProblem("")).toContain("パス");
    expect(knowledgePathProblem("   ")).toContain("パス");
    expect(knowledgePathProblem("user//profile.md")).toContain("空の区切り");
  });
});

describe("front matter の切り分けと読み取り（純粋関数。編集の画面で使う）", () => {
  it("front matter と本文を切り分ける（閉じていなければ全部が本文）", () => {
    const split = splitFrontMatter("---\ntitle: x\ntags: [a]\n---\n\n# 本文\n");
    expect(split.frontMatter).toBe("title: x\ntags: [a]");
    expect(split.body).toBe("\n# 本文\n");
    expect(splitFrontMatter("---\ntitle: x\n# 本文\n").frontMatter).toBe("");
    expect(splitFrontMatter("# 本文\n").body).toBe("# 本文\n");
  });

  it("celeris が返すのと同じ顔ぶれ（title / tags / scope / sources / confidence）を読む", () => {
    const page = knowledgePage();
    const front = parseKnowledgeFrontMatter(page.raw);
    expect(front.title).toBe(page.title);
    expect(front.tags).toEqual(page.tags);
    expect(front.scope).toBe(page.scope);
    expect(front.sources).toEqual(page.sources);
    expect(front.confidence).toBe(page.confidence);
    expect(front.updated).toBe(page.updated);
  });

  it("`- ` の並びでも読む。知らない confidence と front matter 無しは空", () => {
    const front = parseKnowledgeFrontMatter(
      "---\ntitle: 人のこと\ntags:\n  - user\n  - profile\nsources:\n  - human\nconfidence: HIGH\npath: user/profile.md\n---\n本文\n",
    );
    expect(front.tags).toEqual(["user", "profile"]);
    expect(front.sources).toEqual(["human"]);
    expect(front.confidence).toBe("high");
    // 候補（`_inbox/`）だけが持つ取り込み先の鍵。
    expect(front.path).toBe("user/profile.md");

    expect(parseKnowledgeFrontMatter("---\nconfidence: とても高い\n---\n").confidence).toBeNull();
    const empty = parseKnowledgeFrontMatter("# 本文だけ\n");
    expect(empty).toEqual({
      title: null,
      tags: [],
      scope: null,
      sources: [],
      confidence: null,
      updated: null,
      path: null,
    });
  });
});

describe("本文の書き換えと出典（純粋関数）", () => {
  it("front matter を落とし、`celeris:task/<id>` と `[[相対パス]]` をリンクに開く", () => {
    const body = prepareKnowledgeBody(knowledgePage().raw, "environment/clusters/pegasus.md");
    expect(body).not.toContain("title: pegasus の使い方");
    expect(body).toContain("[担当](/tasks/01TASKPEGASUS0000000000001)");
    expect(body).toContain("[../../user/profile.md](/knowledge?path=user%2Fprofile.md)");
  });

  it("KB の根の外に出る `[[…]]` はリンクにしない", () => {
    const body = prepareKnowledgeBody("[[../../../etc/passwd.md]] と [[a.md|別名]]\n", "user/profile.md");
    expect(body).toContain("[[../../../etc/passwd.md]]");
    expect(body).toContain("[別名](/knowledge?path=user%2Fa.md)");
    expect(resolveKnowledgePath("user/profile.md", "../environment/x.md")).toBe("environment/x.md");
    expect(resolveKnowledgePath("user/profile.md", "../../out.md")).toBeNull();
  });

  it("出典は `task:` をタスクへ、`url:` は http(s) だけ外へ、それ以外は文字のまま", () => {
    expect(knowledgeSource("task:01TASK")).toEqual({
      kind: "task",
      label: "タスク 01TASK",
      href: "/tasks/01TASK",
    });
    expect(knowledgeSource("message:01MSG")).toEqual({ kind: "message", label: "対話 01MSG" });
    expect(knowledgeSource("human")).toEqual({ kind: "human", label: "人" });
    expect(knowledgeSource("url:https://example.invalid/a")).toEqual({
      kind: "url",
      label: "https://example.invalid/a",
      href: "https://example.invalid/a",
    });
    // `javascript:` を踏ませない（http / https 以外はリンクにしない）。
    expect(knowledgeSource("url:javascript:alert(1)").kind).toBe("other");
    expect(knowledgeSource("なにか").kind).toBe("other");
  });

  it("確度の言葉と色", () => {
    expect(confidenceLabel("high")).toBe("確度 高");
    expect(confidenceLabel("low")).toBe("確度 低");
    expect(confidenceLabel(null)).toBeNull();
    expect(confidenceTone("high")).toBe("success");
    expect(confidenceTone("low")).toBe("warning");
    expect(confidenceTone("medium")).toBe("neutral");
  });
});

describe("言葉", () => {
  it("ナビの名前と、弾かれた理由の案内", () => {
    expect(KNOWLEDGE_NAV_LABEL).toBe("知識");
    expect(knowledgeErrorHint("etag_mismatch")).toContain("再読み込み");
    expect(knowledgeErrorHint("page_exists")).toContain("上書き");
    expect(knowledgeErrorHint("knowledge_unavailable")).toContain("celerisctl knowledge init");
    expect(knowledgeErrorHint("path_forbidden")).toContain("_inbox");
    expect(knowledgeErrorHint("candidate_not_found")).toContain("再読み込み");
    expect(knowledgeErrorHint("unknown_code")).toBeNull();
  });

  /** ADR-0047 D4（Phase 62）: `op` の色と、accept したときに何が起きるかのヒント。 */
  it("知識整理 run の候補の op（create/update/merge/retire）", () => {
    expect(knowledgeOpTone("create")).toBe("info");
    expect(knowledgeOpTone("update")).toBe("neutral");
    expect(knowledgeOpTone("merge")).toBe("warning");
    expect(knowledgeOpTone("retire")).toBe("danger");
    expect(knowledgeOpTone(null)).toBe("neutral");
    expect(knowledgeOpTone(undefined)).toBe("neutral");

    expect(knowledgeOpHint("merge")).toContain("上書き");
    expect(knowledgeOpHint("retire")).toContain("_retired/");
    expect(knowledgeOpHint("create")).toBeNull();
    expect(knowledgeOpHint(null)).toBeNull();
  });
});

describe("celeris の中継", () => {
  it("ツリーと選んだページを読む（`?q=` / `?scope=` はそのまま渡す）", async () => {
    serveKnowledge(mock);
    const data = await loadKnowledge(client, {
      path: "environment/clusters/pegasus.md",
      q: "pegasus",
      scope: "environment",
    });
    expect(data.tree?.items).toHaveLength(2);
    expect(data.tree?.inbox_count).toBe(2);
    expect(data.page?.title).toBe("pegasus の使い方");
    expect(data.treeError).toBeNull();
    expect(data.pageError).toBeNull();
    expect(mock.requests[0].url).toContain("q=pegasus");
    expect(mock.requests[0].url).toContain("scope=environment");
    expect(mock.requests[1].url).toContain("path=environment%2Fclusters%2Fpegasus.md");
  });

  it("`celerisctl knowledge init` がまだなら `initialized: false` をそのまま渡す", async () => {
    mock.on("GET", "/api/v1/knowledge/tree", (_req, res) => sendJson(res, 200, knowledgeTreeUninitialized()));
    const data = await loadKnowledge(client, {});
    expect(data.tree?.initialized).toBe(false);
    expect(data.tree?.root).toBe("/home/celeris/knowledge");
    expect(data.treeError).toBeNull();
  });

  it("`[knowledge] root` が無ければ `knowledge_unavailable` を data にする", async () => {
    mock.on("GET", "/api/v1/knowledge/tree", (_req, res) =>
      sendProblem(res, { status: 409, code: "knowledge_unavailable", detail: "[knowledge] root is not configured" }),
    );
    const data = await loadKnowledge(client, {});
    expect(data.tree).toBeNull();
    expect(data.treeError?.code).toBe("knowledge_unavailable");
  });

  it("選んだページだけ読めなくてもツリーは出す", async () => {
    serveKnowledge(mock);
    mock.on("GET", "/api/v1/knowledge/page", (_req, res) =>
      sendProblem(res, { status: 404, code: "page_not_found", detail: "page not found: user/none.md" }),
    );
    const data = await loadKnowledge(client, { path: "user/none.md" });
    expect(data.tree).not.toBeNull();
    expect(data.pageError?.code).toBe("page_not_found");
  });

  it("`?path=&q=&scope=&edit=1` を読む", () => {
    const query = readKnowledgeQuery(new Request("http://gui.test/knowledge?path=user/a.md&q=x&scope=user&edit=1"));
    expect(query).toEqual({ path: "user/a.md", q: "x", scope: "user", edit: true });
  });

  it("候補の一覧（`target_exists` は celeris が決める）", async () => {
    serveKnowledge(mock);
    const { inbox, error } = await loadKnowledgeInbox(client);
    expect(error).toBeNull();
    expect(inbox?.items).toHaveLength(2);
    expect(inbox?.items[0].target_exists).toBe(true);
    expect(inbox?.items[1].target_exists).toBe(false);
    expect(inbox?.items[0].target).toBe("environment/clusters/pegasus.md");
  });

  /** ADR-0047 D4（Phase 62）: 知識整理 run が書いた候補（`op` 付き）。`record`（Phase 61）の候補は `op` が無い。 */
  it("知識整理 run の候補は `op` を持つ（record の候補は持たない）", async () => {
    serveKnowledge(mock, {
      inbox: knowledgeInbox({
        items: [
          knowledgeCandidate({ id: "merge-1", op: "merge" }),
          knowledgeCandidate({ id: "retire-1", op: "retire" }),
          knowledgeCandidate({ id: "record-1" }),
        ],
      }),
    });
    const { inbox } = await loadKnowledgeInbox(client);
    expect(inbox?.items.map((i) => i.op)).toEqual(["merge", "retire", undefined]);
  });

  it("保存・取り込み・捨てる（管理系）", async () => {
    serveKnowledge(mock);
    const id = knowledgeCandidate().id;

    const saved = await putKnowledgePage(client, {
      path: "user/profile.md",
      body: "# 人のこと\n",
      etag: "abc",
    });
    expect(saved.ok && saved.op === "knowledge_put" && saved.result.unchanged).toBe(false);
    expect(JSON.parse(mock.requests[0].body)).toEqual({ path: "user/profile.md", body: "# 人のこと\n", etag: "abc" });

    const accepted = await acceptKnowledgeCandidate(client, id, { overwrite: true });
    expect(accepted.ok && accepted.op === "knowledge_accept" && accepted.id).toBe(id);
    expect(mock.requests[1].url).toBe(`/api/v1/knowledge/inbox/${id}/accept`);
    expect(JSON.parse(mock.requests[1].body)).toEqual({ overwrite: true });

    const rejected = await rejectKnowledgeCandidate(client, id);
    expect(rejected.ok && rejected.op === "knowledge_reject" && rejected.result.id).toBe(id);
    expect(mock.requests[2].url).toBe(`/api/v1/knowledge/inbox/${id}/reject`);
  });

  it("衝突（409 `etag_mismatch` / `page_exists`）は例外にせず `{ok:false, error}` にする", async () => {
    mock.on("PUT", "/api/v1/knowledge/page", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "etag_mismatch",
        detail: "the page changed since it was read",
        extra: { etag: "9999" },
      }),
    );
    mock.on("POST", "/api/v1/knowledge/inbox/01X/accept", (_req, res) =>
      sendProblem(res, { status: 409, code: "page_exists", detail: "page exists: user/profile.md" }),
    );

    const put = await putKnowledgePage(client, { path: "user/profile.md", body: "x" });
    expect(put.ok).toBe(false);
    if (!put.ok) {
      expect(put.error.status).toBe(409);
      expect(put.error.code).toBe("etag_mismatch");
      expect(knowledgeErrorHint(put.error.code)).toContain("再読み込み");
    }

    const accept = await acceptKnowledgeCandidate(client, "01X", {});
    expect(accept.ok).toBe(false);
    if (!accept.ok) expect(accept.error.code).toBe("page_exists");
  });

  it("フォームの読み取りは空欄をキーごと送らない", () => {
    const form = new FormData();
    form.set("path", "user/profile.md");
    form.set("body", "# 人のこと\n");
    form.set("etag", "");
    form.set("message", "");
    expect(readKnowledgePagePutBody(form)).toEqual({ path: "user/profile.md", body: "# 人のこと\n" });

    // accept は本文を省略してよい（`path` が空なら候補の `target` に任せる）。
    const accept = new FormData();
    expect(readKnowledgeAcceptBody(accept)).toEqual({});
    accept.set("path", "environment/ldr.md");
    accept.set("overwrite", "1");
    expect(readKnowledgeAcceptBody(accept)).toEqual({ path: "environment/ldr.md", overwrite: true });
  });

  it("6 つの経路が全部 mock にある（§3.98〜3.103）", async () => {
    serveKnowledge(mock);
    const id = knowledgeCandidate().id;
    await loadKnowledge(client, { path: "environment/clusters/pegasus.md" });
    await loadKnowledgeInbox(client);
    await putKnowledgePage(client, { path: "user/profile.md", body: "x" });
    await acceptKnowledgeCandidate(client, id, {});
    await rejectKnowledgeCandidate(client, id);
    expect(mock.requests.map((r) => `${r.method} ${r.url.split("?")[0]}`)).toEqual([
      "GET /api/v1/knowledge/tree",
      "GET /api/v1/knowledge/page",
      "GET /api/v1/knowledge/inbox",
      "PUT /api/v1/knowledge/page",
      `POST /api/v1/knowledge/inbox/${id}/accept`,
      `POST /api/v1/knowledge/inbox/${id}/reject`,
    ]);
    // fixtures の形（`KnowledgePageResult` / `KnowledgeRejectResult`）も型で確かめておく。
    expect(knowledgePageResult().etag).toHaveLength(64);
    expect(knowledgeRejectResult().id).toBe(id);
    expect(knowledgeInbox().initialized).toBe(true);
  });
});
