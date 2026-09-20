import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { CelerisError } from "~/celeris/errors";
import type { MilestoneView, OrgList, ProjectDetail, ProjectList } from "~/celeris/types";
import { loadBoard } from "~/routes/board";
import { taskSummary } from "../mock-celeris/fixtures";
import { type MockCeleris, sendJson, sendProblem, serveTaskManagement, startMockCeleris } from "../mock-celeris/server";

/**
 * `/board`（ADR-0044 D4）の loader。**絞り込みは celeris に丸投げ**なので、ここで見るのは
 * 「URL の検索パラメータが `GET /tasks` のクエリにどう写るか」と「補助の読み取りが落ちても出ること」。
 */

let mock: MockCeleris;
let client: CelerisClient;

const PROJECT_ID = "01PROJECT000000000000001";

const projects: ProjectList = {
  items: [
    {
      id: PROJECT_ID,
      title: "Pluvio",
      request: "…",
      status: "active",
      created_at: "2026-09-19T00:00:00Z",
      updated_at: "2026-09-19T00:00:00Z",
    },
  ],
};

const milestone: MilestoneView = {
  id: "01MILESTONE0000000000001",
  project_id: PROJECT_ID,
  seq: 1,
  title: "調査",
  status: "in_progress",
  created_at: "2026-09-19T00:00:00Z",
  updated_at: "2026-09-19T00:00:00Z",
};

const org: OrgList = {
  items: [
    {
      id: "research-survey",
      name: "研究文献調査課",
      kind: "section",
      parent_id: "research",
      created_at: "2026-09-19T00:00:00Z",
      updated_at: "2026-09-19T00:00:00Z",
    },
  ],
};

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
  serveTaskManagement(mock, { items: [taskSummary()] });
  mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, projects));
  mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, org));
  mock.on("GET", `/api/v1/projects/${PROJECT_ID}`, (_req, res) =>
    sendJson(res, 200, {
      project: projects.items[0],
      milestones: [milestone],
      tasks: [],
    } satisfies ProjectDetail),
  );
});

afterEach(async () => {
  await mock.close();
});

function tasksQuery(): string {
  const req = mock.requests.find((r) => r.method === "GET" && r.url.startsWith("/api/v1/tasks?"));
  return req ? req.url.slice("/api/v1/tasks?".length) : "";
}

describe("loadBoard", () => {
  it("条件なしでも `GET /tasks` を引く（上限と並び順だけ付く）", async () => {
    const result = await loadBoard(client, new Request("http://gui.invalid/board"));

    expect(result.tasks.items).toHaveLength(1);
    expect(result.projects.items).toHaveLength(1);
    expect(result.org).toHaveLength(1);
    // 案件を選んでいないので途中目標は引かない。
    expect(result.milestones).toEqual([]);
    expect(mock.requests.some((r) => r.url === `/api/v1/projects/${PROJECT_ID}`)).toBe(false);
    expect(tasksQuery()).toBe("limit=500&order=created_desc");
  });

  it("フィルタをそのまま `GET /tasks` のクエリに写す（繰り返しは繰り返しのまま。AND は celeris の仕事）", async () => {
    await loadBoard(
      client,
      new Request(
        `http://gui.invalid/board?project=${PROJECT_ID}&label=pluvio&label=survey&category=research&category=docs&assignee=research-survey&milestone=01MILESTONE0000000000001&tier=frontier&priority=P0&priority=P1&q=%E9%96%A2%E9%80%A3%E7%A0%94%E7%A9%B6`,
      ),
    );

    const query = new URLSearchParams(tasksQuery());
    expect(query.get("project")).toBe(PROJECT_ID);
    expect(query.getAll("label")).toEqual(["pluvio", "survey"]);
    expect(query.getAll("category")).toEqual(["research", "docs"]);
    expect(query.get("assignee")).toBe("research-survey");
    expect(query.get("milestone")).toBe("01MILESTONE0000000000001");
    expect(query.getAll("tier")).toEqual(["frontier"]);
    expect(query.getAll("priority")).toEqual(["P0", "P1"]);
    expect(query.get("q")).toBe("関連研究");
    expect(query.get("limit")).toBe("500");
  });

  it("空欄の条件は送らない（`?project=&q=` で全件になる）", async () => {
    await loadBoard(client, new Request("http://gui.invalid/board?project=&q=&label="));

    expect(tasksQuery()).toBe("limit=500&order=created_desc");
  });

  it("案件を選ぶとその案件の途中目標を引く（絞り込みの選択肢とカードの表示）", async () => {
    const result = await loadBoard(client, new Request(`http://gui.invalid/board?project=${PROJECT_ID}`));

    expect(result.milestones).toEqual([milestone]);
  });

  it("案件・組織・途中目標が落ちてもボードは出す（タスクだけは必須）", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );
    mock.on("GET", "/api/v1/org", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));
    mock.on("GET", `/api/v1/projects/${PROJECT_ID}`, (_req, res) =>
      sendProblem(res, { status: 404, code: "project_not_found", detail: "no" }),
    );

    const result = await loadBoard(client, new Request(`http://gui.invalid/board?project=${PROJECT_ID}`));

    expect(result.tasks.items).toHaveLength(1);
    expect(result.projects.items).toEqual([]);
    expect(result.org).toEqual([]);
    expect(result.milestones).toEqual([]);
  });

  it("`GET /tasks` のエラー（知らないラベル等の 400）はそのまま投げる", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) =>
      sendProblem(res, {
        status: 400,
        code: "bad_request",
        detail: 'query parameter `label` must match [a-z0-9-] (got "Pluvio")',
      }),
    );

    let error: unknown;
    try {
      await loadBoard(client, new Request("http://gui.invalid/board?label=Pluvio"));
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisError);
    expect((error as CelerisError).status).toBe(400);
  });
});
