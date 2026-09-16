import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadClusters } from "~/routes/clusters";
import { TaskdClient } from "~/taskd/client.server";
import type { Clusters } from "~/taskd/types";
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

const clustersView: Clusters = {
  items: [
    {
      id: "gpu-a",
      host: "gpu-a.example",
      concurrency: 2,
      sync: "rsync",
      delete_on_push: false,
      has_setup: true,
      env_keys: ["API_KEY"],
      rsync_excludes: [".git"],
      in_use: 1,
      connected: true,
      cooldown_until: null,
      cooldown_remaining_secs: null,
    },
    {
      id: "gpu-b",
      host: "gpu-b.example",
      concurrency: 1,
      sync: "none",
      delete_on_push: true,
      has_setup: false,
      env_keys: [],
      rsync_excludes: [],
      in_use: 0,
      connected: false,
      cooldown_until: "2026-09-15T00:05:00Z",
      cooldown_remaining_secs: 300,
    },
  ],
};

describe("loadClusters", () => {
  it("calls GET /clusters and returns {clusters} as-is", async () => {
    mock.on("GET", "/api/v1/clusters", (_req, res) => {
      sendJson(res, 200, clustersView);
    });

    const result = await loadClusters(client, new Request("http://gui.invalid/clusters"));

    expect(result.clusters).toEqual(clustersView);
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/clusters")).toBe(true);
  });

  it("rejects when taskd is not reachable (loader converts this to a Response)", async () => {
    const closed = await startMockTaskd();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new TaskdClient({ baseUrl, timeoutMs: 1000 });

    await expect(loadClusters(unreachable, new Request("http://gui.invalid/clusters"))).rejects.toBeTruthy();
  });
});
