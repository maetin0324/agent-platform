import { describe, expect, it } from "vitest";
import type { OrgNode, TaskSummary } from "~/celeris/types";
import { buildOrgTree, countWorkload, tasksByAssignee } from "~/lib/org-tree";
import { orgList } from "../mock-celeris/fixtures";

/**
 * `buildOrgTree` / `countWorkload` / `tasksByAssignee`（`/org` の loader が使う純粋関数）のテスト。
 * `GET /org` は平らな配列（position 昇順）を返すだけなので、木に組む・件数を数える判断は
 * すべて GUI 側のこの純粋関数に閉じている（docs/gui/api.md §3.42）。
 */

const node = (id: string, over: Partial<OrgNode> = {}): OrgNode => ({
  id,
  parent_id: null,
  name: id,
  kind: "section",
  position: 0,
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

describe("buildOrgTree", () => {
  it("根（secretary）の下に部・課を parent_id で組む", () => {
    const items = [
      node("secretary", { kind: "secretary", parent_id: null, position: 0 }),
      node("coding", { kind: "department", parent_id: "secretary", position: 1 }),
      node("coding-frontend", { parent_id: "coding", position: 1 }),
      node("coding-poc", { parent_id: "coding", position: 3 }),
      node("research", { kind: "department", parent_id: "secretary", position: 2 }),
    ];

    const { roots, orphanIds } = buildOrgTree(items);

    expect(orphanIds).toEqual([]);
    expect(roots).toHaveLength(1);
    expect(roots[0].node.id).toBe("secretary");
    expect(roots[0].children.map((c) => c.node.id)).toEqual(["coding", "research"]);
    const coding = roots[0].children.find((c) => c.node.id === "coding");
    expect(coding?.children.map((c) => c.node.id)).toEqual(["coding-frontend", "coding-poc"]);
  });

  it("同じ親の中では position 昇順、同値は id 昇順に並ぶ", () => {
    const items = [
      node("secretary", { kind: "secretary" }),
      node("b", { parent_id: "secretary", position: 1 }),
      node("a", { parent_id: "secretary", position: 1 }),
      node("c", { parent_id: "secretary", position: 0 }),
    ];
    const { roots } = buildOrgTree(items);
    expect(roots[0].children.map((c) => c.node.id)).toEqual(["c", "a", "b"]);
  });

  it("存在しない parent_id を指す孤児は、根（secretary）の下に出す", () => {
    const items = [node("secretary", { kind: "secretary" }), node("orphan", { parent_id: "no-such-parent" })];
    const { roots, orphanIds } = buildOrgTree(items);
    expect(orphanIds).toEqual(["orphan"]);
    expect(roots[0].children.map((c) => c.node.id)).toEqual(["orphan"]);
  });

  it("孤児の子（存在する parent_id）は孤児の下にそのまま付く", () => {
    const items = [
      node("secretary", { kind: "secretary" }),
      node("orphan", { parent_id: "missing" }),
      node("orphan-child", { parent_id: "orphan" }),
    ];
    const { roots } = buildOrgTree(items);
    const orphan = roots[0].children.find((c) => c.node.id === "orphan");
    expect(orphan?.children.map((c) => c.node.id)).toEqual(["orphan-child"]);
  });

  it("secretary が無い（壊れた設定）ときも、parent_id が無いノードは根として扱う", () => {
    const items = [node("a", { kind: "department", parent_id: null })];
    const { roots } = buildOrgTree(items);
    expect(roots.map((r) => r.node.id)).toEqual(["a"]);
  });

  it("空配列なら根も孤児も無い", () => {
    expect(buildOrgTree([])).toEqual({ roots: [], orphanIds: [] });
  });

  it("U12（フェーズ 73）: 4 段以上深いサブツリーも parent_id を辿って正しく組む（`/org` の開閉トグルの土台）", () => {
    const { roots } = buildOrgTree(orgList().items);
    const cos = roots.find((r) => r.node.id === "cos");
    const coding = cos?.children.find((c) => c.node.id === "coding");
    const codingPoc = coding?.children.find((c) => c.node.id === "coding-poc");
    expect(codingPoc?.children.map((c) => c.node.id)).toEqual(["coding-poc-alpha", "coding-poc-beta"]);
    const alpha = codingPoc?.children.find((c) => c.node.id === "coding-poc-alpha");
    const alpha1 = alpha?.children.find((c) => c.node.id === "coding-poc-alpha-1");
    expect(alpha1?.children.map((c) => c.node.id)).toEqual(["coding-poc-alpha-1-x"]);
    // cos → coding → coding-poc → coding-poc-alpha → coding-poc-alpha-1 → coding-poc-alpha-1-x
    // の 5 段（開閉トグルは children.length > 0 のノードにだけ出る。`coding-poc` を畳むとこの 4 世代
    // ぶんが一度に隠れる、という「大きな組織で効果が出る」ことの土台をここで確認する）。
    expect(alpha1?.children[0]?.children).toEqual([]);
  });
});

const task = (id: string, over: Partial<TaskSummary> = {}): TaskSummary => ({
  id,
  parent_id: null,
  kind: "execute",
  status: "running",
  title: id,
  priority: 0,
  tier: "standard",
  attempts: 0,
  max_retries: 0,
  depends_on: [],
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  children: 0,
  pending_children: 0,
  conversation: false,
  actions: [],
  assignee: null,
  // ADR-0044 D3（Phase 53）で `TaskSummary` に増えた必須項目。
  labels: [],
  category: "other",
  priority_label: "P3",
  ...over,
});

describe("countWorkload", () => {
  it("assignee ごとに、終端でない（open）ものと全体（total）を数える", () => {
    const tasks = [
      task("t1", { assignee: "coding-poc", status: "running" }),
      task("t2", { assignee: "coding-poc", status: "done" }),
      task("t3", { assignee: "research-survey", status: "blocked" }),
      task("t4", { assignee: null, status: "running" }),
    ];
    const counts = countWorkload(tasks);
    expect(counts.get("coding-poc")).toEqual({ open: 1, total: 2 });
    expect(counts.get("research-survey")).toEqual({ open: 1, total: 1 });
    expect(counts.has("undefined")).toBe(false);
    expect(counts.size).toBe(2);
  });

  it("assignee が無いタスクは数えない", () => {
    expect(countWorkload([task("t1")]).size).toBe(0);
  });

  it("対話用タスク（conversation）は数えない（GUI-R3、Phase 27）", () => {
    const tasks = [
      task("chat", { assignee: "secretary", conversation: true, status: "running" }),
      task("work", { assignee: "secretary", conversation: false, status: "running" }),
    ];
    const counts = countWorkload(tasks);
    expect(counts.get("secretary")).toEqual({ open: 1, total: 1 });
  });
});

describe("tasksByAssignee", () => {
  it("assignee ごとにグループ化する", () => {
    const tasks = [
      task("t1", { assignee: "coding-poc" }),
      task("t2", { assignee: "coding-poc" }),
      task("t3", { assignee: "research-survey" }),
    ];
    const grouped = tasksByAssignee(tasks);
    expect(grouped.get("coding-poc")?.map((t) => t.id)).toEqual(["t1", "t2"]);
    expect(grouped.get("research-survey")?.map((t) => t.id)).toEqual(["t3"]);
  });

  it("assignee が無い・対話用のタスクは含めない", () => {
    const tasks = [task("t1", { assignee: null }), task("t2", { assignee: "coding-poc", conversation: true })];
    const grouped = tasksByAssignee(tasks);
    expect(grouped.size).toBe(0);
  });
});
