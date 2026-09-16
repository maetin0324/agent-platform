import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadAccounts } from "~/routes/accounts";
import { TaskdClient } from "~/taskd/client.server";
import type { AccountList } from "~/taskd/types";
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

const accountsView: AccountList = {
  root: "/home/u/taskd/claude-accounts",
  max_runs_per_account: 2,
  items: [
    {
      id: "a",
      dir: "/home/u/taskd/claude-accounts/a",
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

  it("rejects when taskd is not reachable (loader converts this to a Response)", async () => {
    const closed = await startMockTaskd();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new TaskdClient({ baseUrl, timeoutMs: 1000 });

    await expect(loadAccounts(unreachable, new Request("http://gui.invalid/accounts"))).rejects.toBeTruthy();
  });
});
