import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { CelerisError, CelerisUnavailable } from "~/celeris/errors";
import { loadHealth } from "~/celeris/health.server";
import type { Health } from "~/celeris/types";
import { defaultHealth } from "../mock-celeris/fixtures";
import { type MockCeleris, sendJson, sendProblem, sendSseHello, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("CelerisClient.get", () => {
  it("returns Health from GET /health with default headers (no auth, no origin)", async () => {
    const health = await client.get<Health>("/health");
    expect(health).toEqual(defaultHealth);
    const req = mock.requests.at(-1);
    expect(req?.headers.accept).toBe("application/json");
    expect(req?.headers.authorization).toBeUndefined();
    expect(req?.headers.origin).toBeUndefined();
  });

  it("sends Authorization: Bearer <token> when a token is configured", async () => {
    const authed = new CelerisClient({ baseUrl: mock.baseUrl, token: "t0k" });
    await authed.health();
    const req = mock.requests.at(-1);
    expect(req?.headers.authorization).toBe("Bearer t0k");
  });
});

describe("CelerisClient problem+json handling (Phase G0 受け入れ条件 6)", () => {
  it("409 conflict -> CelerisError{status:409, code:'conflict', extra.expected/actual}", async () => {
    mock.on("POST", "/api/v1/tasks/T1/approve", (_req, res) => {
      sendProblem(res, {
        status: 409,
        code: "conflict",
        detail: "task T1 expected status ready but was done",
        extra: { expected: "ready", actual: "done" },
      });
    });

    let error: unknown;
    try {
      await client.post("/tasks/T1/approve", { note: "ok", expected_status: "ready" });
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisError);
    const celerisError = error as CelerisError;
    expect(celerisError.status).toBe(409);
    expect(celerisError.code).toBe("conflict");
    expect(celerisError.detail).toBe("task T1 expected status ready but was done");
    expect(celerisError.extra.expected).toBe("ready");
    expect(celerisError.extra.actual).toBe("done");
    expect(celerisError.instance).toMatch(/^urn:celeris:request:/);
  });

  it("422 validation -> extra.errors[] is preserved", async () => {
    mock.on("POST", "/api/v1/tasks", (_req, res) => {
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail:
          "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)",
        extra: {
          errors: [
            {
              field: "acceptance",
              message:
                "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)",
            },
          ],
        },
      });
    });

    let error: unknown;
    try {
      await client.post("/tasks", { title: "x" });
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisError);
    const celerisError = error as CelerisError;
    expect(celerisError.status).toBe(422);
    expect(celerisError.code).toBe("validation");
    expect(Array.isArray(celerisError.extra.errors)).toBe(true);
    expect(celerisError.extra.errors).toEqual([
      { field: "acceptance", message: expect.any(String) as unknown as string },
    ]);
  });

  it("non-problem 500 (text/plain) -> CelerisError{code:'unknown', status:500}", async () => {
    mock.on("GET", "/api/v1/whatever", (_req, res) => {
      res.writeHead(500, { "content-type": "text/plain; charset=utf-8" });
      res.end("boom");
    });

    let error: unknown;
    try {
      await client.get("/whatever");
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisError);
    const celerisError = error as CelerisError;
    expect(celerisError.status).toBe(500);
    expect(celerisError.code).toBe("unknown");
  });

  it("connection refused -> CelerisUnavailable (受け入れ条件 6)", async () => {
    const closed = await startMockCeleris();
    const baseUrl = closed.baseUrl;
    await closed.close();

    const unreachable = new CelerisClient({ baseUrl, timeoutMs: 1000 });
    let error: unknown;
    try {
      await unreachable.health();
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisUnavailable);
    expect(error).toBeInstanceOf(Error);
    expect((error as CelerisUnavailable).baseUrl).toBe(baseUrl);
  });

  it("timeout -> CelerisUnavailable", async () => {
    mock.on("GET", "/api/v1/slow", () => {
      // 意図的に応答しない
    });
    const slowClient = new CelerisClient({ baseUrl: mock.baseUrl, timeoutMs: 100 });

    let error: unknown;
    try {
      await slowClient.get("/slow");
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisUnavailable);
  });
});

describe("CelerisClient.post", () => {
  it("sends application/json body and returns the parsed JSON response", async () => {
    mock.on("POST", "/api/v1/tasks/T1/approve", (_req, res, body) => {
      const received = JSON.parse(body) as unknown;
      sendJson(res, 200, { task: { id: "T1", status: "done" }, cascaded: [], received });
    });

    const result = await client.post<{
      task: { id: string; status: string };
      cascaded: string[];
      received: unknown;
    }>("/tasks/T1/approve", { note: "ok", expected_status: "ready" });

    const req = mock.requests.at(-1);
    expect(req?.headers["content-type"]).toBe("application/json");
    expect(JSON.parse(req?.body ?? "")).toEqual({ note: "ok", expected_status: "ready" });
    expect(result.task.status).toBe("done");
  });
});

describe("CelerisClient.patch", () => {
  it("sends application/json body via PATCH and returns the parsed JSON response", async () => {
    mock.on("PATCH", "/api/v1/providers/acct-a", (_req, res, body) => {
      const received = JSON.parse(body) as unknown;
      sendJson(res, 200, { id: "acct-a", received });
    });

    const result = await client.patch<{ id: string; received: unknown }>("/providers/acct-a", { concurrency: 3 });

    const req = mock.requests.at(-1);
    expect(req?.method).toBe("PATCH");
    expect(req?.headers["content-type"]).toBe("application/json");
    expect(JSON.parse(req?.body ?? "")).toEqual({ concurrency: 3 });
    expect(result.id).toBe("acct-a");
  });

  it("converts problem+json errors to CelerisError, same as post", async () => {
    mock.on("PATCH", "/api/v1/providers/missing", (_req, res) => {
      sendProblem(res, { status: 404, code: "provider_not_found", detail: "no such provider" });
    });

    let error: unknown;
    try {
      await client.patch("/providers/missing", {});
    } catch (e) {
      error = e;
    }
    expect(error).toBeInstanceOf(CelerisError);
    expect((error as CelerisError).status).toBe(404);
    expect((error as CelerisError).code).toBe("provider_not_found");
  });
});

describe("CelerisClient.delete", () => {
  it("sends DELETE and returns the parsed JSON response", async () => {
    mock.on("DELETE", "/api/v1/providers/acct-a", (_req, res) => {
      sendJson(res, 200, {});
    });

    const result = await client.delete<Record<string, never>>("/providers/acct-a");

    const req = mock.requests.at(-1);
    expect(req?.method).toBe("DELETE");
    expect(req?.headers.accept).toBe("application/json");
    expect(result).toEqual({});
  });

  it("204 with an empty body (no JSON) does not throw — returns {} (docs/celeris-api-v1.md §3.45)", async () => {
    // `DELETE /org/{id}` は `StatusCode::NO_CONTENT.into_response()`（本文なし）で返る。他の DELETE
    // （providers 等）は 200 `{}` だが、`res.json()` は空文字列の解析に失敗するので 204 は素通しする。
    mock.on("DELETE", "/api/v1/org/coding-poc", (_req, res) => {
      res.writeHead(204);
      res.end();
    });

    const result = await client.delete<Record<string, never>>("/org/coding-poc");

    expect(result).toEqual({});
  });

  it("converts problem+json errors to CelerisError, same as post", async () => {
    mock.on("DELETE", "/api/v1/providers/inuse", (_req, res) => {
      sendProblem(res, { status: 409, code: "account_in_use", detail: "in use" });
    });

    let error: unknown;
    try {
      await client.delete("/providers/inuse");
    } catch (e) {
      error = e;
    }
    expect(error).toBeInstanceOf(CelerisError);
    expect((error as CelerisError).status).toBe(409);
  });
});

describe("CelerisClient.stream", () => {
  it("sends accept/last-event-id and query, and can be read; abort ends the read", async () => {
    mock.on("GET", "/api/v1/stream", (_req, res) => {
      sendSseHello(res, { cursor: 1, now: "2026-09-15T00:00:00Z", daemon: null });
    });

    const controller = new AbortController();
    const res = await client.stream({
      taskId: "01JTESTTESTTESTTESTTESTTES",
      lastEventId: "12",
      signal: controller.signal,
    });

    const req = mock.requests.at(-1);
    expect(req?.url).toBe("/api/v1/stream?task_id=01JTESTTESTTESTTESTTESTTES");
    expect(req?.headers.accept).toBe("text/event-stream");
    expect(req?.headers["last-event-id"]).toBe("12");
    expect(res.body).not.toBeNull();

    const reader = res.body?.getReader();
    if (!reader) throw new Error("expected a readable body");
    const first = await reader.read();
    expect(first.done).toBe(false);
    expect(new TextDecoder().decode(first.value)).toContain("event: hello");

    controller.abort();
    let ended = false;
    try {
      const next = await reader.read();
      ended = next.done;
    } catch {
      ended = true;
    }
    expect(ended).toBe(true);
  });
});

describe("CelerisClient.file", () => {
  it("forwards Range header and download=1 query, returns the response as-is", async () => {
    mock.on("GET", "/api/v1/tasks/x/runs/y/stdout", (_req, res) => {
      res.writeHead(206, { "content-type": "application/octet-stream", "content-range": "bytes 0-9/100" });
      res.end("0123456789");
    });

    const res = await client.file("/tasks/x/runs/y/stdout", { range: "bytes=0-9", download: true });
    const req = mock.requests.at(-1);
    expect(req?.url).toBe("/api/v1/tasks/x/runs/y/stdout?download=1");
    expect(req?.headers.range).toBe("bytes=0-9");
    expect(res.status).toBe(206);
    expect(res.headers.get("content-range")).toBe("bytes 0-9/100");
    expect(await res.text()).toBe("0123456789");
  });

  it("forwards offset/length as query parameters", async () => {
    mock.on("GET", "/api/v1/tasks/x/runs/y/stdout", (_req, res) => {
      sendJson(res, 200, { ok: true });
    });

    await client.file("/tasks/x/runs/y/stdout", { offset: 10, length: 5 });
    const req = mock.requests.at(-1);
    expect(req?.url).toBe("/api/v1/tasks/x/runs/y/stdout?offset=10&length=5");
  });
});

describe("CelerisClient.url", () => {
  it("repeats array values and skips undefined", () => {
    const u = client.url("/tasks", { status: ["ready", "running"], q: undefined, limit: 5 });
    expect(u.pathname).toBe("/api/v1/tasks");
    expect(u.search).toBe("?status=ready&status=running&limit=5");
  });
});

describe("loadHealth", () => {
  it("returns {health, unavailable:false, problem:null} on success", async () => {
    const state = await loadHealth(client);
    expect(state).toEqual({ health: defaultHealth, unavailable: false, problem: null });
  });

  it("returns {unavailable:true} when celeris is unreachable", async () => {
    const closed = await startMockCeleris();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new CelerisClient({ baseUrl, timeoutMs: 1000 });

    const state = await loadHealth(unreachable);
    expect(state).toEqual({ health: null, unavailable: true, problem: null });
  });

  it("returns {problem: '401 unauthorized'} on an unauthorized problem+json response", async () => {
    mock.on("GET", "/api/v1/health", (_req, res) => {
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" });
    });

    const state = await loadHealth(client);
    expect(state).toEqual({ health: null, unavailable: false, problem: "401 unauthorized" });
  });
});
