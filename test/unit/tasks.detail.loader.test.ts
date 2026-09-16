import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadTaskDetail } from "~/routes/tasks.$id";
import { TaskdClient } from "~/taskd/client.server";
import { TaskdError } from "~/taskd/errors";
import type { ArtifactList, EventsPage, TaskDetail } from "~/taskd/types";
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

const taskDetail: TaskDetail = {
  task: {
    id: "T1",
    kind: "execute",
    status: "running",
    title: "do the thing",
    objective: "do it",
    priority: 5,
    attempts: 1,
    created_at: "2026-09-15T00:00:00Z",
    updated_at: "2026-09-15T00:00:01Z",
    acceptance: [],
    depends_on: [],
    inputs: [],
    worker_hint: { tier: "standard" },
    budget: { max_retries: 3, max_turns: 10, max_wall_secs: 600 },
    workspace: { kind: "local", path: "." },
  },
  workspace_dir: "/tmp/ws/T1",
  timers: {
    now: "2026-09-15T00:00:02Z",
    consecutive_requeues: 0,
    consecutive_reviewer_requeues: 0,
    max_requeues: 3,
  },
  criteria: [],
  runs: [],
  prior_review: [],
  answers: [],
  latest_question: null,
  approvals: [],
  dependencies: [],
  dependents: [],
  children: [],
  actions: ["cancel"],
  worker_run_hint: null,
  delegated: [],
};

const eventsPage: EventsPage = {
  has_more: false,
  items: [
    {
      id: 1,
      seq: 0,
      task_id: "T1",
      ts: "2026-09-15T00:00:00Z",
      event: { type: "created", task: taskDetail.task },
    },
  ],
};

const artifactList: ArtifactList = { items: [] };

describe("loadTaskDetail", () => {
  it("calls GET /tasks/{id} and GET /tasks/{id}/events and GET /tasks/{id}/artifacts, returns them as-is", async () => {
    mock.on("GET", "/api/v1/tasks/T1", (_req, res) => {
      sendJson(res, 200, taskDetail);
    });
    mock.on("GET", "/api/v1/tasks/T1/events", (_req, res) => {
      sendJson(res, 200, eventsPage);
    });
    mock.on("GET", "/api/v1/tasks/T1/artifacts", (_req, res) => {
      sendJson(res, 200, artifactList);
    });

    const result = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1"));

    expect(result).toEqual({ detail: taskDetail, events: eventsPage, artifacts: artifactList });
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/tasks/T1")).toBe(true);
    expect(mock.requests.some((r) => r.method === "GET" && r.url.startsWith("/api/v1/tasks/T1/events"))).toBe(true);
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/tasks/T1/artifacts")).toBe(true);
  });

  it("forwards the `types` search param to GET /tasks/{id}/events", async () => {
    mock.on("GET", "/api/v1/tasks/T1", (_req, res) => {
      sendJson(res, 200, taskDetail);
    });
    mock.on("GET", "/api/v1/tasks/T1/events", (_req, res) => {
      sendJson(res, 200, eventsPage);
    });
    mock.on("GET", "/api/v1/tasks/T1/artifacts", (_req, res) => {
      sendJson(res, 200, artifactList);
    });

    await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1?types=transitioned"));

    const eventsReq = mock.requests.find((r) => r.url.startsWith("/api/v1/tasks/T1/events"));
    expect(eventsReq?.url).toBe("/api/v1/tasks/T1/events?types=transitioned");
  });

  it("throws TaskdError with status 404 and code task_not_found when the task does not exist", async () => {
    mock.on("GET", "/api/v1/tasks/MISSING", (_req, res) => {
      sendProblem(res, { status: 404, code: "task_not_found", detail: "task MISSING not found" });
    });
    mock.on("GET", "/api/v1/tasks/MISSING/events", (_req, res) => {
      sendProblem(res, { status: 404, code: "task_not_found", detail: "task MISSING not found" });
    });
    mock.on("GET", "/api/v1/tasks/MISSING/artifacts", (_req, res) => {
      sendProblem(res, { status: 404, code: "task_not_found", detail: "task MISSING not found" });
    });

    let error: unknown;
    try {
      await loadTaskDetail(client, "MISSING", new Request("http://gui.invalid/tasks/MISSING"));
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(TaskdError);
    const taskdError = error as TaskdError;
    expect(taskdError.status).toBe(404);
    expect(taskdError.code).toBe("task_not_found");
  });
});
