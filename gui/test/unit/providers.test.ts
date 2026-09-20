import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import type { Providers } from "~/celeris/types";
import { ADAPTER_OPTIONS, loadProviders } from "~/routes/providers";
import { type MockCeleris, sendJson, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

const providersView: Providers = {
  items: [
    {
      id: "fake-local",
      adapter: "fake",
      tiers: ["standard"],
      concurrency: 2,
      model: "fake",
      env_keys: ["API_KEY"],
      in_use: 1,
      cooldown: null,
      stats: {
        runs: 10,
        done: 8,
        question: 0,
        error: 1,
        requeue: 1,
        lease_expired: 0,
        input_tokens: 1000,
        output_tokens: 200,
        by_day: [],
      },
    },
    {
      id: "acct-a",
      adapter: "fake",
      tiers: ["standard"],
      concurrency: 1,
      model: null,
      env_keys: ["ACCOUNT"],
      in_use: 0,
      cooldown: { provider: "acct-a", until: "2026-09-15T00:05:00Z", reason: "throttled" },
      stats: {
        runs: 1,
        done: 0,
        question: 0,
        error: 0,
        requeue: 1,
        lease_expired: 0,
        input_tokens: 0,
        output_tokens: 0,
        by_day: [],
      },
    },
  ],
};

describe("ADAPTER_OPTIONS", () => {
  it("includes acp (ADR-0026) and paperqa (ADR-0027) alongside the existing adapters", () => {
    expect(ADAPTER_OPTIONS).toEqual(["fake", "claude-code", "codex", "acp", "paperqa", "local-deep-research"]);
  });
});

describe("loadProviders", () => {
  it("calls GET /providers and returns {providers, fetchedAt} as-is", async () => {
    mock.on("GET", "/api/v1/providers", (_req, res) => {
      sendJson(res, 200, providersView);
    });

    const result = await loadProviders(client, new Request("http://gui.invalid/providers"));

    expect(result.providers).toEqual(providersView);
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/providers")).toBe(true);
  });

  it("returns fetchedAt as an ISO date string close to now", async () => {
    mock.on("GET", "/api/v1/providers", (_req, res) => {
      sendJson(res, 200, providersView);
    });

    const before = Date.now();
    const result = await loadProviders(client, new Request("http://gui.invalid/providers"));
    const after = Date.now();

    const fetchedAtMs = new Date(result.fetchedAt).getTime();
    expect(Number.isNaN(fetchedAtMs)).toBe(false);
    expect(result.fetchedAt).toBe(new Date(fetchedAtMs).toISOString());
    expect(fetchedAtMs).toBeGreaterThanOrEqual(before);
    expect(fetchedAtMs).toBeLessThanOrEqual(after);
  });

  it("rejects when celeris is not reachable (loader converts this to a Response)", async () => {
    const closed = await startMockCeleris();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new CelerisClient({ baseUrl, timeoutMs: 1000 });

    await expect(loadProviders(unreachable, new Request("http://gui.invalid/providers"))).rejects.toBeTruthy();
  });
});
