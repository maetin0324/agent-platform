import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { buildNewPlanSpec, loadNewPlan } from "~/routes/plans.new";
import { TaskdClient } from "~/taskd/client.server";
import { createPlan } from "~/taskd/route-actions.server";
import type { Task } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

// docs/adr/0005 D5: Plan フォームは NewPlanSpec と 1:1。goal は空でも送り taskd の 422 文言を出す。

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

function form(entries: Record<string, string>): FormData {
  const f = new FormData();
  for (const [k, v] of Object.entries(entries)) f.append(k, v);
  return f;
}

describe("buildNewPlanSpec", () => {
  it("sends goal always (even empty) and omits empty optionals", () => {
    expect(buildNewPlanSpec(form({ goal: "", workspace: "", priority: "", max_turns: "" }))).toEqual({ goal: "" });
  });

  it("maps filled fields, numbers as numbers, unknown tier ignored", () => {
    expect(
      buildNewPlanSpec(
        form({
          goal: "build it",
          workspace: "ws-p",
          tier: "cheap",
          priority: "2",
          max_turns: "5",
          max_wall_secs: "60",
          max_retries: "0",
        }),
      ),
    ).toEqual({
      goal: "build it",
      workspace: "ws-p",
      tier: "cheap",
      priority: 2,
      max_turns: 5,
      max_wall_secs: 60,
      max_retries: 0,
    });
    expect(buildNewPlanSpec(form({ goal: "g", tier: "ultra" }))).toEqual({ goal: "g" });
  });
});

describe("createPlan / loadNewPlan", () => {
  it("POST /plans and returns the Task (kind plan, draft)", async () => {
    const task = { id: "01HZZZZZZZZZZZZZZZZZZZZZZZ", kind: "plan", status: "draft" } as unknown as Task;
    mock.on("POST", "/api/v1/plans", (_req, res) => sendJson(res, 201, task));
    const outcome = await createPlan(client, { goal: "g" });
    expect(outcome).toEqual({ ok: true, task });
    expect(JSON.parse(mock.requests.at(-1)?.body ?? "")).toEqual({ goal: "g" });
  });

  it("422 goal must not be blank → fields.goal with taskd's wording", async () => {
    mock.on("POST", "/api/v1/plans", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "goal must not be blank",
        extra: { errors: [{ field: "goal", message: "goal must not be blank" }] },
      }),
    );
    const outcome = await createPlan(client, { goal: "  " });
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.status).toBe(422);
    expect(outcome.error.fields.goal).toEqual(["goal must not be blank"]);
  });

  it("loadNewPlan returns GET /config as-is", async () => {
    const config = { plan_auto_accept: false, workspace_root: "/tmp/ws" };
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, config));
    expect(await loadNewPlan(client, new Request("http://gui.invalid/plans/new"))).toEqual(config);
  });
});
