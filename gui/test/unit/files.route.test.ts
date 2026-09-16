import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { relayArtifactFile } from "~/routes/files.artifacts";
import { relayRunFile } from "~/routes/files.runs";
import { TaskdClient } from "~/taskd/client.server";
import { type MockTaskd, sendProblem, startMockTaskd } from "../mock-taskd/server";

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("relayRunFile", () => {
  it("relays content-type and body from taskd's run file response", async () => {
    mock.on("GET", "/api/v1/tasks/T1/runs/R1/stdout", (_req, res) => {
      res.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
      res.end("line1\nline2\n");
    });

    const res = await relayRunFile(
      client,
      "T1",
      "R1",
      "stdout",
      new Request("http://gui.invalid/files/tasks/T1/runs/R1/stdout"),
    );

    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("text/plain; charset=utf-8");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    expect(await res.text()).toBe("line1\nline2\n");
  });

  it("forwards the Range header to taskd", async () => {
    mock.on("GET", "/api/v1/tasks/T1/runs/R1/stdout", (_req, res) => {
      res.writeHead(200, { "content-type": "text/plain" });
      res.end("abcde");
    });

    await relayRunFile(
      client,
      "T1",
      "R1",
      "stdout",
      new Request("http://gui.invalid/files/tasks/T1/runs/R1/stdout", { headers: { Range: "bytes=0-4" } }),
    );

    const req = mock.requests.at(-1);
    expect(req?.headers.range).toBe("bytes=0-4");
  });

  it("forwards ?offset= and ?download= as query parameters to taskd", async () => {
    mock.on("GET", "/api/v1/tasks/T1/runs/R1/stdout", (_req, res) => {
      res.writeHead(200, { "content-type": "text/plain" });
      res.end("abcde");
    });

    await relayRunFile(
      client,
      "T1",
      "R1",
      "stdout",
      new Request("http://gui.invalid/files/tasks/T1/runs/R1/stdout?offset=10&download=1"),
    );

    const req = mock.requests.at(-1);
    expect(req?.url).toContain("offset=10");
    expect(req?.url).toContain("download=1");
  });

  it("returns taskd's 403 path_forbidden without throwing", async () => {
    mock.on("GET", "/api/v1/tasks/T1/runs/R1/stdout", (_req, res) => {
      sendProblem(res, { status: 403, code: "path_forbidden", detail: "path escapes run directory" });
    });

    const res = await relayRunFile(
      client,
      "T1",
      "R1",
      "stdout",
      new Request("http://gui.invalid/files/tasks/T1/runs/R1/stdout"),
    );

    expect(res.status).toBe(403);
  });

  it("returns 503 (not an unhandled throw) when taskd is unreachable", async () => {
    const closed = await startMockTaskd();
    const unreachableClient = new TaskdClient({ baseUrl: closed.baseUrl, timeoutMs: 500 });
    await closed.close();

    const res = await relayRunFile(
      unreachableClient,
      "T1",
      "R1",
      "stdout",
      new Request("http://gui.invalid/files/tasks/T1/runs/R1/stdout"),
    );

    expect(res.status).toBe(503);
  });
});

describe("relayArtifactFile", () => {
  it("relays content-type and body from taskd's artifact response", async () => {
    mock.on("GET", "/api/v1/tasks/T1/artifacts/0", (_req, res) => {
      res.writeHead(200, { "content-type": "application/json" });
      res.end('{"a":1}');
    });

    const res = await relayArtifactFile(
      client,
      "T1",
      "0",
      new Request("http://gui.invalid/files/tasks/T1/artifacts/0"),
    );

    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("application/json");
    expect(await res.text()).toBe('{"a":1}');
  });

  it("relays the X-Taskd-Sha256 header", async () => {
    mock.on("GET", "/api/v1/tasks/T1/artifacts/0", (_req, res) => {
      res.writeHead(200, {
        "content-type": "text/markdown",
        "x-taskd-sha256": "deadbeef",
        "x-taskd-sha256-current": "deadbeef",
      });
      res.end("# hello");
    });

    const res = await relayArtifactFile(
      client,
      "T1",
      "0",
      new Request("http://gui.invalid/files/tasks/T1/artifacts/0"),
    );

    expect(res.headers.get("x-taskd-sha256")).toBe("deadbeef");
    expect(res.headers.get("x-taskd-sha256-current")).toBe("deadbeef");
  });

  it("returns 503 (not an unhandled throw) when taskd is unreachable", async () => {
    const closed = await startMockTaskd();
    const unreachableClient = new TaskdClient({ baseUrl: closed.baseUrl, timeoutMs: 500 });
    await closed.close();

    const res = await relayArtifactFile(
      unreachableClient,
      "T1",
      "0",
      new Request("http://gui.invalid/files/tasks/T1/artifacts/0"),
    );

    expect(res.status).toBe(503);
  });
});
