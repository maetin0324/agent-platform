import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import {
  createMilestone,
  decideMilestone,
  patchMilestoneStatus,
  patchProjectStatus,
  startProjectPlan,
} from "~/celeris/projects-admin.server";
import type {
  ArtifactList,
  Milestone,
  MilestoneDecided,
  MilestoneView,
  OrgList,
  Project,
  ProjectDetail,
  Report,
  ReportList,
  TaskDetail,
} from "~/celeris/types";
import { loadProjectDetail } from "~/routes/projects.$id";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

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

  it("ADR-0038（Phase 41 / G13j）: milestones[] の review / proposal をそのまま通す", async () => {
    const milestone: MilestoneView = {
      id: "m1",
      project_id: "p1",
      seq: 1,
      title: "隣接領域の動向調査",
      description: "",
      status: "in_progress",
      created_at: "…",
      updated_at: "…",
      review: { message_id: "msg1", text: "候補を 3 本に絞りました。次は…", at: "2026-09-18T00:00:00Z" },
      proposal: {
        id: "m2",
        project_id: "p1",
        seq: 2,
        title: "候補の比較実験",
        description: "3 本を比較する",
        status: "proposed",
        created_at: "…",
        updated_at: "…",
      },
    };
    const detail: ProjectDetail = { project: project(), milestones: [milestone], tasks: [] };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.detail.milestones[0].review).toEqual(milestone.review);
    expect(result.detail.milestones[0].proposal).toEqual(milestone.proposal);
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
    const url = new URL(req?.url ?? "", "http://mock-celeris.invalid");
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
      // ADR-0044 D3（Phase 53）で `TaskDetail` に増えた必須項目。
      priority_label: "P3",
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

  // ADR-0039 D1（Phase G13k）: 案件の作業場所は loader を素通りする（GUI 側で新しい判断はしない）。
  it("project.workspace をそのまま通し、GET /clusters を編集フォームの選択肢として添える", async () => {
    const detail: ProjectDetail = {
      project: project({ workspace: { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/benchfs" } }),
      milestones: [],
      tasks: [],
    };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/reports", (_req, res) => sendJson(res, 200, { items: [] } satisfies ReportList));
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
      }),
    );

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.detail.project.workspace).toEqual({
      kind: "remote",
      cluster: "pegasus",
      path: "/work/NBB/rmaeda/benchfs",
    });
    expect(result.clusters).toHaveLength(1);
    expect(result.clusters[0].id).toBe("pegasus");
  });

  it("GET /clusters が落ちても案件の詳細は返す（選択肢は空扱い）", async () => {
    const detail: ProjectDetail = { project: project(), milestones: [], tasks: [] };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/reports", (_req, res) => sendJson(res, 200, { items: [] } satisfies ReportList));
    mock.on("GET", "/api/v1/clusters", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));
    expect(result.clusters).toEqual([]);
  });

  /**
   * 中止・一時停止・アーカイブ（ADR-0044 D6、Phase 55 / G19）。loader は素通りさせるだけ
   * （`archived_at` / `paused_from` / `paused` / `cancelled` を GUI 側で計算し直さない）。
   * `GET /projects/{id}`（個別）はアーカイブ済みでもそのまま見えるので、隠す判断もしない。
   */
  it("archived_at / paused_from と paused / cancelled の途中目標をそのまま通す", async () => {
    const detail: ProjectDetail = {
      project: project({ status: "paused", paused_from: "active", archived_at: "2026-09-19T12:00:00Z" }),
      milestones: [
        {
          id: "m1",
          project_id: "p1",
          seq: 1,
          title: "調査",
          description: "",
          status: "paused",
          paused_from: "in_progress",
          created_at: "…",
          updated_at: "…",
        },
        {
          id: "m2",
          project_id: "p1",
          seq: 2,
          title: "統合",
          description: "",
          status: "cancelled",
          created_at: "…",
          updated_at: "…",
        },
      ],
      tasks: [],
    };
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.detail.project.status).toBe("paused");
    expect(result.detail.project.paused_from).toBe("active");
    expect(result.detail.project.archived_at).toBe("2026-09-19T12:00:00Z");
    expect(result.detail.milestones.map((m) => m.status)).toEqual(["paused", "cancelled"]);
    expect(result.detail.milestones[0].paused_from).toBe("in_progress");
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

/**
 * 「この方針で進める」（`POST /projects/{id}/plan`。**管理系**、202 `{task_id}`。docs/celeris-api-v1.md §3.61、
 * 監査 H3）。GUI 側では判断しない: 選んだ途中目標と一言をそのまま送り、202 の `task_id` を画面へ渡すだけ。
 */
describe("startProjectPlan (POST /projects/{id}/plan)", () => {
  it("途中目標も一言も無ければ空の本文を送る（どちらも省略可）", async () => {
    mock.on("POST", "/api/v1/projects/p1/plan", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({});
      sendJson(res, 202, { task_id: "01JPLAN" });
    });

    const result = await startProjectPlan(client, "p1", new FormData());
    expect(result).toEqual({ ok: true, op: "project_plan", accepted: { task_id: "01JPLAN" } });
  });

  it("選んだ途中目標と一言を送る（空文字は送らない）", async () => {
    mock.on("POST", "/api/v1/projects/p1/plan", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ milestone_id: "m1", note: "急がなくてよい" });
      sendJson(res, 202, { task_id: "01JPLAN" });
    });

    const form = new FormData();
    form.set("milestone_id", "m1");
    form.set("note", "急がなくてよい");
    const result = await startProjectPlan(client, "p1", form);
    expect(result.ok).toBe(true);
  });

  it("422 validation（途中目標が別の案件のもの）は ActionError にして返す", async () => {
    mock.on("POST", "/api/v1/projects/p1/plan", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "milestone m9 does not belong to project p1",
      }),
    );
    const form = new FormData();
    form.set("milestone_id", "m9");
    const result = await startProjectPlan(client, "p1", form);
    expect(result).toMatchObject({ ok: false, op: "project_plan", error: { status: 422, code: "validation" } });
  });

  it("401 unauthorized（管理系）もそのまま返す", async () => {
    mock.on("POST", "/api/v1/projects/p1/plan", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const result = await startProjectPlan(client, "p1", new FormData());
    expect(result).toMatchObject({ ok: false, op: "project_plan", error: { status: 401, code: "unauthorized" } });
  });
});

/**
 * 途中目標の判定（`POST /milestones/{id}/decide`。**管理系**、202。ADR-0038 D2、docs/celeris-api-v1.md §3.63、
 * Phase 41 / G13j）。GUI 側は 3 値を解釈しない: フォームの `decision`/`note` をそのまま送るだけ。
 */
describe("decideMilestone (POST /milestones/{id}/decide)", () => {
  const decided = (over: Partial<MilestoneDecided> = {}): MilestoneDecided => ({
    decision: "ok",
    milestone: {
      id: "m1",
      project_id: "p1",
      seq: 1,
      title: "隣接領域の動向調査",
      description: "",
      status: "reached",
      created_at: "…",
      updated_at: "…",
    },
    ...over,
  });

  it("ok: note 無しでも送れる（decision だけの本文）", async () => {
    mock.on("POST", "/api/v1/milestones/m1/decide", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ decision: "ok" });
      sendJson(res, 202, decided({ next_milestone: { ...decided().milestone, id: "m2", status: "approved" } }));
    });

    const form = new FormData();
    form.set("decision", "ok");
    const result = await decideMilestone(client, "m1", form);

    expect(result.ok).toBe(true);
    if (result.ok && result.op === "milestone_decide") {
      expect(result.decided.decision).toBe("ok");
      expect(result.decided.next_milestone?.id).toBe("m2");
    } else {
      throw new Error(`unexpected result: ${JSON.stringify(result)}`);
    }
  });

  it("discuss: decision と note を送る", async () => {
    mock.on("POST", "/api/v1/milestones/m1/decide", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ decision: "discuss", note: "候補をもう 1 本足してほしい" });
      sendJson(res, 202, decided({ decision: "discuss", message_id: "msg2", conversation_task_id: "t9" }));
    });

    const form = new FormData();
    form.set("decision", "discuss");
    form.set("note", "候補をもう 1 本足してほしい");
    const result = await decideMilestone(client, "m1", form);

    expect(result).toMatchObject({
      ok: true,
      op: "milestone_decide",
      decided: { decision: "discuss", message_id: "msg2", conversation_task_id: "t9" },
    });
  });

  it("ng: decision と note（理由）を送る", async () => {
    mock.on("POST", "/api/v1/milestones/m1/decide", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ decision: "ng", note: "この切り方は広すぎる" });
      sendJson(res, 202, decided({ decision: "ng", message_id: "msg3" }));
    });

    const form = new FormData();
    form.set("decision", "ng");
    form.set("note", "この切り方は広すぎる");
    const result = await decideMilestone(client, "m1", form);

    expect(result).toMatchObject({ ok: true, op: "milestone_decide", decided: { decision: "ng" } });
  });

  it("422 validation（discuss で note が空）をそのまま ActionError にする", async () => {
    mock.on("POST", "/api/v1/milestones/m1/decide", (_req, res) =>
      sendProblem(res, { status: 422, code: "validation", detail: "note is required for discuss" }),
    );
    const form = new FormData();
    form.set("decision", "discuss");
    const result = await decideMilestone(client, "m1", form);
    expect(result).toMatchObject({ ok: false, op: "milestone_decide", error: { status: 422, code: "validation" } });
  });

  it("401 unauthorized（管理系）をそのまま返す", async () => {
    mock.on("POST", "/api/v1/milestones/m1/decide", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const result = await decideMilestone(client, "m1", new FormData());
    expect(result).toMatchObject({ ok: false, op: "milestone_decide", error: { status: 401, code: "unauthorized" } });
  });

  it("404 milestone_not_found をそのまま返す", async () => {
    mock.on("POST", "/api/v1/milestones/missing/decide", (_req, res) =>
      sendProblem(res, { status: 404, code: "milestone_not_found", detail: "no such milestone" }),
    );
    const result = await decideMilestone(client, "missing", new FormData());
    expect(result).toMatchObject({
      ok: false,
      op: "milestone_decide",
      error: { status: 404, code: "milestone_not_found" },
    });
  });

  it("409 milestone_reached（既に達成済み）をそのまま返す", async () => {
    mock.on("POST", "/api/v1/milestones/m1/decide", (_req, res) =>
      sendProblem(res, { status: 409, code: "milestone_reached", detail: "already reached" }),
    );
    const form = new FormData();
    form.set("decision", "ok");
    const result = await decideMilestone(client, "m1", form);
    expect(result).toMatchObject({
      ok: false,
      op: "milestone_decide",
      error: { status: 409, code: "milestone_reached" },
    });
  });
});
