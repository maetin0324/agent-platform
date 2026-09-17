import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadProjectDetail } from "~/routes/projects.$id";
import { TaskdClient } from "~/taskd/client.server";
import { createMilestone, patchMilestoneStatus, patchProjectStatus } from "~/taskd/projects-admin.server";
import type { Milestone, OrgList, Project, ProjectDetail } from "~/taskd/types";
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
