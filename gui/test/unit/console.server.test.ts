import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { buildInstructBodyFromForm, loadConsole, sendInstruct } from "~/celeris/console.server";
import type { ConsoleInstructAccepted, OrgList, ProjectList } from "~/celeris/types";
import { consolePage } from "../mock-celeris/fixtures";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

/**
 * Console の中継（ADR-0048 D1/D3、`~/celeris/console.server.ts`、GUI Phase G22）。実 celeris は起動せず、
 * プロセス内の偽 celeris（`test/mock-celeris/server.ts`）だけを見る（`test/unit/conversation.test.ts` を
 * 削除する前と同じ作法）。
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

function form(entries: Array<[string, string]>): FormData {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
}

describe("loadConsole", () => {
  it("GET /console・GET /org・GET /projects を束ね、scope をそのまま渡す", async () => {
    const org: OrgList = {
      items: [
        {
          id: "cos",
          parent_id: null,
          name: "CoS",
          kind: "secretary",
          position: 0,
          created_at: "2026-09-20T00:00:00Z",
          updated_at: "2026-09-20T00:00:00Z",
        },
      ],
    };
    mock.on("GET", "/api/v1/console", (req, res) => {
      expect(req.url).toBe("/api/v1/console?scope=project%3A01JPROJECT&limit=100");
      sendJson(res, 200, consolePage());
    });
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, org));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));

    const result = await loadConsole(client, "project:01JPROJECT", new Request("http://gui.invalid/"));
    expect(result.scope).toBe("project:01JPROJECT");
    expect(result.page.items.length).toBeGreaterThan(0);
    expect(result.org).toHaveLength(1);
    expect(result.projects).toEqual([]);
  });

  it("GET /org・GET /projects が落ちても Console 自体は出す（空扱い）", async () => {
    mock.on("GET", "/api/v1/console", (_req, res) => sendJson(res, 200, consolePage()));
    mock.on("GET", "/api/v1/org", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );

    const result = await loadConsole(client, "all", new Request("http://gui.invalid/"));
    expect(result.org).toEqual([]);
    expect(result.projects).toEqual([]);
    expect(result.page.items.length).toBeGreaterThan(0);
  });

  // ADR-0056 D4（Phase 78/80）: `GET /mcp/clients` は human ブロックの「外部（<name>）」帯の名前解決に使う。
  it("GET /mcp/clients を束ね、mcpClients としてそのまま渡す", async () => {
    mock.on("GET", "/api/v1/console", (_req, res) => sendJson(res, 200, consolePage()));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/mcp/clients", (_req, res) =>
      sendJson(res, 200, { items: [{ id: "chatgpt", name: "chatgpt", created_at: "2026-09-20T00:00:00Z" }] }),
    );

    const result = await loadConsole(client, "all", new Request("http://gui.invalid/"));
    expect(result.mcpClients).toHaveLength(1);
    expect(result.mcpClients[0].id).toBe("chatgpt");
  });

  it("GET /mcp/clients が落ちても（トークン未設定等）Console 自体は出す（空扱い）", async () => {
    mock.on("GET", "/api/v1/console", (_req, res) => sendJson(res, 200, consolePage()));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/mcp/clients", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await loadConsole(client, "all", new Request("http://gui.invalid/"));
    expect(result.mcpClients).toEqual([]);
    expect(result.page.items.length).toBeGreaterThan(0);
  });

  it("GET /console が落ちたら投げる（loader 側が celerisErrorResponse / isCelerisUnavailable で処理する）", async () => {
    mock.on("GET", "/api/v1/console", (_req, res) =>
      sendProblem(res, { status: 400, code: "bad_request", detail: "bad scope" }),
    );
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));

    await expect(loadConsole(client, "bogus", new Request("http://gui.invalid/"))).rejects.toMatchObject({
      status: 400,
      code: "bad_request",
    });
  });
});

describe("sendInstruct（POST /console/instruct。202 を素通しする）", () => {
  it("202 の ConsoleInstructAccepted をそのまま返す", async () => {
    const accepted: ConsoleInstructAccepted = { message_id: "01JMSG", task_id: "01JTASK", node_id: "cos" };
    mock.on("POST", "/api/v1/console/instruct", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ text: "このバグを直して" });
      sendJson(res, 202, accepted);
    });

    const outcome = await sendInstruct(client, { text: "このバグを直して" });
    expect(outcome).toEqual({ ok: true, op: "instruct", accepted });
  });

  it("scope を渡すとそのまま本文に載る", async () => {
    mock.on("POST", "/api/v1/console/instruct", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ text: "@coding-poc 直して", scope: "node:coding-poc" });
      sendJson(res, 202, { message_id: "m1", task_id: "t1", node_id: "coding-poc" } satisfies ConsoleInstructAccepted);
    });
    const outcome = await sendInstruct(client, { text: "@coding-poc 直して", scope: "node:coding-poc" });
    expect(outcome.ok).toBe(true);
  });

  it("404 org_node_not_found は文言をそのまま返す", async () => {
    mock.on("POST", "/api/v1/console/instruct", (_req, res) =>
      sendProblem(res, { status: 404, code: "org_node_not_found", detail: "no such node: nobody" }),
    );
    const outcome = await sendInstruct(client, { text: "x", scope: "node:nobody" });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.detail).toBe("no such node: nobody");
  });

  it("422 validation（空白のみ）は文言をそのまま返す", async () => {
    mock.on("POST", "/api/v1/console/instruct", (_req, res) =>
      sendProblem(res, { status: 422, code: "validation", detail: "text must not be blank" }),
    );
    const outcome = await sendInstruct(client, { text: "   " });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.status).toBe(422);
  });
});

describe("buildInstructBodyFromForm", () => {
  it("text と scope を読む", () => {
    expect(
      buildInstructBodyFromForm(
        form([
          ["text", "話す"],
          ["scope", "node:coding-poc"],
        ]),
      ),
    ).toEqual({
      text: "話す",
      scope: "node:coding-poc",
    });
  });

  it("scope が空なら送らない（celeris の既定 = CoS）", () => {
    expect(buildInstructBodyFromForm(form([["text", "話す"]]))).toEqual({ text: "話す" });
    expect(
      buildInstructBodyFromForm(
        form([
          ["text", "話す"],
          ["scope", ""],
        ]),
      ),
    ).toEqual({ text: "話す" });
  });
});
