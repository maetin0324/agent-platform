import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadProjects } from "~/routes/projects";
import { TaskdClient } from "~/taskd/client.server";
import { createProject, patchProjectWorkspace, readProjectCreateInput } from "~/taskd/projects-admin.server";
import type { Clusters, Project, ProjectDetail, ProjectList } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, serveProjectList, startMockTaskd } from "../mock-taskd/server";

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

  // ADR-0039 D1（Phase G13k）: 作業場所（任意）の 3 通り。
  it("workspace_kind = undecided（省略を含む）は workspace キー自体を送らない", () => {
    const form = new FormData();
    form.set("title", "t");
    form.set("request", "r");
    form.set("workspace_kind", "undecided");
    expect(readProjectCreateInput(form)).toEqual({ title: "t", request: "r" });
  });

  it("workspace_kind = local は {kind:'local', path} を足す", () => {
    const form = new FormData();
    form.set("title", "t");
    form.set("request", "r");
    form.set("workspace_kind", "local");
    form.set("workspace_path", "~/workspace/rust/pluvio-poc");
    expect(readProjectCreateInput(form)).toEqual({
      title: "t",
      request: "r",
      workspace: { kind: "local", path: "~/workspace/rust/pluvio-poc" },
    });
  });

  it("workspace_kind = remote は {kind:'remote', cluster, path} を足す", () => {
    const form = new FormData();
    form.set("title", "t");
    form.set("request", "r");
    form.set("workspace_kind", "remote");
    form.set("workspace_cluster", "pegasus");
    form.set("workspace_path", "/work/NBB/rmaeda/workspace/rust/benchfs");
    expect(readProjectCreateInput(form)).toEqual({
      title: "t",
      request: "r",
      workspace: { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/workspace/rust/benchfs" },
    });
  });
});

/** ADR-0039 D1（Phase G13k）: 作成時に送る `workspace` の本文 3 種（3.46 の要求本文どおり）。 */
describe("createProject の workspace 本文（3 種、docs/taskd-api-v1.md §3.46）", () => {
  it("undecided: workspace キーを送らない", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ title: "t", request: "r" });
      sendJson(res, 201, project("p1"));
    });
    const result = await createProject(client, { title: "t", request: "r" });
    expect(result.ok).toBe(true);
  });

  it("local: {kind:'local', path} をそのまま送る", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({
        title: "t",
        request: "r",
        workspace: { kind: "local", path: "~/workspace/rust/pluvio-poc" },
      });
      sendJson(res, 201, project("p1", { workspace: { kind: "local", path: "/home/user/workspace/rust/pluvio-poc" } }));
    });
    const result = await createProject(client, {
      title: "t",
      request: "r",
      workspace: { kind: "local", path: "~/workspace/rust/pluvio-poc" },
    });
    expect(result.ok).toBe(true);
    if (result.ok)
      expect(result.project.workspace).toEqual({ kind: "local", path: "/home/user/workspace/rust/pluvio-poc" });
  });

  it("remote: {kind:'remote', cluster, path} をそのまま送る", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({
        title: "t",
        request: "r",
        workspace: { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/workspace/rust/benchfs" },
      });
      sendJson(
        res,
        201,
        project("p1", {
          workspace: { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/workspace/rust/benchfs" },
        }),
      );
    });
    const result = await createProject(client, {
      title: "t",
      request: "r",
      workspace: { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/workspace/rust/benchfs" },
    });
    expect(result.ok).toBe(true);
  });

  it("422 validation（知らない cluster）は errors[].field = 'workspace.cluster' をそのまま返す", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "unknown cluster: nope",
        extra: { errors: [{ field: "workspace.cluster", message: "unknown cluster: nope" }] },
      }),
    );
    const result = await createProject(client, {
      title: "t",
      request: "r",
      workspace: { kind: "remote", cluster: "nope", path: "/x" },
    });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.fields["workspace.cluster"]).toEqual(["unknown cluster: nope"]);
  });
});

describe("patchProjectWorkspace (PATCH /projects/{id}, ADR-0039 D1)", () => {
  it("local を保存する", async () => {
    const updated = project("p1", { workspace: { kind: "local", path: "/home/user/workspace/rust/pluvio-poc" } });
    mock.on("PATCH", "/api/v1/projects/p1", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ workspace: { kind: "local", path: "~/workspace/rust/pluvio-poc" } });
      sendJson(res, 200, updated);
    });
    const result = await patchProjectWorkspace(client, "p1", { kind: "local", path: "~/workspace/rust/pluvio-poc" });
    expect(result).toEqual({ ok: true, op: "project_workspace", project: updated });
  });

  it("remote を保存する", async () => {
    const workspace = { kind: "remote" as const, cluster: "pegasus", path: "/work/NBB/rmaeda/workspace/rust/benchfs" };
    const updated = project("p1", { workspace });
    mock.on("PATCH", "/api/v1/projects/p1", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ workspace });
      sendJson(res, 200, updated);
    });
    const result = await patchProjectWorkspace(client, "p1", workspace);
    expect(result).toEqual({ ok: true, op: "project_workspace", project: updated });
  });

  it("消去: workspace = null を明示して送る", async () => {
    const updated = project("p1");
    mock.on("PATCH", "/api/v1/projects/p1", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ workspace: null });
      sendJson(res, 200, updated);
    });
    const result = await patchProjectWorkspace(client, "p1", null);
    expect(result).toEqual({ ok: true, op: "project_workspace", project: updated });
  });

  it("422 validation（知らない cluster）は errors[].field = 'workspace.cluster' をそのまま返す", async () => {
    mock.on("PATCH", "/api/v1/projects/p1", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "unknown cluster: nope",
        extra: { errors: [{ field: "workspace.cluster", message: "unknown cluster: nope" }] },
      }),
    );
    const result = await patchProjectWorkspace(client, "p1", { kind: "remote", cluster: "nope", path: "/x" });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.op).toBe("project_workspace");
      expect(result.error.fields["workspace.cluster"]).toEqual(["unknown cluster: nope"]);
    }
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

  // ADR-0039 D1（Phase G13k）: 新規フォームの作業場所（クラスタ）の選択肢。
  it("GET /clusters の一覧を作業場所の選択肢として通す", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/clusters", (_req, res) =>
      sendJson(res, 200, {
        items: [
          {
            id: "pegasus",
            host: "pegasus",
            concurrency: 1,
            delete_on_push: false,
            env_keys: [],
            has_setup: false,
            rsync_excludes: [],
            sync: "rsync",
          },
        ],
      } satisfies Clusters),
    );
    const result = await loadProjects(client, new Request("http://gui.invalid/projects"));
    expect(result.clusters).toHaveLength(1);
    expect(result.clusters[0].id).toBe("pegasus");
  });

  it("GET /clusters が落ちても一覧・作成フォームは出す（空扱い）", async () => {
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/clusters", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );
    const result = await loadProjects(client, new Request("http://gui.invalid/projects"));
    expect(result.clusters).toEqual([]);
  });

  /**
   * アーカイブ（ADR-0044 D6、Phase 55 / G19）。隠す・出すの判断は taskd なので、GUI は
   * **`?archived=1` を付けるかどうか**だけを決める（既定は付けない = taskd が隠す）。
   */
  it("既定では archived を送らない（taskd がアーカイブ済みを隠す）", async () => {
    serveProjectList(mock, { archived: [project("p9", { archived_at: "2026-09-19T12:00:00Z" })] });
    mock.on("GET", "/api/v1/projects/p1", (_req, res) =>
      sendJson(res, 200, { project: project("p1"), milestones: [], tasks: [] } satisfies ProjectDetail),
    );

    const result = await loadProjects(client, new Request("http://gui.invalid/projects"));

    expect(result.showArchived).toBe(false);
    expect(result.rows.map((r) => r.project.id)).toEqual(["p1"]);
    const url = new URL(
      mock.requests.find((r) => r.url.startsWith("/api/v1/projects?"))?.url ?? "/api/v1/projects",
      "http://mock-taskd.invalid",
    );
    expect(url.searchParams.has("archived")).toBe(false);
  });

  it("?archived=1 のときだけ GET /projects に archived=1 を付け、アーカイブ済みも並べる", async () => {
    serveProjectList(mock, { archived: [project("p9", { archived_at: "2026-09-19T12:00:00Z" })] });
    for (const id of ["p1", "p9"]) {
      mock.on("GET", `/api/v1/projects/${id}`, (_req, res) =>
        sendJson(res, 200, { project: project(id), milestones: [], tasks: [] } satisfies ProjectDetail),
      );
    }

    const result = await loadProjects(client, new Request("http://gui.invalid/projects?archived=1"));

    expect(result.showArchived).toBe(true);
    expect(result.rows.map((r) => r.project.id)).toEqual(["p1", "p9"]);
    const url = new URL(
      mock.requests.find((r) => r.url.startsWith("/api/v1/projects?"))?.url ?? "",
      "http://mock-taskd.invalid",
    );
    expect(url.searchParams.get("archived")).toBe("1");
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

  it("401 unauthorized（Phase 27 M-4 で管理系になった。org-admin と同じ code=unauthorized で、案内文は Flash.tsx 共通）", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await createProject(client, { title: "p1", request: "…" });

    expect(result).toEqual({
      ok: false,
      error: expect.objectContaining({ status: 401, code: "unauthorized" }) as unknown,
    });
  });
});
