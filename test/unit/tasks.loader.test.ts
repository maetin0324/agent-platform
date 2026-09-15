import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadTasks } from "~/routes/tasks";
import { TaskdClient } from "~/taskd/client.server";
import { TaskdUnavailable } from "~/taskd/errors";
import type { TaskList } from "~/taskd/types";
import { type MockTaskd, sendJson, startMockTaskd } from "../mock-taskd/server";

let mock: MockTaskd;
let client: TaskdClient;

const sampleTaskList: TaskList = {
  items: [
    {
      id: "01JTASK0000000000000000A1",
      parent_id: null,
      kind: "execute",
      status: "ready",
      title: "do the thing",
      priority: 0,
      tier: "standard",
      adapter: null,
      attempts: 0,
      max_retries: 2,
      depends_on: [],
      created_at: "2026-09-14T00:00:00Z",
      updated_at: "2026-09-15T00:00:00Z",
      lease_expires_at: null,
      backoff_until: null,
      children: 0,
      pending_children: 0,
    },
  ],
  next_cursor: "opaque-cursor-1",
  total: 42,
  counts_by_status: { ready: 10, running: 2, done: 30 },
};

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

function createRequest(url: string): Request {
  return new Request(url);
}

describe("loadTasks", () => {
  it("forwards status/q/order/limit/cursor from the URL to GET /api/v1/tasks, repeating multi-valued status", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => {
      sendJson(res, 200, sampleTaskList);
    });

    await loadTasks(
      client,
      createRequest(
        "http://gui.invalid/tasks?status=ready&status=running&q=thing&order=created_desc&limit=25&cursor=abc123",
      ),
    );

    const req = mock.requests.at(-1);
    expect(req?.method).toBe("GET");
    expect(req?.url).toBe(
      "/api/v1/tasks?status=ready&status=running&q=thing&order=created_desc&limit=25&cursor=abc123",
    );
  });

  it("forwards kind (multi) and parent/root_only when present", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => {
      sendJson(res, 200, sampleTaskList);
    });

    await loadTasks(
      client,
      createRequest(
        "http://gui.invalid/tasks?kind=execute&kind=review&parent=01JPARENT00000000000000A1&root_only=true",
      ),
    );

    const req = mock.requests.at(-1);
    expect(req?.url).toBe("/api/v1/tasks?kind=execute&kind=review&parent=01JPARENT00000000000000A1&root_only=true");
  });

  it("does not forward query params that are absent from the URL", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => {
      sendJson(res, 200, sampleTaskList);
    });

    await loadTasks(client, createRequest("http://gui.invalid/tasks"));

    const req = mock.requests.at(-1);
    expect(req?.url).toBe("/api/v1/tasks");
  });

  it("returns the TaskList response from taskd unmodified (no added/removed fields)", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => {
      sendJson(res, 200, sampleTaskList);
    });

    const result = await loadTasks(client, createRequest("http://gui.invalid/tasks"));

    expect(result).toEqual(sampleTaskList);
  });

  it("throws TaskdUnavailable when taskd is unreachable", async () => {
    const closed = await startMockTaskd();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new TaskdClient({ baseUrl, timeoutMs: 1000 });

    let error: unknown;
    try {
      await loadTasks(unreachable, createRequest("http://gui.invalid/tasks"));
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(TaskdUnavailable);
  });
});
