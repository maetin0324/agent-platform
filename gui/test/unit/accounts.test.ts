import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import type { AccountList } from "~/celeris/types";
import { loadAccounts } from "~/routes/accounts";
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

const accountsView: AccountList = {
  root: "/home/u/celeris/claude-accounts",
  roots: { "claude-code": "/home/u/celeris/claude-accounts", codex: "/home/u/celeris/codex-accounts" },
  max_runs_per_account: 2,
  items: [
    {
      id: "a",
      adapter: "claude-code",
      dir: "/home/u/celeris/claude-accounts/a",
      logged_in: true,
      in_use: 1,
      usage: {
        five_hour: { utilization: 0.14, resets_at: "2026-09-16T05:00:00Z" },
        seven_day: { utilization: 0.24, resets_at: "2026-09-23T00:00:00Z" },
        status: "allowed",
        observed_at: "2026-09-16T00:00:00Z",
        source: "run",
      },
      score: 0.81,
      excluded_reason: null,
      cooldown: null,
      last_check: { at: "2026-09-16T00:00:00Z", result: "ok", detail: "ok" },
      login_pending: false,
      stats: { runs: 12, done: 10, error: 1, input_tokens: 1234, output_tokens: 567 },
    },
    {
      id: "c",
      adapter: "codex",
      dir: "/home/u/celeris/codex-accounts/c",
      logged_in: false,
      in_use: 0,
      usage: null,
      score: null,
      excluded_reason: "not_logged_in",
      cooldown: null,
      last_check: null,
      login_pending: false,
      stats: { runs: 0, done: 0, error: 0, input_tokens: 0, output_tokens: 0 },
    },
  ],
};

describe("loadAccounts", () => {
  it("calls GET /accounts and returns {accounts, fetchedAt} as-is", async () => {
    mock.on("GET", "/api/v1/accounts", (_req, res) => sendJson(res, 200, accountsView));

    const result = await loadAccounts(client, new Request("http://gui.invalid/accounts"));

    expect(result.accounts).toEqual(accountsView);
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/accounts")).toBe(true);
    expect(Number.isNaN(new Date(result.fetchedAt).getTime())).toBe(false);
  });

  it("returns {root: null, items: []} as-is when [accounts] is not configured", async () => {
    mock.on("GET", "/api/v1/accounts", (_req, res) =>
      sendJson(res, 200, { root: null, max_runs_per_account: 2, items: [] } satisfies AccountList),
    );

    const result = await loadAccounts(client, new Request("http://gui.invalid/accounts"));
    expect(result.accounts.root).toBeNull();
    expect(result.accounts.items).toEqual([]);
  });

  it("rejects when celeris is not reachable (loader converts this to a Response)", async () => {
    const closed = await startMockCeleris();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new CelerisClient({ baseUrl, timeoutMs: 1000 });

    await expect(loadAccounts(unreachable, new Request("http://gui.invalid/accounts"))).rejects.toBeTruthy();
  });

  // ADR-0056 D4（Phase 78/80）: `GET /mcp/clients` は `GET /llm/sources` / `GET /secrets` と同じ扱い
  // （落ちても `/accounts` 自体は壊さない。この節だけにエラーを出す）。
  it("calls GET /mcp/clients and returns it as-is under mcpClients", async () => {
    mock.on("GET", "/api/v1/accounts", (_req, res) => sendJson(res, 200, accountsView));
    mock.on("GET", "/api/v1/mcp/clients", (_req, res) =>
      sendJson(res, 200, {
        items: [{ id: "chatgpt", name: "chatgpt", created_at: "2026-09-20T00:00:00Z", scopes: ["knowledge:read"] }],
      }),
    );

    const result = await loadAccounts(client, new Request("http://gui.invalid/accounts"));
    expect(result.mcpClientsError).toBeNull();
    expect(result.mcpClients?.items).toHaveLength(1);
    expect(result.mcpClients?.items[0].id).toBe("chatgpt");
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/mcp/clients")).toBe(true);
  });

  it("falls back to mcpClientsError (not a thrown error) when GET /mcp/clients requires a token", async () => {
    mock.on("GET", "/api/v1/accounts", (_req, res) => sendJson(res, 200, accountsView));
    mock.on("GET", "/api/v1/mcp/clients", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await loadAccounts(client, new Request("http://gui.invalid/accounts"));
    expect(result.mcpClients).toBeNull();
    expect(result.mcpClientsError?.status).toBe(401);
    // /accounts 自体は壊れない
    expect(result.accounts).toEqual(accountsView);
  });
});
