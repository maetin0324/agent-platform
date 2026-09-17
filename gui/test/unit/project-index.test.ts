import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { buildTaskPlacements, milestoneTitle } from "~/lib/project-index";
import { loadTasksPage } from "~/routes/tasks";
import { TaskdClient } from "~/taskd/client.server";
import type { ConfigView, Milestone, ProjectDetail, ProjectList, TaskList } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * 裏方のタスクから案件・途中目標へ戻る索引（Phase G13f-1、監査 M2）。
 * `TaskSummary` に `project_id` が無いので、案件の詳細から「どのタスクがどの案件か」を引く。
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

const milestone = (id: string, seq: number, title: string): Milestone => ({
  id,
  seq,
  title,
  project_id: "01JPROJECT",
  status: "in_progress",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
});

const detail = (): ProjectDetail => ({
  project: {
    id: "01JPROJECT",
    title: "Pluvio の新テーマ",
    request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証",
    status: "active",
    created_at: "2026-09-17T00:00:00Z",
    updated_at: "2026-09-17T00:00:00Z",
  },
  milestones: [milestone("01JMS1", 1, "関連研究の洗い出し")],
  tasks: [
    {
      id: "01JTASK1",
      title: "関連研究を調べる",
      status: "running",
      parent_id: null,
      depends_on: [],
      assignee: "research-survey",
      milestone_id: "01JMS1",
      conversation: false,
    },
    {
      id: "01JTASK2",
      title: "対話: 秘書",
      status: "done",
      parent_id: null,
      depends_on: [],
      assignee: "secretary",
      milestone_id: null,
      conversation: true,
    },
  ],
});

describe("milestoneTitle", () => {
  it("通し番号と題名を並べる", () => {
    expect(milestoneTitle([milestone("01JMS1", 1, "関連研究の洗い出し")], "01JMS1")).toBe("#1 関連研究の洗い出し");
  });

  it("途中目標が無い・見つからないとき", () => {
    expect(milestoneTitle([], null)).toBeNull();
    expect(milestoneTitle([], "01JMISSING")).toBe("01JMISSING");
  });
});

describe("buildTaskPlacements", () => {
  it("タスク id から案件と途中目標を引けるようにする", () => {
    const placements = buildTaskPlacements([detail()]);
    expect(placements["01JTASK1"]).toEqual({
      projectId: "01JPROJECT",
      projectTitle: "Pluvio の新テーマ",
      milestoneId: "01JMS1",
      milestoneTitle: "#1 関連研究の洗い出し",
    });
    // 対話用タスクも索引には入れる（裏方の一覧からも案件へ戻れるように）。
    expect(placements["01JTASK2"].milestoneTitle).toBeNull();
  });
});

describe("loadTasksPage の索引（`/tasks` の案件・担当の列）", () => {
  const emptyTasks: TaskList = { items: [], next_cursor: null, total: 0, counts_by_status: {} };
  const config = { genres: [] } as unknown as ConfigView;
  const request = () => new Request("http://gui.invalid/tasks");

  it("GET /projects と各案件の GET /projects/{id} を束ねて索引にする", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTasks));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, config));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendJson(res, 200, { items: [detail().project] } satisfies ProjectList),
    );
    mock.on("GET", "/api/v1/projects/01JPROJECT", (_req, res) => sendJson(res, 200, detail()));

    const result = await loadTasksPage(client, request());
    expect(result.placements["01JTASK1"].projectTitle).toBe("Pluvio の新テーマ");
    expect(result.placements["01JTASK1"].milestoneTitle).toBe("#1 関連研究の洗い出し");
  });

  it("案件が読めなくても一覧は出す（索引が空になるだけ）", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTasks));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, config));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );

    const result = await loadTasksPage(client, request());
    expect(result.placements).toEqual({});
    expect(result.tasks).toEqual(emptyTasks);
  });

  it("担当の名前は GET /org から引く（落ちても一覧は出す）", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTasks));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, config));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) =>
      sendJson(res, 200, {
        items: [
          {
            id: "research-survey",
            parent_id: "research",
            name: "関連研究調査課",
            kind: "section",
            position: 0,
            created_at: "2026-09-17T00:00:00Z",
            updated_at: "2026-09-17T00:00:00Z",
          },
        ],
      }),
    );

    const result = await loadTasksPage(client, request());
    expect(result.assigneeNames).toEqual({ "research-survey": "関連研究調査課" });
  });
});
