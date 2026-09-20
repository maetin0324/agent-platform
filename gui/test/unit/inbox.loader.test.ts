import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { CelerisError } from "~/celeris/errors";
import type { Inbox } from "~/celeris/types";
import { loadInbox } from "~/routes/inbox";
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

const emptyInbox: Inbox = {
  approvals: [],
  questions: [],
  drafts: [],
  attention: [],
  counts: { approvals: 0, questions: 0, drafts: 0, attention: 0, by_status: {} },
};

describe("loadInbox", () => {
  it("returns GET /inbox as-is", async () => {
    mock.on("GET", "/api/v1/inbox", (_req, res) => {
      sendJson(res, 200, emptyInbox);
    });

    const result = await loadInbox(client, new Request("http://gui.invalid/"));

    expect(result).toEqual(emptyInbox);
  });

  it("returns null (not throw) when celeris is unreachable, so / stays 200 (docs/adr/0003 D4)", async () => {
    const closed = await startMockCeleris();
    const unreachable = new CelerisClient({ baseUrl: closed.baseUrl, timeoutMs: 500 });
    await closed.close();

    const result = await loadInbox(unreachable, new Request("http://gui.invalid/"));

    expect(result).toBeNull();
  });

  it("converts other celeris errors into a thrown Response (docs/adr/0004 D6)", async () => {
    mock.on("GET", "/api/v1/inbox", (_req, res) => {
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" });
    });

    let thrown: unknown;
    try {
      await loadInbox(client, new Request("http://gui.invalid/"));
    } catch (e) {
      thrown = e;
    }

    expect(thrown).toBeInstanceOf(Response);
    const response = thrown as Response;
    expect(response.status).toBe(401);
    const body = (await response.json()) as { kind: string; code: string };
    expect(body.kind).toBe("celeris_error");
    expect(body.code).toBe("unauthorized");
  });

  it("does not wrap CelerisError as a plain Error (sanity check on celerisErrorResponse)", async () => {
    mock.on("GET", "/api/v1/inbox", (_req, res) => {
      sendProblem(res, { status: 500, code: "internal", detail: "boom" });
    });

    try {
      await loadInbox(client, new Request("http://gui.invalid/"));
      expect.unreachable("expected loadInbox to throw");
    } catch (e) {
      expect(e).not.toBeInstanceOf(CelerisError);
      expect(e).toBeInstanceOf(Response);
    }
  });
});
