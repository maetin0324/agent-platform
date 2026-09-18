import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadArtifacts } from "~/routes/artifacts";
import { TaskdClient } from "~/taskd/client.server";
import type { ArtifactList, OrgList, Project, ProjectDetail, ProjectList, TaskDetail } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

const project = (over: Partial<Project> = {}): Project => ({
  id: "p1",
  title: "Pluvio の新テーマ",
  request: "…",
  status: "active",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

const taskDetail = (over: Partial<TaskDetail> = {}): TaskDetail => ({
  task: {
    id: "t1",
    kind: "execute",
    status: "done",
    title: "survey",
    objective: "survey",
    priority: 0,
    attempts: 1,
    created_at: "…",
    updated_at: "…",
    acceptance: [],
    depends_on: [],
    inputs: [],
    worker_hint: { tier: "standard" },
    budget: { max_retries: 3, max_turns: 10, max_wall_secs: 600 },
    workspace: { kind: "local", path: "lab/pluvio-survey" },
    assignee: "research-survey",
  },
  workspace_dir: "/home/user/workspace/lab/pluvio-survey",
  timers: { now: "…", consecutive_requeues: 0, consecutive_reviewer_requeues: 0, max_requeues: 3 },
  criteria: [],
  runs: [],
  prior_review: [],
  answers: [],
  latest_question: null,
  approvals: [],
  dependencies: [],
  dependents: [],
  children: [],
  actions: [],
  worker_run_hint: null,
  delegated: [],
  ...over,
});

describe("loadArtifacts", () => {
  it("project クエリが無ければ、案件の一覧だけを返す（GET /tasks 等は呼ばない）", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [project()] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));

    const result = await loadArtifacts(client, new Request("http://gui.invalid/artifacts"));

    expect(result.projects).toEqual([project()]);
    expect(result.selectedProjectId).toBeNull();
    expect(result.projectNotFound).toBe(false);
    expect(result.rows).toEqual([]);
  });

  it("案件を選ぶと、その案件のタスクごとの成果物を GET /tasks/{id}/artifacts と GET /tasks/{id} で束ねて返す", async () => {
    const detail: ProjectDetail = {
      project: project(),
      milestones: [],
      tasks: [
        {
          id: "t1",
          title: "survey",
          status: "done",
          parent_id: null,
          depends_on: [],
          assignee: "research-survey",
          conversation: false,
        },
      ],
    };
    const artifacts: ArtifactList = {
      items: [
        {
          idx: 0,
          run_id: "run1",
          ts: "2026-09-17T00:00:00Z",
          artifact: { name: "report.md", path: "artifacts/report.md", sha256: "abc", kind: "markdown" },
          exists: true,
          forbidden: false,
          size: 10,
          sha256_current: "abc",
          sha256_matches: true,
        },
      ],
    };
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [project()] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) =>
      sendJson(res, 200, {
        items: [
          {
            id: "research-survey",
            parent_id: "research",
            name: "関連研究調査課",
            kind: "section",
            position: 0,
            created_at: "…",
            updated_at: "…",
          },
        ],
      } satisfies OrgList),
    );
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/tasks/t1", (_req, res) => sendJson(res, 200, taskDetail()));
    mock.on("GET", "/api/v1/tasks/t1/artifacts", (_req, res) => sendJson(res, 200, artifacts));

    const result = await loadArtifacts(client, new Request("http://gui.invalid/artifacts?project=p1"));

    expect(result.selectedProjectId).toBe("p1");
    expect(result.projectNotFound).toBe(false);
    expect(result.rows).toHaveLength(1);
    const row = result.rows[0];
    expect(row.taskId).toBe("t1");
    expect(row.taskTitle).toBe("survey");
    expect(row.assigneeName).toBe("関連研究調査課");
    expect(row.workspace).toEqual({
      text: "/home/user/workspace/lab/pluvio-survey",
      vscodeHref: "vscode://file/home/user/workspace/lab/pluvio-survey",
      localCopyNote: null,
    });
    expect(row.artifact).toEqual(artifacts.items[0]);
  });

  it("存在しない案件（404 project_not_found）は projectNotFound: true を返す（例外にしない）", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [project()] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects/missing", (_req, res) =>
      sendProblem(res, { status: 404, code: "project_not_found", detail: "no such project" }),
    );

    const result = await loadArtifacts(client, new Request("http://gui.invalid/artifacts?project=missing"));

    expect(result.selectedProjectId).toBe("missing");
    expect(result.projectNotFound).toBe(true);
    expect(result.rows).toEqual([]);
  });

  it("タスクの GET /tasks/{id} や GET /tasks/{id}/artifacts が失敗しても、他のタスクの行は返す", async () => {
    const detail: ProjectDetail = {
      project: project(),
      milestones: [],
      tasks: [
        {
          id: "t1",
          title: "survey",
          status: "done",
          parent_id: null,
          depends_on: [],
          assignee: null,
          conversation: false,
        },
        {
          id: "t2",
          title: "poc",
          status: "running",
          parent_id: null,
          depends_on: [],
          assignee: null,
          conversation: false,
        },
      ],
    };
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [project()] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    // t1: 両方失敗（500）。t2: 成功。
    mock.on("GET", "/api/v1/tasks/t1", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );
    mock.on("GET", "/api/v1/tasks/t1/artifacts", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );
    mock.on("GET", "/api/v1/tasks/t2", (_req, res) =>
      sendJson(res, 200, taskDetail({ task: { ...taskDetail().task, id: "t2", title: "poc", assignee: null } })),
    );
    mock.on("GET", "/api/v1/tasks/t2/artifacts", (_req, res) =>
      sendJson(res, 200, {
        items: [
          {
            idx: 0,
            run_id: "run2",
            ts: "2026-09-17T00:00:00Z",
            artifact: { name: "sources.json", path: "artifacts/sources.json", sha256: "def", kind: "json" },
            exists: true,
            forbidden: false,
            size: 5,
            sha256_current: "def",
            sha256_matches: true,
          },
        ],
      } satisfies ArtifactList),
    );

    const result = await loadArtifacts(client, new Request("http://gui.invalid/artifacts?project=p1"));

    expect(result.rows).toHaveLength(1);
    expect(result.rows[0].taskId).toBe("t2");
  });
});
