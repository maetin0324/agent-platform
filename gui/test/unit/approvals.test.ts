import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  approvalNodeName,
  approvalProjectName,
  approvalsPendingCount,
  splitApprovals,
  standingRuleTargetName,
} from "~/lib/approvals";
import { loadApprovals } from "~/routes/approvals";
import {
  buildApprovalDecideInput,
  buildStandingRuleCreateInput,
  createStandingRule,
  decideApproval,
  deleteStandingRule,
} from "~/taskd/approvals-admin.server";
import { TaskdClient } from "~/taskd/client.server";
import type {
  Approval,
  ApprovalDecideResult,
  ApprovalList,
  DaemonView,
  OrgList,
  OrgNode,
  Project,
  ProjectList,
  StandingRule,
  StandingRuleList,
} from "~/taskd/types";
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

const approval = (id: string, over: Partial<Approval> = {}): Approval => ({
  id,
  node_id: "coding-poc",
  question: "which cluster should I use for the benchmark?",
  created_at: "2026-09-17T00:00:00Z",
  ...over,
});

const standingRule = (id: string, over: Partial<StandingRule> = {}): StandingRule => ({
  id,
  rule: "クラスタへの実験投入は毎回聞かずに進めてよい",
  created_at: "2026-09-17T00:00:00Z",
  ...over,
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

const project = (id: string, over: Partial<Project> = {}): Project => ({
  id,
  title: id,
  request: "…",
  status: "proposed",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

describe("approvalProjectName / approvalNodeName / standingRuleTargetName (~/lib/approvals.ts)", () => {
  const org = [orgNode("coding-poc", { name: "PoC・R&D 課" })];
  const projects = [project("p1", { title: "Pluvio" })];

  it("project_id が無ければ「案件なし」", () => {
    expect(approvalProjectName({ project_id: null }, projects)).toBe("案件なし");
  });

  it("project_id があれば GET /projects の title を出す", () => {
    expect(approvalProjectName({ project_id: "p1" }, projects)).toBe("Pluvio");
  });

  it("案件が見つからなければ id をそのまま出す", () => {
    expect(approvalProjectName({ project_id: "missing" }, projects)).toBe("missing");
  });

  it("ノード名は GET /org の name を出す。見つからなければ id", () => {
    expect(approvalNodeName({ node_id: "coding-poc" }, org)).toBe("PoC・R&D 課");
    expect(approvalNodeName({ node_id: "missing" }, org)).toBe("missing");
  });

  it("永続の認可の宛先: node_id が無ければ「全員」、あればノード名", () => {
    expect(standingRuleTargetName({ node_id: null }, org)).toBe("全員");
    expect(standingRuleTargetName({ node_id: "coding-poc" }, org)).toBe("PoC・R&D 課");
    expect(standingRuleTargetName({ node_id: "missing" }, org)).toBe("missing");
  });
});

describe("approvalsPendingCount (DaemonSnapshot.approvals_pending, docs/taskd-api-v1.md §3.20 の追加)", () => {
  it("daemon が無ければ 0", () => {
    expect(approvalsPendingCount(null)).toBe(0);
    expect(approvalsPendingCount(undefined)).toBe(0);
  });

  it("snapshot が無ければ 0（最初の tick より前）", () => {
    expect(approvalsPendingCount({ now: "…", snapshot: null } as DaemonView)).toBe(0);
  });

  it("古いスナップショットで approvals_pending が無ければ 0", () => {
    const daemon = { now: "…", snapshot: {} } as unknown as DaemonView;
    expect(approvalsPendingCount(daemon)).toBe(0);
  });

  it("approvals_pending をそのまま返す", () => {
    const daemon = { now: "…", snapshot: { approvals_pending: 3 } } as unknown as DaemonView;
    expect(approvalsPendingCount(daemon)).toBe(3);
  });
});

describe("splitApprovals (~/lib/approvals.ts. decision の有無で分ける)", () => {
  it("decision が無ければ pending、あれば decided", () => {
    const a1 = approval("a1");
    const a2 = approval("a2", { decision: "once", answer: "cluster-a" });
    const a3 = approval("a3", { decision: "standing", answer: "毎回聞かずに進めてよい" });
    expect(splitApprovals([a1, a2, a3])).toEqual({ pending: [a1], decided: [a2, a3] });
  });

  it("空配列", () => {
    expect(splitApprovals([])).toEqual({ pending: [], decided: [] });
  });
});

describe("loadApprovals (docs/taskd-api-v1.md §3.56。pending と決定済みを分ける)", () => {
  it("GET /approvals をフィルタ無しで 1 回だけ呼び、decision の有無で pending / decided に分ける", async () => {
    // 実機で確認した taskd の挙動（`pending=false` が絞り込まない）に合わせ、GUI 側はフィルタ無しで 1 回だけ呼ぶ
    // （docs/taskd-requests.md R5）。
    const a1 = approval("a1");
    const a2 = approval("a2", { decision: "once", answer: "cluster-a", decided_at: "2026-09-17T01:00:00Z" });
    mock.on("GET", "/api/v1/approvals", (_req, res) => sendJson(res, 200, { items: [a1, a2] } satisfies ApprovalList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/standing-rules", (_req, res) =>
      sendJson(res, 200, { items: [standingRule("s1")] } satisfies StandingRuleList),
    );

    const result = await loadApprovals(client, new Request("http://gui.invalid/approvals"));

    expect(result.pending).toEqual([a1]);
    expect(result.decided).toEqual([a2]);
    expect(result.standingRules).toEqual([standingRule("s1")]);
    const approvalRequests = mock.requests.filter((r) => r.url.startsWith("/api/v1/approvals"));
    expect(approvalRequests).toHaveLength(1);
    expect(approvalRequests[0].url).toBe("/api/v1/approvals");
  });

  it("GET /org・GET /projects が落ちても認可の一覧は返す（名前解決にしか使わないため）", async () => {
    mock.on("GET", "/api/v1/approvals", (_req, res) => sendJson(res, 200, { items: [] } satisfies ApprovalList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );
    mock.on("GET", "/api/v1/standing-rules", (_req, res) =>
      sendJson(res, 200, { items: [] } satisfies StandingRuleList),
    );

    const result = await loadApprovals(client, new Request("http://gui.invalid/approvals"));
    expect(result.org).toEqual([]);
    expect(result.projects).toEqual([]);
    expect(result.pending).toEqual([]);
    expect(result.decided).toEqual([]);
  });

  it("401 unauthorized（GET /approvals は通常の認証。§3.56 前書き）はそのまま投げる", async () => {
    mock.on("GET", "/api/v1/approvals", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/standing-rules", (_req, res) =>
      sendJson(res, 200, { items: [] } satisfies StandingRuleList),
    );

    await expect(loadApprovals(client, new Request("http://gui.invalid/approvals"))).rejects.toMatchObject({
      status: 401,
      code: "unauthorized",
    });
  });
});

describe("buildApprovalDecideInput", () => {
  it("answer / decision / scope を FormData から読む", () => {
    const form = new FormData();
    form.set("answer", "cluster-a を使ってよい");
    form.set("decision", "standing");
    form.set("scope", "all");
    expect(buildApprovalDecideInput(form)).toEqual({
      answer: "cluster-a を使ってよい",
      decision: "standing",
      scope: "all",
    });
  });

  it("decision が無効・無ければ既定 once、scope が無ければ省く", () => {
    const form = new FormData();
    form.set("answer", "ok");
    expect(buildApprovalDecideInput(form)).toEqual({ answer: "ok", decision: "once" });
  });
});

describe("decideApproval (docs/taskd-api-v1.md §3.57. POST /approvals/{id}/decide, 管理系)", () => {
  it("once — 既存の質問に答える経路で再開する", async () => {
    const result: ApprovalDecideResult = {
      approval: approval("a1", { decision: "once", answer: "cluster-a" }),
      transition: { id: "t1", from: "blocked", to: "running", reason: "answered" },
    };
    mock.on("POST", "/api/v1/approvals/a1/decide", (_req, res) => sendJson(res, 200, result));
    const outcome = await decideApproval(client, "a1", { decision: "once", answer: "cluster-a" });
    expect(outcome).toEqual({ ok: true, op: "decide", id: "a1", result });
  });

  it("standing — standing_rule も応答に含む", async () => {
    const result: ApprovalDecideResult = {
      approval: approval("a1", { decision: "standing", answer: "毎回聞かずに進めてよい" }),
      standing_rule: standingRule("s1"),
    };
    mock.on("POST", "/api/v1/approvals/a1/decide", (_req, res) => sendJson(res, 200, result));
    const outcome = await decideApproval(client, "a1", { decision: "standing", answer: "毎回聞かずに進めてよい" });
    expect(outcome.ok).toBe(true);
    if (outcome.ok) expect(outcome.result.standing_rule).toEqual(standingRule("s1"));
  });

  it("denied", async () => {
    const result: ApprovalDecideResult = { approval: approval("a1", { decision: "denied", answer: "認めない: だめ" }) };
    mock.on("POST", "/api/v1/approvals/a1/decide", (_req, res) => sendJson(res, 200, result));
    const outcome = await decideApproval(client, "a1", { decision: "denied", answer: "だめ" });
    expect(outcome).toEqual({ ok: true, op: "decide", id: "a1", result });
  });

  it("404 approval_not_found", async () => {
    mock.on("POST", "/api/v1/approvals/missing/decide", (_req, res) =>
      sendProblem(res, { status: 404, code: "approval_not_found", detail: "no such approval" }),
    );
    const outcome = await decideApproval(client, "missing", { decision: "once", answer: "x" });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.error.status).toBe(404);
      expect(outcome.error.code).toBe("approval_not_found");
    }
  });

  it("401 unauthorized（token_file 未設定でも管理系は拒否される）", async () => {
    mock.on("POST", "/api/v1/approvals/a1/decide", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const outcome = await decideApproval(client, "a1", { decision: "once", answer: "x" });
    expect(outcome).toEqual({
      ok: false,
      op: "decide",
      id: "a1",
      error: expect.objectContaining({ status: 401, code: "unauthorized" }) as unknown,
    });
  });

  it("422 validation（answer が空白だけ）", async () => {
    mock.on("POST", "/api/v1/approvals/a1/decide", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "invalid",
        extra: { errors: [{ field: "answer", message: "must not be blank" }] },
      }),
    );
    const outcome = await decideApproval(client, "a1", { decision: "once", answer: "   " });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.fields.answer).toEqual(["must not be blank"]);
  });
});

describe("buildStandingRuleCreateInput", () => {
  it("node_id / rule を読む", () => {
    const form = new FormData();
    form.set("node_id", "coding-poc");
    form.set("rule", "クラスタへの実験投入は毎回聞かずに進めてよい");
    expect(buildStandingRuleCreateInput(form)).toEqual({
      node_id: "coding-poc",
      rule: "クラスタへの実験投入は毎回聞かずに進めてよい",
    });
  });

  it("node_id が空なら省く（全員向け）", () => {
    const form = new FormData();
    form.set("rule", "全員向けの規則");
    expect(buildStandingRuleCreateInput(form)).toEqual({ rule: "全員向けの規則" });
  });
});

describe("createStandingRule / deleteStandingRule (docs/taskd-api-v1.md §3.59〜3.60, 管理系)", () => {
  it("POST /standing-rules — success", async () => {
    const rule = standingRule("s1", { node_id: "coding-poc" });
    mock.on("POST", "/api/v1/standing-rules", (_req, res) => sendJson(res, 201, rule));
    const outcome = await createStandingRule(client, { node_id: "coding-poc", rule: rule.rule });
    expect(outcome).toEqual({ ok: true, op: "create", id: "s1", rule });
  });

  it("POST /standing-rules — 422 validation（rule が空白だけ）", async () => {
    mock.on("POST", "/api/v1/standing-rules", (_req, res) =>
      sendProblem(res, { status: 422, code: "validation", detail: "invalid" }),
    );
    const outcome = await createStandingRule(client, { rule: "   " });
    expect(outcome.ok).toBe(false);
  });

  it("POST /standing-rules — 401 unauthorized", async () => {
    mock.on("POST", "/api/v1/standing-rules", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const outcome = await createStandingRule(client, { rule: "x" });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.status).toBe(401);
  });

  it("DELETE /standing-rules/{id} — success (204, no body)", async () => {
    mock.on("DELETE", "/api/v1/standing-rules/s1", (_req, res) => {
      res.writeHead(204);
      res.end();
    });
    const outcome = await deleteStandingRule(client, "s1");
    expect(outcome).toEqual({ ok: true, op: "delete", id: "s1" });
  });

  it("DELETE /standing-rules/{id} — 404 standing_rule_not_found", async () => {
    mock.on("DELETE", "/api/v1/standing-rules/missing", (_req, res) =>
      sendProblem(res, { status: 404, code: "standing_rule_not_found", detail: "no such rule" }),
    );
    const outcome = await deleteStandingRule(client, "missing");
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.error.status).toBe(404);
      expect(outcome.error.code).toBe("standing_rule_not_found");
    }
  });
});
