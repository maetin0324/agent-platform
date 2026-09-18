import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadOrg } from "~/routes/org";
import { TaskdClient } from "~/taskd/client.server";
import {
  buildOrgCreateInput,
  buildOrgPatchInput,
  createOrgNode,
  deleteOrgNode,
  patchOrgNode,
} from "~/taskd/org-admin.server";
import type { MemoryView, OrgList, OrgNode, StandingRuleList, TaskList } from "~/taskd/types";
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

const orgNode = (id: string, over: Partial<OrgNode> = {}): OrgNode => ({
  id,
  parent_id: null,
  name: id,
  kind: "section",
  position: 0,
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

describe("buildOrgCreateInput", () => {
  it("reads id/name/kind/parent_id/genre/brief/position from a FormData", () => {
    const form = new FormData();
    form.set("id", "coding-poc");
    form.set("name", "PoC・R&D 課");
    form.set("kind", "section");
    form.set("parent_id", "coding");
    form.set("genre", "coding");
    form.set("brief", "小さく試す");
    form.set("position", "4");
    expect(buildOrgCreateInput(form)).toEqual({
      id: "coding-poc",
      name: "PoC・R&D 課",
      kind: "section",
      parent_id: "coding",
      genre: "coding",
      brief: "小さく試す",
      position: 4,
    });
  });

  it("omits optional fields when blank, defaults kind to section", () => {
    const form = new FormData();
    form.set("id", "x");
    form.set("name", "X");
    expect(buildOrgCreateInput(form)).toEqual({ id: "x", name: "X", kind: "section" });
  });
});

describe("buildOrgPatchInput", () => {
  it("genre: 空の選択肢を選ぶと明示的に null（分野なし）を送る", () => {
    const form = new FormData();
    form.set("name", "N");
    form.set("kind", "section");
    form.set("parent_id", "coding");
    form.set("genre", "");
    form.set("brief", "");
    const body = buildOrgPatchInput(form);
    expect(body.genre).toBeNull();
  });

  it("genre: 値を選べばその値を送る", () => {
    const form = new FormData();
    form.set("name", "N");
    form.set("kind", "section");
    form.set("parent_id", "coding");
    form.set("genre", "research");
    form.set("brief", "");
    expect(buildOrgPatchInput(form).genre).toBe("research");
  });

  it("parent_id: 空の選択肢を選ぶと明示的に null（根にする）を送る", () => {
    const form = new FormData();
    form.set("name", "N");
    form.set("kind", "secretary");
    form.set("parent_id", "");
    form.set("brief", "");
    expect(buildOrgPatchInput(form).parent_id).toBeNull();
  });
});

const emptyTaskList: TaskList = { items: [], total: 0, next_cursor: null, counts_by_status: {} };

describe("loadOrg", () => {
  it("GET /org と、1 回の GET /tasks（limit=500）の assignee から抱えている仕事を数える（GUI-R3、Phase 27）", async () => {
    const org: OrgList = {
      items: [
        orgNode("secretary", { kind: "secretary" }),
        orgNode("coding", { kind: "department", parent_id: "secretary" }),
        orgNode("coding-poc", { parent_id: "coding", genre: "coding" }),
      ],
    };
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, org));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, { genres: [{ id: "coding" }] }));
    mock.on("GET", "/api/v1/tasks", (_req, res) =>
      sendJson(res, 200, {
        items: [
          {
            id: "t1",
            title: "PoC",
            status: "running",
            parent_id: null,
            depends_on: [],
            assignee: "coding-poc",
            conversation: false,
            kind: "execute",
            priority: 0,
            tier: "standard",
            attempts: 0,
            max_retries: 0,
            created_at: "…",
            updated_at: "…",
            children: 0,
            pending_children: 0,
            actions: [],
          },
          {
            id: "t2",
            title: "検証",
            status: "done",
            parent_id: null,
            depends_on: [],
            assignee: "coding-poc",
            conversation: false,
            kind: "execute",
            priority: 0,
            tier: "standard",
            attempts: 0,
            max_retries: 0,
            created_at: "…",
            updated_at: "…",
            children: 0,
            pending_children: 0,
            actions: [],
          },
          {
            id: "chat",
            title: "対話: …[秘書]",
            status: "done",
            parent_id: null,
            depends_on: [],
            assignee: "secretary",
            conversation: true,
            kind: "execute",
            priority: 0,
            tier: "standard",
            attempts: 0,
            max_retries: 0,
            created_at: "…",
            updated_at: "…",
            children: 0,
            pending_children: 0,
            actions: [],
          },
        ],
        total: 3,
        next_cursor: null,
        counts_by_status: {},
      } satisfies TaskList),
    );

    const result = await loadOrg(client, new Request("http://gui.invalid/org"));

    expect(result.org).toEqual(org);
    expect(result.genres).toEqual(["coding"]);
    expect(result.workload["coding-poc"]).toEqual({ open: 1, total: 2 });
    // 対話用タスク（conversation: true）は数えない・一覧にも出さない。
    expect(result.workload.secretary).toBeUndefined();
    expect(result.tasksByAssignee["coding-poc"]?.map((t) => t.id)).toEqual(["t1", "t2"]);
    expect(result.tasksByAssignee.secretary).toBeUndefined();
    const req = mock.requests.find((r) => r.url.startsWith("/api/v1/tasks"));
    expect(req?.url).toContain("limit=500");
  });

  it("GET /config が失敗しても組織は返す（genres は空になる）", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/config", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTaskList));

    const result = await loadOrg(client, new Request("http://gui.invalid/org"));
    expect(result.genres).toEqual([]);
    expect(result.workload).toEqual({});
  });

  it("?selected= が無ければ GET /standing-rules を呼ばない（standingRules は空）", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, { genres: [] }));
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTaskList));

    const result = await loadOrg(client, new Request("http://gui.invalid/org"));
    expect(result.standingRules).toEqual([]);
    expect(mock.requests.some((r) => r.url.startsWith("/api/v1/standing-rules"))).toBe(false);
  });

  it("?selected=<node> があれば GET /standing-rules?node=<node> を呼ぶ（ADR-0033 D5、§3.58）", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, { genres: [] }));
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTaskList));
    mock.on("GET", "/api/v1/standing-rules", (_req, res) =>
      sendJson(res, 200, {
        items: [{ id: "s1", node_id: "coding-poc", rule: "毎回聞かずに進めてよい", created_at: "…" }],
      } satisfies StandingRuleList),
    );

    const result = await loadOrg(client, new Request("http://gui.invalid/org?selected=coding-poc"));
    expect(result.standingRules).toEqual([
      { id: "s1", node_id: "coding-poc", rule: "毎回聞かずに進めてよい", created_at: "…" },
    ]);
    const req = mock.requests.find((r) => r.url.startsWith("/api/v1/standing-rules"));
    expect(req?.url).toContain("node=coding-poc");
  });

  it("GET /standing-rules が失敗しても組織は返す（standingRules は空になる）", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, { genres: [] }));
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTaskList));
    mock.on("GET", "/api/v1/standing-rules", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );

    const result = await loadOrg(client, new Request("http://gui.invalid/org?selected=coding-poc"));
    expect(result.standingRules).toEqual([]);
  });
});

/**
 * 記憶（SPEC §3.2「記憶は案件をまたぐ」。ADR-0033 D6、docs/taskd-api-v1.md §3.62、監査 M4）。
 * 読み取り専用。`[memory]` 未設定は 409 `memory_unavailable` で、その旨だけを画面に出す。
 */
describe("loadOrg と記憶（GET /org/{id}/memory）", () => {
  const baseMocks = () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/config", (_req, res) => sendJson(res, 200, { genres: [] }));
    mock.on("GET", "/api/v1/tasks", (_req, res) => sendJson(res, 200, emptyTaskList));
    mock.on("GET", "/api/v1/standing-rules", (_req, res) =>
      sendJson(res, 200, { items: [] } satisfies StandingRuleList),
    );
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] }));
  };

  it("担当を選んでいなければ読まない", async () => {
    baseMocks();
    const result = await loadOrg(client, new Request("http://gui.invalid/org"));
    expect(result.memory).toBeNull();
    expect(result.memoryUnavailable).toBe(false);
    expect(mock.requests.some((r) => r.url.includes("/memory"))).toBe(false);
  });

  it("担当を選ぶと GET /org/{id}/memory を読む（案件を選べば ?project= も送る）", async () => {
    baseMocks();
    const memory: MemoryView = {
      notes: "- 2026-09-17: pegasus は pjsub で投げる\n",
      notes_path: "/var/lib/taskd/memory/coding-poc/notes.md",
      project: "- 2026-09-17: Pluvio は非同期ランタイム基盤\n",
      project_path: "/var/lib/taskd/memory/coding-poc/projects/p1.md",
    };
    mock.on("GET", "/api/v1/org/coding-poc/memory", (_req, res) => sendJson(res, 200, memory));

    const result = await loadOrg(client, new Request("http://gui.invalid/org?selected=coding-poc&project=p1"));
    expect(result.memory).toEqual(memory);
    const asked = mock.requests.find((r) => r.url.includes("/memory"));
    expect(asked?.url).toBe("/api/v1/org/coding-poc/memory?project=p1");
  });

  it("409 memory_unavailable は「記憶の置き場所が設定されていない」として扱う（画面は壊さない）", async () => {
    baseMocks();
    mock.on("GET", "/api/v1/org/coding-poc/memory", (_req, res) =>
      sendProblem(res, { status: 409, code: "memory_unavailable", detail: "[memory] is not configured" }),
    );

    const result = await loadOrg(client, new Request("http://gui.invalid/org?selected=coding-poc"));
    expect(result.memory).toBeNull();
    expect(result.memoryUnavailable).toBe(true);
  });

  it("他の失敗（404 など）は「記憶なし」として黙って畳む（組織は出す）", async () => {
    baseMocks();
    mock.on("GET", "/api/v1/org/coding-poc/memory", (_req, res) =>
      sendProblem(res, { status: 404, code: "org_node_not_found", detail: "no such node" }),
    );

    const result = await loadOrg(client, new Request("http://gui.invalid/org?selected=coding-poc"));
    expect(result.memory).toBeNull();
    expect(result.memoryUnavailable).toBe(false);
  });
});

describe("createOrgNode / patchOrgNode / deleteOrgNode (ADR-0033 D1, docs/taskd-api-v1.md §3.43〜3.45)", () => {
  it("POST /org — success", async () => {
    const node = orgNode("coding-poc", { parent_id: "coding" });
    mock.on("POST", "/api/v1/org", (_req, res) => sendJson(res, 201, node));
    const result = await createOrgNode(client, { id: "coding-poc", name: "coding-poc", kind: "section" });
    expect(result).toEqual({ ok: true, op: "create", id: "coding-poc", node });
  });

  it("POST /org — 409 org_node_exists は文言をそのまま返す", async () => {
    mock.on("POST", "/api/v1/org", (_req, res) =>
      sendProblem(res, { status: 409, code: "org_node_exists", detail: "coding-poc already exists" }),
    );
    const result = await createOrgNode(client, { id: "coding-poc", name: "x", kind: "section" });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.status).toBe(409);
      expect(result.error.code).toBe("org_node_exists");
      expect(result.error.detail).toBe("coding-poc already exists");
    }
  });

  it("POST /org — 401 unauthorized（token_file 未設定でも管理系は拒否される）", async () => {
    mock.on("POST", "/api/v1/org", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const result = await createOrgNode(client, { id: "x", name: "x", kind: "section" });
    expect(result).toEqual({
      ok: false,
      op: "create",
      id: "x",
      error: expect.objectContaining({ status: 401, code: "unauthorized" }) as unknown,
    });
  });

  it("PATCH /org/{id} — success", async () => {
    const node = orgNode("coding-poc", { brief: "更新後" });
    mock.on("PATCH", "/api/v1/org/coding-poc", (_req, res) => sendJson(res, 200, node));
    const result = await patchOrgNode(client, "coding-poc", { brief: "更新後" });
    expect(result).toEqual({ ok: true, op: "patch", id: "coding-poc", node });
  });

  it("PATCH /org/{id} — 404 org_node_not_found", async () => {
    mock.on("PATCH", "/api/v1/org/missing", (_req, res) =>
      sendProblem(res, { status: 404, code: "org_node_not_found", detail: "no such node" }),
    );
    const result = await patchOrgNode(client, "missing", {});
    expect(result.ok).toBe(false);
  });

  it("DELETE /org/{id} — success (taskd sends 204 with an empty body, not 200 {})", async () => {
    // `crates/task-api/src/handlers.rs::delete_org_node` は `StatusCode::NO_CONTENT.into_response()`
    // （本文なし）を返す。`res.json()` はそれを空文字列として解析に失敗するので、
    // `TaskdClient.delete` は 204 を特別扱いする必要がある（`~/taskd/client.server.ts`）。
    mock.on("DELETE", "/api/v1/org/coding-poc", (_req, res) => {
      res.writeHead(204);
      res.end();
    });
    const result = await deleteOrgNode(client, "coding-poc");
    expect(result).toEqual({ ok: true, op: "delete", id: "coding-poc" });
  });

  it("DELETE /org/{id} — 409 org_node_in_use（仕事を抱えている・子ノードがある）の文言をそのまま返す", async () => {
    mock.on("DELETE", "/api/v1/org/coding", (_req, res) =>
      sendProblem(res, { status: 409, code: "org_node_in_use", detail: "coding has 2 open task(s)" }),
    );
    const result = await deleteOrgNode(client, "coding");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.code).toBe("org_node_in_use");
      expect(result.error.detail).toBe("coding has 2 open task(s)");
      expect(result.error.status).toBe(409);
    }
  });

  it("組織の編集は成功しても POST /reload を呼ばない（DB が正。ADR-0033 D1）", async () => {
    mock.on("POST", "/api/v1/org", (_req, res) => sendJson(res, 201, orgNode("x")));
    await createOrgNode(client, { id: "x", name: "x", kind: "section" });
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });
});
