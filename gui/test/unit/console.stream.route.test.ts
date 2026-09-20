import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { relayConsoleStream } from "~/routes/console.stream";
import { type MockCeleris, sendProblem, sendSseHello, startMockCeleris } from "../mock-celeris/server";

/**
 * `/console/stream` の中継（ADR-0048 D1、`~/routes/console.stream.ts`、GUI Phase G22）。
 * `test/unit/events.route.test.ts`（`~/routes/events.ts`）と同じ作り。
 */

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("relayConsoleStream", () => {
  it("celeris の SSE 応答をそのまま中継する（ヘッダ・本文）", async () => {
    mock.on("GET", "/api/v1/console/stream", (_req, res) => {
      sendSseHello(res, { cursor: "c1", scope: "all", now: "2026-09-20T00:00:00Z" });
    });

    const res = await relayConsoleStream(client, new Request("http://gui.invalid/console/stream"));

    expect(res.headers.get("content-type")).toContain("text/event-stream");
    const reader = res.body?.getReader();
    if (!reader) throw new Error("expected a readable body");
    const first = await reader.read();
    expect(first.done).toBe(false);
    expect(new TextDecoder().decode(first.value)).toContain("event: hello");
  });

  it("?scope= と ?since= をそのままクエリに転送する", async () => {
    mock.on("GET", "/api/v1/console/stream", (_req, res) => {
      sendSseHello(res, { cursor: "c2", scope: "node:coding-poc", now: "t" });
    });

    await relayConsoleStream(client, new Request("http://gui.invalid/console/stream?scope=node%3Acoding-poc&since=c1"));

    const req = mock.requests.at(-1);
    expect(req?.url).toContain("scope=node%3Acoding-poc");
    expect(req?.url).toContain("since=c1");
  });

  it("celeris の非 2xx（503 too_many_streams）をそのまま返す", async () => {
    mock.on("GET", "/api/v1/console/stream", (_req, res) => {
      sendProblem(res, { status: 503, code: "too_many_streams", detail: "too many SSE connections" });
    });

    const res = await relayConsoleStream(client, new Request("http://gui.invalid/console/stream"));
    expect(res.status).toBe(503);
  });

  it("celeris に届かないときは 503（投げっぱなしにしない）", async () => {
    const closed = await startMockCeleris();
    const unreachableClient = new CelerisClient({ baseUrl: closed.baseUrl, timeoutMs: 500 });
    await closed.close();

    const res = await relayConsoleStream(unreachableClient, new Request("http://gui.invalid/console/stream"));
    expect(res.status).toBe(503);
  });
});
