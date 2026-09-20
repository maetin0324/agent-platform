import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { CelerisUnavailable } from "~/celeris/errors";
import type { ConfigView, TaskList } from "~/celeris/types";
import { loadTasks, loadTasksPage } from "~/routes/tasks";
import { type MockCeleris, sendJson, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

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
      conversation: false,
      actions: ["cancel"],
      // ADR-0044 D3（Phase 53）で `TaskSummary` に増えた必須項目。
      labels: [],
      category: "other",
      priority_label: "P3",
    },
  ],
  next_cursor: "opaque-cursor-1",
  total: 42,
  counts_by_status: { ready: 10, running: 2, done: 30 },
};

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
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

  it("forwards genre (multi, ADR-0027 D1) alongside status/kind", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => {
      sendJson(res, 200, sampleTaskList);
    });

    await loadTasks(client, createRequest("http://gui.invalid/tasks?genre=coding&genre=literature"));

    const req = mock.requests.at(-1);
    expect(req?.url).toBe("/api/v1/tasks?genre=coding&genre=literature");
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

  // ADR-0044 D6（Phase 55 / G19）: アーカイブされた案件のタスクは celeris が既定で隠す。
  // GUI は `?archived=1` を素通しするだけ（絞り込みを GUI で再実装しない）。
  it("forwards archived=1 when present (ADR-0044 D6) and omits it otherwise", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => {
      sendJson(res, 200, sampleTaskList);
    });

    await loadTasks(client, createRequest("http://gui.invalid/tasks?archived=1"));
    expect(mock.requests.at(-1)?.url).toBe("/api/v1/tasks?archived=1");

    await loadTasks(client, createRequest("http://gui.invalid/tasks"));
    expect(mock.requests.at(-1)?.url).toBe("/api/v1/tasks");
  });

  it("returns the TaskList response from celeris unmodified (no added/removed fields)", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => {
      sendJson(res, 200, sampleTaskList);
    });

    const result = await loadTasks(client, createRequest("http://gui.invalid/tasks"));

    expect(result).toEqual(sampleTaskList);
  });

  it("throws CelerisUnavailable when celeris is unreachable", async () => {
    const closed = await startMockCeleris();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new CelerisClient({ baseUrl, timeoutMs: 1000 });

    let error: unknown;
    try {
      await loadTasks(unreachable, createRequest("http://gui.invalid/tasks"));
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisUnavailable);
  });
});

describe("loadTasksPage", () => {
  const config = {
    config_path: "/tmp/config.toml",
    db: "/tmp/celeris.sqlite3",
    workspace_root: "/tmp/ws",
    tick_ms: 200,
    max_concurrency: 2,
    lease_grace_secs: 30,
    idle_timeout_secs: 60,
    kill_grace_secs: 5,
    review_timeout_secs: 60,
    error_cooldown_secs: 0,
    retry_backoff_base_secs: 0,
    retry_backoff_max_secs: 0,
    max_requeues: 3,
    plan_auto_accept: false,
    reviewer: { adapter: "fake", tier: "cheap" },
    providers: [],
    api: { bind: "127.0.0.1:7710", auth_required: false, allowed_hosts: [] },
    genres: [
      {
        id: "coding",
        description: "コードを書く",
        capabilities: ["実装", "テスト"],
        input_artifacts: ["spec.md"],
        output_artifacts: ["diff.patch"],
        default_role: "implementer",
        roles: ["lead", "implementer"],
      },
    ],
  } as unknown as ConfigView;

  it("calls GET /tasks and GET /config in parallel and returns {tasks, config} unmodified", async () => {
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, sampleTaskList));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, config));

    const result = await loadTasksPage(client, createRequest("http://gui.invalid/tasks"));

    // 案件・担当の索引（監査 M2）は `GET /projects` / `GET /org` が無いここでは空になるだけ（画面は出る）。
    expect(result).toEqual({ tasks: sampleTaskList, config, placements: {}, assigneeNames: {} });
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/config")).toBe(true);
  });

  it("falls back to an empty genres[] when celeris has no [[genres]] configured", async () => {
    const { genres: _genres, ...configWithoutGenres } = config;
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, sampleTaskList));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, configWithoutGenres));

    const result = await loadTasksPage(client, createRequest("http://gui.invalid/tasks"));

    expect(result.config.genres).toBeUndefined();
  });
});
