import { describe, expect, it } from "vitest";
import { buildOrgTree, countWorkload, flattenProjectTasks } from "~/lib/org-tree";
import type { OrgNode, Project, ProjectTaskView } from "~/taskd/types";

/**
 * `buildOrgTree` / `countWorkload` / `flattenProjectTasks`（`/org` の loader が使う純粋関数）のテスト。
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
});

const projectTask = (id: string, over: Partial<ProjectTaskView> = {}): ProjectTaskView => ({
  id,
  title: id,
  status: "running",
  parent_id: null,
  depends_on: [],
  assignee: null,
  milestone_id: null,
  ...over,
});

const project = (id: string, title = id): Project => ({
  id,
  title,
  request: "…",
  status: "active",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
});

describe("flattenProjectTasks", () => {
  it("各案件の tasks に project_id / project_title を添えて 1 本の配列にする", () => {
    const flat = flattenProjectTasks([
      { project: project("p1", "Pluvio"), tasks: [projectTask("t1"), projectTask("t2")] },
      { project: project("p2", "Other"), tasks: [projectTask("t3")] },
    ]);
    expect(flat).toEqual([
      { ...projectTask("t1"), project_id: "p1", project_title: "Pluvio" },
      { ...projectTask("t2"), project_id: "p1", project_title: "Pluvio" },
      { ...projectTask("t3"), project_id: "p2", project_title: "Other" },
    ]);
  });
});

describe("countWorkload", () => {
  it("assignee ごとに、終端でない（open）ものと全体（total）を数える", () => {
    const tasks = flattenProjectTasks([
      {
        project: project("p1"),
        tasks: [
          projectTask("t1", { assignee: "coding-poc", status: "running" }),
          projectTask("t2", { assignee: "coding-poc", status: "done" }),
          projectTask("t3", { assignee: "research-survey", status: "blocked" }),
          projectTask("t4", { assignee: null, status: "running" }),
        ],
      },
    ]);
    const counts = countWorkload(tasks);
    expect(counts.get("coding-poc")).toEqual({ open: 1, total: 2 });
    expect(counts.get("research-survey")).toEqual({ open: 1, total: 1 });
    expect(counts.has("undefined")).toBe(false);
    expect(counts.size).toBe(2);
  });

  it("assignee が無いタスクは数えない", () => {
    const tasks = flattenProjectTasks([{ project: project("p1"), tasks: [projectTask("t1")] }]);
    expect(countWorkload(tasks).size).toBe(0);
  });
});
