import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import type { OrgList, Project, ProjectDetail } from "~/celeris/types";
import { PRIMARY_REPO_MARK, repoKindLabel, repoRunLabel, repoSyncLabel } from "~/lib/labels";
import { primaryRepo, repoLocationText, repoPlaceOf } from "~/lib/repo-form";
import { loadProjectDetail } from "~/routes/projects.$id";
import { projectRepo } from "../mock-celeris/fixtures";
import { type MockCeleris, sendJson, startMockCeleris } from "../mock-celeris/server";

/**
 * 案件の「リポジトリ」節（ADR-0043 D1、docs/celeris-api-v1.md §3.68〜3.71。Phase 52 / G16）。
 * DOM を描画する unit テストがこのリポジトリに無い（G10-U1）ので、
 * (1) loader が `ProjectDetail.repos` をそのまま通すこと、
 * (2) 行に出す文言・初期値を決める純粋関数、
 * (3) ルートが 4 つの intent を受けて節を描いていること（ソースの確認。`action-feedback.test.ts` と同じ作り）
 * の 3 つで見る。
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

const project = (over: Partial<Project> = {}): Project => ({
  id: "p1",
  title: "benchfs",
  request: "…",
  status: "active",
  created_at: "2026-09-19T00:00:00Z",
  updated_at: "2026-09-19T00:00:00Z",
  ...over,
});

describe("loadProjectDetail の repos（§3.47 / ADR-0043 D1）", () => {
  it("ProjectDetail.repos を並べ替えずそのまま通す（primary が先頭なのは celeris が決める）", async () => {
    const repos = [
      projectRepo(),
      projectRepo({
        id: "01MOCKREPO0000000000000002",
        name: "benchfs-paper",
        kind: "dir",
        location: { kind: "local", path: "/home/mock/workspace/papers/benchfs" },
        default_branch: null,
        is_primary: false,
      }),
    ];
    const detail: ProjectDetail = {
      project: project({ workspace: { kind: "local", path: "/home/mock/workspace/rust/benchfs" } }),
      milestones: [],
      tasks: [],
      repos,
    };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.detail.repos).toEqual(repos);
    expect(result.detail.repos?.[0].name).toBe("benchfs");
    // `Project.workspace` は primary の location の写し（GUI の後方互換）。
    expect(result.detail.project.workspace).toEqual({ kind: "local", path: "/home/mock/workspace/rust/benchfs" });
  });

  it("repos が無い（Phase 52 より前の）応答でも案件の詳細は出る", async () => {
    const detail: ProjectDetail = { project: project(), milestones: [], tasks: [] };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.detail.repos).toBeUndefined();
  });
});

describe("行に出す値（純粋関数）", () => {
  it("手元のリポジトリはパスそのまま、クラスタは cluster:path", () => {
    expect(repoLocationText({ kind: "local", path: "/home/u/workspace/benchfs" })).toBe("/home/u/workspace/benchfs");
    expect(repoLocationText({ kind: "remote", cluster: "pegasus", path: "/work/benchfs" })).toBe(
      "pegasus:/work/benchfs",
    );
  });

  it("編集フォームの置き場所の初期値", () => {
    expect(repoPlaceOf({ kind: "local", path: "/x" })).toBe("local");
    expect(repoPlaceOf({ kind: "remote", cluster: "c", path: "/x" })).toBe("remote");
    expect(repoPlaceOf(null)).toBe("local");
  });

  it("primary の 1 件を取り出す（無ければ null）", () => {
    const repos = [projectRepo({ is_primary: false }), projectRepo({ id: "r2", name: "b", is_primary: true })];
    expect(primaryRepo(repos)?.name).toBe("b");
    expect(primaryRepo([])).toBeNull();
    expect(primaryRepo(undefined)).toBeNull();
  });

  it("種類・実行環境・同期の日本語（知らない値は素のまま）", () => {
    expect(repoKindLabel("git")).toBe("git");
    expect(repoKindLabel("dir")).toBe("ディレクトリ");
    expect(repoKindLabel("svn")).toBe("svn");
    expect(repoRunLabel("auto")).toBe("自動");
    expect(repoRunLabel("host")).toBe("ホスト");
    expect(repoRunLabel("container")).toBe("コンテナ");
    expect(repoSyncLabel("worktree")).toBe("worktree");
    expect(repoSyncLabel("none")).toBe("同期しない");
    expect(PRIMARY_REPO_MARK).toBe("主");
  });
});

function readSource(relative: string): string {
  return readFileSync(fileURLToPath(new URL(`../../app/${relative}`, import.meta.url)), "utf8");
}

describe("/projects/:id が「リポジトリ」節を持つ", () => {
  const route = readSource("routes/projects.$id.tsx");

  it("ProjectRepos を描いている", () => {
    expect(route).toContain("ProjectRepos");
    expect(route).toContain('data-testid="project-repos-section"');
  });

  for (const intent of ["repo_create", "repo_patch", "repo_primary", "repo_delete"]) {
    it(`action が intent ${intent} を受ける`, () => {
      expect(route).toContain(`case "${intent}":`);
    });
  }

  it("並べ替えず celeris の順で出す（ソート関数を呼ばない）", () => {
    expect(route).not.toMatch(/repos[^\n]*\.sort\(/);
  });
});

describe("/projects の「追加のリポジトリ」", () => {
  const route = readSource("routes/projects.tsx");

  it("従来の単一の workspace フォーム（WorkspaceFields / readProjectCreateInput）はそのまま残っている", () => {
    expect(route).toContain("WorkspaceFields");
    expect(route).toContain("readProjectCreateInput");
    expect(route).toContain('data-testid="project-new-form"');
  });

  it("案件の 201 のあとに POST /projects/{id}/repos を行ごとに送る", () => {
    expect(route).toContain("readExtraRepoCreateBodies");
    expect(route).toContain("createRepo");
  });
});
