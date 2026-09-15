import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadDaemon } from "~/routes/daemon";
import { TaskdClient } from "~/taskd/client.server";
import { runReplay } from "~/taskd/route-actions.server";
import type { ConfigView, DaemonView, ReplayReport } from "~/taskd/types";
import { type MockTaskd, sendJson, startMockTaskd } from "../mock-taskd/server";

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

const configView: ConfigView = {
  config_path: "/tmp/taskd.toml",
  db: "/tmp/taskd.sqlite3",
  workspace_root: "/tmp/workspaces",
  tick_ms: 200,
  max_concurrency: 4,
  lease_grace_secs: 30,
  idle_timeout_secs: 60,
  kill_grace_secs: 5,
  review_timeout_secs: 120,
  error_cooldown_secs: 30,
  retry_backoff_base_secs: 0,
  retry_backoff_max_secs: 60,
  max_requeues: 3,
  plan_auto_accept: false,
  reviewer: { adapter: null, tier: "standard" },
  providers: [],
  api: { bind: "127.0.0.1:7710", auth_required: false, allowed_hosts: [] },
};

const daemonViewWithSnapshot: DaemonView = {
  now: "2026-09-15T00:00:03Z",
  snapshot: {
    instance_id: "01MDAEMONINSTANCE0000001",
    pid: 4242,
    hostname: "lab-01",
    started_at: "2026-09-15T00:00:00Z",
    last_tick_at: "2026-09-15T00:00:02Z",
    ticks: 8812,
    tick_ms: 200,
    in_flight: [{ task_id: "T1", run_id: "R1", provider: "fake-local", kind: "worker", since: "2026-09-15T00:00:01Z" }],
    cooldowns: [],
    awaiting_human: ["T2"],
    unroutable: [],
    providers: [{ id: "fake-local", adapter: "fake", tiers: ["standard"], concurrency: 2, model: "fake", in_use: 1 }],
  },
};

const daemonViewNoSnapshot: DaemonView = { now: "2026-09-15T00:00:00Z", snapshot: null };

describe("loadDaemon", () => {
  it("calls GET /daemon and GET /config in parallel and returns {daemon, config} as-is (with snapshot)", async () => {
    mock.on("GET", "/api/v1/daemon", (_req, res) => {
      sendJson(res, 200, daemonViewWithSnapshot);
    });
    mock.on("GET", "/api/v1/config", (_req, res) => {
      sendJson(res, 200, configView);
    });

    const result = await loadDaemon(client, new Request("http://gui.invalid/daemon"));

    expect(result).toEqual({ daemon: daemonViewWithSnapshot, config: configView });
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/daemon")).toBe(true);
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/config")).toBe(true);
  });

  it("returns {daemon, config} as-is when snapshot is null (before the first tick)", async () => {
    mock.on("GET", "/api/v1/daemon", (_req, res) => {
      sendJson(res, 200, daemonViewNoSnapshot);
    });
    mock.on("GET", "/api/v1/config", (_req, res) => {
      sendJson(res, 200, configView);
    });

    const result = await loadDaemon(client, new Request("http://gui.invalid/daemon"));

    expect(result).toEqual({ daemon: daemonViewNoSnapshot, config: configView });
  });
});

describe("runReplay", () => {
  it("POSTs {} to /replay and returns ok:true with the report", async () => {
    const report: ReplayReport = { tasks: 10, mismatches: [] };
    mock.on("POST", "/api/v1/replay", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({});
      sendJson(res, 200, report);
    });

    const outcome = await runReplay(client);

    expect(outcome).toEqual({ ok: true, report });
  });

  it("returns ok:false with status 503 and code replay_in_progress on concurrent replay", async () => {
    mock.on("POST", "/api/v1/replay", (_req, res) => {
      res.writeHead(503, {
        "content-type": "application/problem+json; charset=utf-8",
        "retry-after": "5",
      });
      res.end(
        JSON.stringify({
          type: "urn:taskd:problem:replay_in_progress",
          title: "replay in progress",
          status: 503,
          detail: "another replay is already running",
          code: "replay_in_progress",
          instance: "urn:taskd:request:01MREPLAYINPROGRESS0001",
        }),
      );
    });

    const outcome = await runReplay(client);

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected ok:false");
    expect(outcome.error.status).toBe(503);
    expect(outcome.error.code).toBe("replay_in_progress");
  });

  it("returns ok:false with code unavailable when taskd is not reachable", async () => {
    const closed = await startMockTaskd();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new TaskdClient({ baseUrl, timeoutMs: 1000 });

    const outcome = await runReplay(unreachable);

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected ok:false");
    expect(outcome.error.code).toBe("unavailable");
  });
});
