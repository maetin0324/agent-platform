import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import type { EventsPage } from "~/celeris/types";
import { loadRunEvents } from "~/routes/tasks.$id.runs.$runId.events";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

/**
 * `/tasks/:id/runs/:runId/events`（ADR-0048 D1 §3.100、`~/routes/tasks.$id.runs.$runId.events.ts`、
 * GUI Phase G22）。Console の progress ブロックの「すべて見る」が使う resource route。
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

describe("loadRunEvents", () => {
  it("GET /tasks/{id}/runs/{run_id}/events をそのまま返す", async () => {
    const page: EventsPage = {
      has_more: false,
      items: [
        {
          id: 1,
          seq: 1,
          task_id: "t1",
          ts: "2026-09-20T01:00:00Z",
          event: {
            type: "worker_progress",
            run_id: "r1",
            msg: "x",
            kind: "tool_use",
            tool: "Bash",
            summary: "cargo test",
          },
        },
      ],
    };
    mock.on("GET", "/api/v1/tasks/t1/runs/r1/events", (req, res) => {
      expect(req.url).toBe("/api/v1/tasks/t1/runs/r1/events");
      sendJson(res, 200, page);
    });

    const result = await loadRunEvents(client, "t1", "r1", new Request("http://gui.invalid/tasks/t1/runs/r1/events"));
    expect(result).toEqual(page);
  });

  it("after_seq / limit をクエリへ転送する", async () => {
    mock.on("GET", "/api/v1/tasks/t1/runs/r1/events", (req, res) => {
      expect(req.url).toBe("/api/v1/tasks/t1/runs/r1/events?after_seq=5&limit=200");
      sendJson(res, 200, { has_more: false, items: [] } satisfies EventsPage);
    });

    await loadRunEvents(client, "t1", "r1", new Request("http://gui.invalid/x?after_seq=5&limit=200"));
  });

  it("知らないタスク/run は 404 をそのまま投げる", async () => {
    mock.on("GET", "/api/v1/tasks/t1/runs/r1/events", (_req, res) =>
      sendProblem(res, { status: 404, code: "task_not_found", detail: "no such task" }),
    );

    await expect(
      loadRunEvents(client, "t1", "r1", new Request("http://gui.invalid/tasks/t1/runs/r1/events")),
    ).rejects.toMatchObject({ status: 404, code: "task_not_found" });
  });
});
