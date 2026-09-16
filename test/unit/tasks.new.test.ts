import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { buildCriteria, buildDependsOn, buildNewTaskSpec, loadNewTask } from "~/routes/tasks.new";
import { TaskdClient } from "~/taskd/client.server";
import { createTask } from "~/taskd/route-actions.server";
import type { ConfigView, Task, TaskList } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

// docs/adr/0005 D5: フォームは NewTaskSpec と 1:1、空欄は省く、検証は taskd。

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

function form(entries: [string, string][]): FormData {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
}

const config = {
  config_path: "/tmp/taskd.toml",
  db: "/tmp/taskd.sqlite3",
  workspace_root: "/tmp/ws",
  tick_ms: 200,
  max_concurrency: 2,
  lease_grace_secs: 30,
  idle_timeout_secs: 60,
  kill_grace_secs: 5,
  review_timeout_secs: 60,
  error_cooldown_secs: 0,
  retry_backoff_base_secs: 0,
  retry_backoff_max_secs: 0,
  max_requeues: 3,
  plan_auto_accept: false,
  reviewer: { adapter: "fake", tier: "cheap" },
  providers: [],
  api: { bind: "127.0.0.1:7710", auth_required: false, allowed_hosts: [] },
} as unknown as ConfigView;

describe("buildCriteria", () => {
  it("pairs criterion_type[i] with criterion_value[i], skipping blank rows, keeping order", () => {
    const criteria = buildCriteria(
      form([
        ["criterion_type", "command"],
        ["criterion_value", "cargo test"],
        ["criterion_type", "human"],
        ["criterion_value", "   "],
        ["criterion_type", "artifact_exists"],
        ["criterion_value", "bench.json"],
        ["criterion_type", "reviewer"],
        ["criterion_value", "diff is minimal"],
        ["criterion_type", "human"],
        ["criterion_value", "someone signs off"],
      ]),
    );
    expect(criteria).toEqual([
      { type: "command", cmd: "cargo test", expect_exit: 0 },
      { type: "artifact_exists", name: "bench.json" },
      { type: "reviewer", text: "diff is minimal" },
      { type: "human", text: "someone signs off" },
    ]);
  });

  it("returns [] when every row is blank (taskd then answers 422)", () => {
    expect(
      buildCriteria(
        form([
          ["criterion_type", "human"],
          ["criterion_value", ""],
        ]),
      ),
    ).toEqual([]);
  });
});

describe("buildDependsOn", () => {
  it("merges checkboxes and the free-text field, de-duplicated", () => {
    const ids = buildDependsOn(
      form([
        ["depends_on", "A"],
        ["depends_on", "B"],
        ["depends_on_extra", "B, C\nD"],
      ]),
    );
    expect(ids).toEqual(["A", "B", "C", "D"]);
  });
});

describe("buildNewTaskSpec", () => {
  it("sends title/objective/acceptance always and omits empty optional fields", () => {
    const spec = buildNewTaskSpec(
      form([
        ["title", ""],
        ["objective", ""],
        ["kind", ""],
        ["tier", ""],
        ["priority", ""],
        ["max_turns", ""],
        ["workspace", ""],
      ]),
    );
    expect(spec).toEqual({ title: "", objective: "", acceptance: [] });
  });

  it("maps every filled field to the NewTaskSpec key, numbers as numbers", () => {
    const spec = buildNewTaskSpec(
      form([
        ["title", "t"],
        ["objective", "o"],
        ["criterion_type", "human"],
        ["criterion_value", "x"],
        ["kind", "review"],
        ["tier", "cheap"],
        ["adapter", "fake"],
        ["priority", "7"],
        ["parent", "P"],
        ["depends_on_extra", "D1"],
        ["max_turns", "3"],
        ["max_wall_secs", "120"],
        ["max_retries", "0"],
        ["workspace", "ws-x"],
        ["role", "reviewer-a"],
        ["aggregate", "on"],
      ]),
    );
    expect(spec).toEqual({
      title: "t",
      objective: "o",
      acceptance: [{ type: "human", text: "x" }],
      kind: "review",
      tier: "cheap",
      adapter: "fake",
      priority: 7,
      parent: "P",
      depends_on: ["D1"],
      max_turns: 3,
      max_wall_secs: 120,
      max_retries: 0,
      workspace: "ws-x",
      role: "reviewer-a",
      aggregate: true,
    });
  });

  it("role: sets spec.role when filled, any free-text name is accepted (not limited to [[roles]])", () => {
    const spec = buildNewTaskSpec(
      form([
        ["title", "t"],
        ["objective", "o"],
        ["role", "not-a-known-role"],
      ]),
    );
    expect(spec.role).toBe("not-a-known-role");
  });

  it("role: omits spec.role when the field is empty", () => {
    const spec = buildNewTaskSpec(
      form([
        ["title", "t"],
        ["objective", "o"],
        ["role", ""],
      ]),
    );
    expect(spec.role).toBeUndefined();
  });

  it("aggregate: sets spec.aggregate = true when the checkbox is present", () => {
    const spec = buildNewTaskSpec(
      form([
        ["title", "t"],
        ["objective", "o"],
        ["aggregate", "on"],
      ]),
    );
    expect(spec.aggregate).toBe(true);
  });

  it("aggregate: omits spec.aggregate (not false) when the checkbox is absent", () => {
    const spec = buildNewTaskSpec(
      form([
        ["title", "t"],
        ["objective", "o"],
      ]),
    );
    expect(spec.aggregate).toBeUndefined();
    expect("aggregate" in spec).toBe(false);
  });
});

describe("createTask / loadNewTask", () => {
  it("POST /tasks with the spec as JSON and returns the created Task", async () => {
    const task = { id: "01HZZZZZZZZZZZZZZZZZZZZZZZ", status: "draft" } as unknown as Task;
    mock.on("POST", "/api/v1/tasks", (_req, res) => sendJson(res, 201, task));
    const spec = { title: "t", objective: "o", acceptance: [{ type: "human" as const, text: "x" }] };
    const result = await createTask(client, spec);
    expect(result).toEqual({ ok: true, task });
    expect(JSON.parse(mock.requests.at(-1)?.body ?? "")).toEqual(spec);
  });

  it("422 with field acceptance → CreateFailure with taskd's wording under fields.acceptance", async () => {
    const message =
      "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)";
    mock.on("POST", "/api/v1/tasks", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: message,
        extra: { errors: [{ field: "acceptance", message }] },
      }),
    );
    const result = await createTask(client, { title: "t", objective: "o", acceptance: [] });
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.status).toBe(422);
    expect(result.error.fields.acceptance).toEqual([message]);
  });

  it("422 for a missing dependency lands under fields.depends_on", async () => {
    mock.on("POST", "/api/v1/tasks", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "dependency X does not exist",
        extra: { errors: [{ field: "depends_on", message: "dependency X does not exist" }] },
      }),
    );
    const result = await createTask(client, { title: "t", objective: "o", acceptance: [], depends_on: ["X"] });
    expect(!result.ok && result.error.fields.depends_on).toEqual(["dependency X does not exist"]);
  });

  it("loadNewTask fetches candidates (limit=500, created_desc) and config in parallel", async () => {
    const list: TaskList = { items: [], next_cursor: null, counts_by_status: {} } as unknown as TaskList;
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, list));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, config));
    const data = await loadNewTask(client, new Request("http://gui.invalid/tasks/new"));
    expect(data).toEqual({ candidates: list, config });
    const tasksReq = mock.requests.find((r) => r.url.startsWith("/api/v1/tasks"));
    expect(tasksReq?.url).toContain("limit=500");
    expect(tasksReq?.url).toContain("order=created_desc");
  });
});
