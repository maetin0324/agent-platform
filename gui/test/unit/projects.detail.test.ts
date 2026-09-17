import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadProjectDetail } from "~/routes/projects.$id";
import { TaskdClient } from "~/taskd/client.server";
import { createMilestone, patchMilestoneStatus, patchProjectStatus } from "~/taskd/projects-admin.server";
import type {
  ArtifactList,
  Milestone,
  OrgList,
  Project,
  ProjectDetail,
  Report,
  ReportList,
  TaskDetail,
} from "~/taskd/types";
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
  title: "Pluvio",
  request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証",
  status: "active",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

describe("loadProjectDetail", () => {
  it("案件・途中目標・仕事の木（tasks）と組織を束ねて返す", async () => {
    const detail: ProjectDetail = {
      project: project(),
      milestones: [
        {
          id: "m1",
          project_id: "p1",
          seq: 1,
          title: "調査",
          description: "",
          status: "approved",
          created_at: "…",
          updated_at: "…",
        },
      ],
      tasks: [
        {
          id: "t1",
          title: "survey",
          status: "running",
          parent_id: null,
          depends_on: [],
          assignee: "research-survey",
          milestone_id: "m1",
          conversation: false,
        },
      ],
    };
    const org: OrgList = {
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
    };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, org));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.detail).toEqual(detail);
    expect(result.org).toEqual(org);
  });

  it("GET /org が失敗しても案件の詳細は返す（組織は空扱い）", async () => {
    const detail: ProjectDetail = { project: project(), milestones: [], tasks: [] };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.detail).toEqual(detail);
    expect(result.org).toEqual({ items: [] });
  });

  it("GET /reports?project=<id> を呼び、この案件のすべての段の報告を返す（level を付けない。SPEC §4「報告の流れ」タブ）", async () => {
    const detail: ProjectDetail = { project: project(), milestones: [], tasks: [] };
    const reportsResponse: ReportList = {
      items: [
        {
          id: "r1",
          kind: "proposal",
          level: 1,
          node_id: "research-survey",
          headline: "この framing で論文が書けそう",
          created_at: "…",
        } satisfies Report,
      ],
    };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/reports", (_req, res) => sendJson(res, 200, reportsResponse));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.reports).toEqual(reportsResponse);
    expect(typeof result.fetchedAt).toBe("string");
    const req = mock.requests.find((r) => r.url.startsWith("/api/v1/reports"));
    const url = new URL(req?.url ?? "", "http://mock-taskd.invalid");
    expect(url.searchParams.get("project")).toBe("p1");
    expect(url.searchParams.has("level")).toBe(false);
    expect(url.searchParams.has("unread")).toBe(false);
  });

  it("GET /reports が失敗しても案件の詳細は返す（報告は空扱い）", async () => {
    const detail: ProjectDetail = { project: project(), milestones: [], tasks: [] };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/reports", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));
    expect(result.reports).toEqual({ items: [] });
  });

  it("GET /projects/{id} の tasks ぶん GET /tasks/{id} と GET /tasks/{id}/artifacts を束ね、成果物一覧（artifactRows）を組む（Phase G13c）", async () => {
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
    const org: OrgList = {
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
    };
    const taskDetail: TaskDetail = {
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
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, org));
    mock.on("GET", "/api/v1/reports", (_req, res) => sendJson(res, 200, { items: [] } satisfies ReportList));
    mock.on("GET", "/api/v1/tasks/t1", (_req, res) => sendJson(res, 200, taskDetail));
    mock.on("GET", "/api/v1/tasks/t1/artifacts", (_req, res) => sendJson(res, 200, artifacts));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.artifactRows).toHaveLength(1);
    expect(result.artifactRows[0]).toMatchObject({
      taskId: "t1",
      taskTitle: "survey",
      assigneeName: "関連研究調査課",
      workspace: {
        text: "/home/user/workspace/lab/pluvio-survey",
        vscodeHref: "vscode://file/home/user/workspace/lab/pluvio-survey",
      },
    });
    expect(result.artifactRows[0].artifact).toEqual(artifacts.items[0]);
  });

  it("404 project_not_found は例外として投げる（loader が Response に変換する）", async () => {
    mock.on("GET", "/api/v1/projects/missing", (_req, res) =>
      sendProblem(res, { status: 404, code: "project_not_found", detail: "no such project" }),
    );
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));

    await expect(
      loadProjectDetail(client, "missing", new Request("http://gui.invalid/projects/missing")),
    ).rejects.toBeTruthy();
  });
});

describe("patchProjectStatus (PATCH /projects/{id})", () => {
  it("success", async () => {
    const updated = project({ status: "done" });
    mock.on("PATCH", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, updated));

    const result = await patchProjectStatus(client, "p1", "done");

    expect(result).toEqual({ ok: true, op: "project_status", project: updated });
  });

  it("400 bad_request（知らない値）はそのまま ActionError にする", async () => {
    mock.on("PATCH", "/api/v1/projects/p1", (_req, res) =>
      sendProblem(res, { status: 400, code: "bad_request", detail: "invalid status" }),
    );
    const result = await patchProjectStatus(client, "p1", "done");
    expect(result.ok).toBe(false);
  });
});

describe("createMilestone (POST /projects/{id}/milestones)", () => {
  it("title だけでも作れる（description/status は省略可、既定 proposed）", async () => {
    const created: Milestone = {
      id: "m1",
      project_id: "p1",
      seq: 1,
      title: "調査",
      description: "",
      status: "proposed",
      created_at: "…",
      updated_at: "…",
    };
    mock.on("POST", "/api/v1/projects/p1/milestones", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ title: "調査" });
      sendJson(res, 201, created);
    });

    const form = new FormData();
    form.set("title", "調査");
    const result = await createMilestone(client, "p1", form);

    expect(result).toEqual({ ok: true, op: "milestone_create", milestone: created });
  });

  it("404 project_not_found を ActionError として返す", async () => {
    mock.on("POST", "/api/v1/projects/missing/milestones", (_req, res) =>
      sendProblem(res, { status: 404, code: "project_not_found", detail: "no such project" }),
    );
    const form = new FormData();
    form.set("title", "x");
    const result = await createMilestone(client, "missing", form);
    expect(result.ok).toBe(false);
  });
});

describe("patchMilestoneStatus (PATCH /milestones/{id})", () => {
  it("success — SPEC §7 のアジャイル判定（Go か再設計か）", async () => {
    const updated: Milestone = {
      id: "m1",
      project_id: "p1",
      seq: 1,
      title: "調査",
      description: "",
      status: "reached",
      created_at: "…",
      updated_at: "…",
    };
    mock.on("PATCH", "/api/v1/milestones/m1", (_req, res) => sendJson(res, 200, updated));

    const result = await patchMilestoneStatus(client, "m1", "reached");

    expect(result).toEqual({ ok: true, op: "milestone_status", milestone: updated });
  });

  it("404 milestone_not_found を ActionError として返す", async () => {
    mock.on("PATCH", "/api/v1/milestones/missing", (_req, res) =>
      sendProblem(res, { status: 404, code: "milestone_not_found", detail: "no such milestone" }),
    );
    const result = await patchMilestoneStatus(client, "missing", "reached");
    expect(result.ok).toBe(false);
  });
});
