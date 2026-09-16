import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadInbox } from "~/routes/inbox";
import { TaskdClient } from "~/taskd/client.server";
import { TaskdError } from "~/taskd/errors";
import type { Inbox } from "~/taskd/types";
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

  it("returns null (not throw) when taskd is unreachable, so / stays 200 (docs/adr/0003 D4)", async () => {
    const closed = await startMockTaskd();
    const unreachable = new TaskdClient({ baseUrl: closed.baseUrl, timeoutMs: 500 });
    await closed.close();

    const result = await loadInbox(unreachable, new Request("http://gui.invalid/"));

    expect(result).toBeNull();
  });

  it("converts other taskd errors into a thrown Response (docs/adr/0004 D6)", async () => {
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
    expect(body.kind).toBe("taskd_error");
    expect(body.code).toBe("unauthorized");
  });

  it("does not wrap TaskdError as a plain Error (sanity check on taskdErrorResponse)", async () => {
    mock.on("GET", "/api/v1/inbox", (_req, res) => {
      sendProblem(res, { status: 500, code: "internal", detail: "boom" });
    });

    try {
      await loadInbox(client, new Request("http://gui.invalid/"));
      expect.unreachable("expected loadInbox to throw");
    } catch (e) {
      expect(e).not.toBeInstanceOf(TaskdError);
      expect(e).toBeInstanceOf(Response);
    }
  });
});
