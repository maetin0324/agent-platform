import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { relayEvents } from "~/routes/events";
import { type MockCeleris, sendProblem, sendSseHello, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("relayEvents", () => {
  it("relays the upstream SSE response as-is (headers + body)", async () => {
    mock.on("GET", "/api/v1/stream", (_req, res) => {
      sendSseHello(res, { cursor: 1, now: "2026-09-15T00:00:00Z", daemon: null });
    });

    const res = await relayEvents(client, new Request("http://gui.invalid/events"));

    expect(res.headers.get("content-type")).toContain("text/event-stream");
    expect(res.body).not.toBeNull();
    const reader = res.body?.getReader();
    if (!reader) throw new Error("expected a readable body");
    const first = await reader.read();
    expect(first.done).toBe(false);
    expect(new TextDecoder().decode(first.value)).toContain("event: hello");
  });

  it("forwards the Last-Event-ID header to client.stream", async () => {
    mock.on("GET", "/api/v1/stream", (_req, res) => {
      sendSseHello(res, { cursor: 42, now: "2026-09-15T00:00:00Z", daemon: null });
    });

    await relayEvents(client, new Request("http://gui.invalid/events", { headers: { "Last-Event-ID": "42" } }));

    const req = mock.requests.at(-1);
    expect(req?.headers["last-event-id"]).toBe("42");
  });

  it("forwards ?task_id= as the task_id query parameter", async () => {
    mock.on("GET", "/api/v1/stream", (_req, res) => {
      sendSseHello(res, { cursor: 1, now: "2026-09-15T00:00:00Z", daemon: null });
    });

    await relayEvents(client, new Request("http://gui.invalid/events?task_id=XXXX"));

    const req = mock.requests.at(-1);
    expect(req?.url).toContain("task_id=XXXX");
  });

  it("relays celeris's non-2xx status (e.g. 503 too_many_streams) instead of throwing (docs/DESIGN.md §6.4)", async () => {
    mock.on("GET", "/api/v1/stream", (_req, res) => {
      sendProblem(res, { status: 503, code: "too_many_streams", detail: "too many SSE connections" });
    });

    const res = await relayEvents(client, new Request("http://gui.invalid/events"));

    expect(res.status).toBe(503);
  });

  it("returns 503 (not an unhandled throw) when celeris is unreachable", async () => {
    const closed = await startMockCeleris();
    const unreachableClient = new CelerisClient({ baseUrl: closed.baseUrl, timeoutMs: 500 });
    await closed.close();

    const res = await relayEvents(unreachableClient, new Request("http://gui.invalid/events"));

    expect(res.status).toBe(503);
  });
});
