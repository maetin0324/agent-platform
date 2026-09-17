import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadProjects } from "~/routes/projects";
import { TaskdClient } from "~/taskd/client.server";
import { createProject, readProjectCreateInput } from "~/taskd/projects-admin.server";
import type { Project, ProjectDetail, ProjectList } from "~/taskd/types";
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

const project = (id: string, over: Partial<Project> = {}): Project => ({
  id,
  title: id,
  request: "…",
  status: "proposed",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

describe("readProjectCreateInput", () => {
  it("reads title/request from a FormData (sent as-is, taskd validates)", () => {
    const form = new FormData();
    form.set("title", "Pluvio の新テーマ");
    form.set("request", "Pluvio を基盤に用いた新たな研究テーマの模索、検証");
    expect(readProjectCreateInput(form)).toEqual({
      title: "Pluvio の新テーマ",
      request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証",
    });
  });

  it("blank fields stay blank (GUI does not validate; taskd's 422 is what the user sees)", () => {
    expect(readProjectCreateInput(new FormData())).toEqual({ title: "", request: "" });
  });
});

describe("loadProjects", () => {
  it("GET /projects の一覧に、各案件の GET /projects/{id} から途中目標の件数を添える", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendJson(res, 200, {
        items: [project("p1", { title: "Pluvio" }), project("p2", { title: "Other" })],
      } satisfies ProjectList),
    );
    mock.on("GET", "/api/v1/projects/p1", (_req, res) =>
      sendJson(res, 200, {
        project: project("p1", { title: "Pluvio" }),
        milestones: [
          {
            id: "m1",
            project_id: "p1",
            seq: 1,
            title: "…",
            description: "",
            status: "approved",
            created_at: "…",
            updated_at: "…",
          },
          {
            id: "m2",
            project_id: "p1",
            seq: 2,
            title: "…",
            description: "",
            status: "proposed",
            created_at: "…",
            updated_at: "…",
          },
        ],
        tasks: [],
      } satisfies ProjectDetail),
    );
    mock.on("GET", "/api/v1/projects/p2", (_req, res) =>
      sendJson(res, 200, {
        project: project("p2", { title: "Other" }),
        milestones: [],
        tasks: [],
      } satisfies ProjectDetail),
    );

    const result = await loadProjects(client, new Request("http://gui.invalid/projects"));

    expect(result.rows).toEqual([
      { project: project("p1", { title: "Pluvio" }), milestoneCount: 2 },
      { project: project("p2", { title: "Other" }), milestoneCount: 0 },
    ]);
  });

  it("1 件の案件の詳細取得が失敗しても、その案件は件数 0 として残りは表示する", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendJson(res, 200, { items: [project("p1")] } satisfies ProjectList),
    );
    mock.on("GET", "/api/v1/projects/p1", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );

    const result = await loadProjects(client, new Request("http://gui.invalid/projects"));

    expect(result.rows).toEqual([{ project: project("p1"), milestoneCount: 0 }]);
  });

  it("案件が無ければ空", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    const result = await loadProjects(client, new Request("http://gui.invalid/projects"));
    expect(result.rows).toEqual([]);
  });
});

describe("createProject (POST /projects, docs/taskd-api-v1.md §3.46)", () => {
  it("success — 作られた案件は必ず status = proposed", async () => {
    const created = project("p1", { status: "proposed" });
    mock.on("POST", "/api/v1/projects", (_req, res) => sendJson(res, 201, created));

    const result = await createProject(client, { title: "p1", request: "…" });

    expect(result).toEqual({ ok: true, project: created });
  });

  it("422 validation（空白だけの title/request）は例外にせず CreateFailure として返す", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "title must not be blank",
        extra: { errors: [{ field: "title", message: "title must not be blank" }] },
      }),
    );

    const result = await createProject(client, { title: "   ", request: "…" });

    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.status).toBe(422);
      expect(result.error.fields.title).toEqual(["title must not be blank"]);
    }
  });
});
