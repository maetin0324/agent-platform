import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { TaskdClient } from "~/taskd/client.server";
import {
  createRepo,
  deleteRepo,
  listRepos,
  patchRepo,
  readExtraRepoCreateBodies,
  readRepoCreateBody,
  readRepoPatchBody,
  setPrimaryRepo,
} from "~/taskd/repos-admin.server";
import { defaultRepoList, projectRepo } from "../mock-taskd/fixtures";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * 案件のリポジトリ（ADR-0043 D1、docs/taskd-api-v1.md §3.68〜3.71。Phase 52 / G16）。
 * フォームの読み手（`RepoCreateBody` / `RepoPatchBody`）と 5 本の中継を検証する。
 * GUI 側では検証しないので、空の値・知らないクラスタ・使用中の削除は taskd の 409 / 422 をそのまま返す。
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

function form(entries: Array<[string, string]>): FormData {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
}

describe("readRepoCreateBody（§3.69 の要求本文）", () => {
  it("パスだけ書けば location だけを送る（name / kind / default_branch / run は省略）", () => {
    expect(
      readRepoCreateBody(
        form([
          ["repo_place", "local"],
          ["repo_path", "~/workspace/rust/benchfs"],
          ["repo_name", ""],
          ["repo_kind", ""],
          ["repo_default_branch", ""],
          ["repo_run", ""],
        ]),
      ),
    ).toEqual({ location: { kind: "local", path: "~/workspace/rust/benchfs" } });
  });

  it("空欄はキーごと送らない（`name` を省略すると taskd がパスの末尾から slug を作る）", () => {
    const body = readRepoCreateBody(form([["repo_path", "~/workspace/rust/benchfs"]]));
    expect(Object.keys(body)).toEqual(["location"]);
    expect("name" in body).toBe(false);
    expect("kind" in body).toBe(false);
    expect("run" in body).toBe(false);
    expect("default_branch" in body).toBe(false);
  });

  it("kind = auto は「taskd に決めさせる」なのでキーを送らない（RepoKind に auto は無い）", () => {
    const body = readRepoCreateBody(
      form([
        ["repo_kind", "auto"],
        ["repo_path", "/x"],
      ]),
    );
    expect("kind" in body).toBe(false);
  });

  it("全部書いた場合", () => {
    expect(
      readRepoCreateBody(
        form([
          ["repo_name", "benchfs"],
          ["repo_kind", "git"],
          ["repo_place", "local"],
          ["repo_path", "~/workspace/rust/benchfs"],
          ["repo_default_branch", "main"],
          ["repo_run", "host"],
        ]),
      ),
    ).toEqual({
      name: "benchfs",
      kind: "git",
      location: { kind: "local", path: "~/workspace/rust/benchfs" },
      default_branch: "main",
      run: "host",
    });
  });

  it("クラスタ（remote）は {kind:'remote', cluster, path}", () => {
    expect(
      readRepoCreateBody(
        form([
          ["repo_place", "remote"],
          ["repo_cluster", "pegasus"],
          ["repo_path", "/work/NBB/rmaeda/workspace/rust/benchfs"],
        ]),
      ),
    ).toEqual({
      location: { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/workspace/rust/benchfs" },
    });
  });

  it("空のパス・知らないクラスタも検証せずそのまま組む（taskd の 422 に委ねる）", () => {
    expect(readRepoCreateBody(form([["repo_place", "remote"]]))).toEqual({
      location: { kind: "remote", cluster: "", path: "" },
    });
  });
});

describe("readRepoPatchBody（§3.70 の要求本文）", () => {
  it("行の編集は name / location / default_branch / run を送る", () => {
    expect(
      readRepoPatchBody(
        form([
          ["repo_name", "benchfs2"],
          ["repo_place", "local"],
          ["repo_path", "/home/u/workspace/benchfs"],
          ["repo_default_branch", "develop"],
          ["repo_run", "container"],
        ]),
      ),
    ).toEqual({
      name: "benchfs2",
      location: { kind: "local", path: "/home/u/workspace/benchfs" },
      default_branch: "develop",
      run: "container",
    });
  });

  it("default_branch を空欄にすると null を明示して消す", () => {
    const body = readRepoPatchBody(
      form([
        ["repo_name", "benchfs"],
        ["repo_place", "local"],
        ["repo_path", "/x"],
        ["repo_default_branch", ""],
        ["repo_run", "auto"],
      ]),
    );
    expect(body.default_branch).toBeNull();
  });

  it("run が空なら run キーを送らない（変えない）", () => {
    const body = readRepoPatchBody(
      form([
        ["repo_place", "local"],
        ["repo_path", "/x"],
      ]),
    );
    expect("run" in body).toBe(false);
  });
});

describe("readExtraRepoCreateBodies（/projects の「追加のリポジトリ」）", () => {
  it("行が無ければ空", () => {
    expect(readExtraRepoCreateBodies(new FormData())).toEqual([]);
  });

  it("行ごとに列を突き合わせて RepoCreateBody を並べる", () => {
    const f = form([
      ["extra_repo_name", "paper"],
      ["extra_repo_kind", ""],
      ["extra_repo_place", "local"],
      ["extra_repo_path", "~/workspace/papers/benchfs"],
      ["extra_repo_cluster", ""],
      ["extra_repo_default_branch", ""],
      ["extra_repo_run", "auto"],
      ["extra_repo_name", ""],
      ["extra_repo_kind", "git"],
      ["extra_repo_place", "remote"],
      ["extra_repo_path", "/work/NBB/rmaeda/data"],
      ["extra_repo_cluster", "pegasus"],
      ["extra_repo_default_branch", "main"],
      ["extra_repo_run", "host"],
    ]);
    expect(readExtraRepoCreateBodies(f)).toEqual([
      { name: "paper", location: { kind: "local", path: "~/workspace/papers/benchfs" }, run: "auto" },
      {
        kind: "git",
        location: { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/data" },
        default_branch: "main",
        run: "host",
      },
    ]);
  });

  it("パスが空の行（足しただけの行）は送らない", () => {
    const f = form([
      ["extra_repo_name", ""],
      ["extra_repo_kind", ""],
      ["extra_repo_place", "local"],
      ["extra_repo_path", "   "],
      ["extra_repo_cluster", ""],
      ["extra_repo_default_branch", ""],
      ["extra_repo_run", "auto"],
    ]);
    expect(readExtraRepoCreateBodies(f)).toEqual([]);
  });
});

describe("listRepos (GET /projects/{id}/repos, §3.68)", () => {
  it("taskd の並び（primary が先頭）をそのまま返す", async () => {
    mock.on("GET", "/api/v1/projects/p1/repos", (_req, res) => sendJson(res, 200, defaultRepoList));
    const list = await listRepos(client, "p1");
    expect(list.items).toHaveLength(2);
    expect(list.items[0].is_primary).toBe(true);
    expect(list.items[0].name).toBe("benchfs");
  });
});

describe("createRepo (POST /projects/{id}/repos, §3.69)", () => {
  it("201 — 本文をそのまま送る", async () => {
    const created = projectRepo();
    mock.on("POST", "/api/v1/projects/p1/repos", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ location: { kind: "local", path: "~/workspace/rust/benchfs" } });
      sendJson(res, 201, created);
    });
    const outcome = await createRepo(client, "p1", { location: { kind: "local", path: "~/workspace/rust/benchfs" } });
    expect(outcome).toEqual({ ok: true, op: "repo_create", repo: created });
  });

  it("422 validation（不正な名前）は例外にせず taskd の文言のまま返す", async () => {
    mock.on("POST", "/api/v1/projects/p1/repos", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: 'invalid repo name: "../evil"',
        extra: { errors: [{ field: "name", message: 'invalid repo name: "../evil"' }] },
      }),
    );
    const outcome = await createRepo(client, "p1", { name: "../evil", location: { kind: "local", path: "/x" } });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.op).toBe("repo_create");
      expect(outcome.error.status).toBe(422);
      expect(outcome.error.code).toBe("validation");
      expect(outcome.error.detail).toBe('invalid repo name: "../evil"');
      expect(outcome.error.fields.name).toEqual(['invalid repo name: "../evil"']);
    }
  });

  it("401 unauthorized（管理系。token_file 未設定でも 401）", async () => {
    mock.on("POST", "/api/v1/projects/p1/repos", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const outcome = await createRepo(client, "p1", { location: { kind: "local", path: "/x" } });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.code).toBe("unauthorized");
  });
});

describe("patchRepo / setPrimaryRepo (PATCH /repos/{id}, §3.70)", () => {
  it("書いたものだけ送る", async () => {
    const updated = projectRepo({ name: "benchfs2" });
    mock.on("PATCH", "/api/v1/repos/01MOCKREPO0000000000000001", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ name: "benchfs2" });
      sendJson(res, 200, updated);
    });
    const outcome = await patchRepo(client, "01MOCKREPO0000000000000001", { name: "benchfs2" });
    expect(outcome).toEqual({ ok: true, op: "repo_patch", repo: updated });
  });

  it("「主にする」は is_primary: true だけを送る（false は送らない）", async () => {
    const updated = projectRepo({ id: "01MOCKREPO0000000000000002", name: "benchfs-paper", is_primary: true });
    mock.on("PATCH", "/api/v1/repos/01MOCKREPO0000000000000002", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ is_primary: true });
      sendJson(res, 200, updated);
    });
    const outcome = await setPrimaryRepo(client, "01MOCKREPO0000000000000002");
    expect(outcome).toEqual({ ok: true, op: "repo_primary", repo: updated });
  });

  it("404 repo_not_found", async () => {
    mock.on("PATCH", "/api/v1/repos/nope", (_req, res) =>
      sendProblem(res, { status: 404, code: "repo_not_found", detail: "repo nope not found" }),
    );
    const outcome = await patchRepo(client, "nope", { name: "x" });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.detail).toBe("repo nope not found");
  });
});

describe("deleteRepo (DELETE /repos/{id}, §3.71)", () => {
  it("204 は本文なしでも成功として扱う", async () => {
    mock.on("DELETE", "/api/v1/repos/01MOCKREPO0000000000000002", (_req, res) => {
      res.writeHead(204);
      res.end();
    });
    const outcome = await deleteRepo(client, "01MOCKREPO0000000000000002");
    expect(outcome).toEqual({ ok: true, op: "repo_delete", repoId: "01MOCKREPO0000000000000002" });
  });

  it("409 repo_in_use（未終端のタスクが使っている）は taskd の文言のまま返す", async () => {
    mock.on("DELETE", "/api/v1/repos/01MOCKREPO0000000000000001", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "repo_in_use",
        detail: 'repository "benchfs" is used by 2 unfinished tasks',
      }),
    );
    const outcome = await deleteRepo(client, "01MOCKREPO0000000000000001");
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.op).toBe("repo_delete");
      expect(outcome.error.status).toBe(409);
      expect(outcome.error.code).toBe("repo_in_use");
      expect(outcome.error.detail).toBe('repository "benchfs" is used by 2 unfinished tasks');
      // 409 は「状態が変わりました」として loader の再検証で最新にする（ActionError.conflict）。
      expect(outcome.error.conflict).toBe(true);
    }
  });
});
