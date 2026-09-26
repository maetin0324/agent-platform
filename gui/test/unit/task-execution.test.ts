import { describe, expect, it } from "vitest";
import type {
  ExecutionMetrics,
  ExecutionPlanOverview,
  ExecutionView,
  ExecutionWorkUnitView,
  QuotaUse,
} from "~/celeris/types";
import {
  costReferenceLabel,
  currentWorkUnit,
  directExecutionSummary,
  gateModeLabel,
  isRepairWorkUnit,
  planSummaryLine,
  planVersionLabel,
  quotaSummaryLines,
  runEndLabel,
  runEndTone,
} from "~/lib/task-execution";

/**
 * `~/lib/task-execution.ts`（celeris ADR-0072 D19/D20 の「実行」節）の純粋関数。
 * D20 が受け入れ条件 (b) に挙げる 4 つの状況を fixture にする: 計画なし・計画あり・replan あり・repair あり。
 */

function metrics(over: Partial<ExecutionMetrics> = {}): ExecutionMetrics {
  return {
    continuations: 0,
    max_turn_failures: 0,
    replans: 0,
    repairs_total: 0,
    retries: 0,
    work_units_done: 0,
    work_units_total: 0,
    final_status: "running",
    ...over,
  };
}

function wu(over: Partial<ExecutionWorkUnitView> = {}): ExecutionWorkUnitView {
  return {
    id: "wu-1",
    key: "a",
    seq: 0,
    kind: "implement",
    title: "実装する",
    status: "ready",
    depends_on: [],
    runs: 0,
    continuations: 0,
    retries: 0,
    created_at: "2026-09-25T00:00:00Z",
    updated_at: "2026-09-25T00:00:00Z",
    ...over,
  };
}

function plan(over: Partial<ExecutionPlanOverview> = {}): ExecutionPlanOverview {
  return {
    id: "plan-1",
    version: 1,
    origin: "planner",
    rationale: "調査してから実装する",
    work_units: [],
    versions: [],
    ...over,
  };
}

// ---- fixture 1: 計画なし（直接実行） ----
const NO_PLAN: ExecutionView = {
  gate: {
    mode: "atomic",
    source: "policy",
    score: 1,
    threshold: 5,
    rule_id: "atomic/score",
    policy_version: "exec-gate/1",
    shadow: false,
  },
  phase: null,
  plan: null,
  metrics: metrics({ runs_by_role: { worker: 2 }, continuations: 1, gate_mode: "atomic" }),
};

// ---- fixture 2: 計画あり（3 WU、1 完了・1 実行中） ----
const WITH_PLAN: ExecutionView = {
  gate: {
    mode: "compound",
    source: "policy",
    score: 6,
    threshold: 5,
    rule_id: "compound/score",
    policy_version: "exec-gate/1",
    shadow: false,
  },
  phase: "executing",
  plan: plan({
    work_units: [
      wu({ id: "wu-a", key: "survey", seq: 0, kind: "investigate", title: "調査", status: "done" }),
      wu({
        id: "wu-b",
        key: "build",
        seq: 1,
        kind: "implement",
        title: "実装",
        status: "running",
        depends_on: ["survey"],
        runs: 1,
        model: "model-std",
        harness: "coding",
      }),
      wu({ id: "wu-c", key: "test", seq: 2, kind: "test", title: "検証", status: "pending", depends_on: ["build"] }),
    ],
  }),
  metrics: metrics({ work_units_total: 3, work_units_done: 1, gate_mode: "compound" }),
};

// ---- fixture 3: replan あり（版の履歴が 2 件） ----
const WITH_REPLAN: ExecutionView = {
  ...WITH_PLAN,
  plan: plan({
    version: 2,
    work_units: WITH_PLAN.plan?.work_units ?? [],
    versions: [
      { id: "plan-1", version: 1, origin: "planner", status: "superseded", created_at: "2026-09-25T00:00:00Z" },
      {
        id: "plan-2",
        version: 2,
        origin: "human",
        status: "active",
        reason: "add a follow-up step",
        created_at: "2026-09-25T01:00:00Z",
      },
    ],
  }),
  metrics: metrics({ work_units_total: 4, work_units_done: 1, replans: 1, gate_mode: "compound" }),
};

// ---- fixture 4: repair あり ----
const WITH_REPAIR: ExecutionView = {
  ...WITH_PLAN,
  plan: plan({
    work_units: [
      ...(WITH_PLAN.plan?.work_units ?? []),
      wu({
        id: "wu-repair-1",
        key: "repair-1",
        seq: 3,
        kind: "repair",
        title: "repair (format): 修復",
        status: "ready",
      }),
    ],
  }),
  metrics: metrics({
    work_units_total: 4,
    work_units_done: 1,
    repairs_total: 1,
    repairs_by_class: { format: 1 },
    gate_mode: "compound",
  }),
};

describe("task-execution", () => {
  it("計画なし: 直接実行の 1 行要約", () => {
    expect(directExecutionSummary(NO_PLAN.metrics)).toBe("直接実行（Run 2 回、continuation 1 回）");
    expect(NO_PLAN.plan).toBeNull();
    expect(gateModeLabel(NO_PLAN)).toBe("atomic — atomic/score");
  });

  it("計画あり: 現在の WU は running が優先、見出しに Run 番号・model・harness・status が並ぶ", () => {
    const p = WITH_PLAN.plan;
    expect(p).not.toBeNull();
    if (!p) return;
    const current = currentWorkUnit(p);
    expect(current?.key).toBe("build");
    expect(planSummaryLine(p, WITH_PLAN.metrics)).toBe(
      "1 / 3 WorkUnits 完了 · 現在: 実装 · Run #2 · model-std · coding · running",
    );
  });

  it("replan あり: 版の履歴に 2 件、理由付き", () => {
    const p = WITH_REPLAN.plan;
    expect(p).not.toBeNull();
    if (!p) return;
    expect(p.versions).toHaveLength(2);
    expect(planVersionLabel(p.versions[0])).toBe("v1（planner / superseded）");
    expect(planVersionLabel(p.versions[1])).toBe("v2（human / active） — add a follow-up step");
  });

  it("repair あり: repair WU が kind で見分けられる", () => {
    const p = WITH_REPAIR.plan;
    expect(p).not.toBeNull();
    if (!p) return;
    const repairUnit = p.work_units.find((w) => w.key === "repair-1");
    expect(repairUnit).toBeDefined();
    if (repairUnit) {
      expect(isRepairWorkUnit(repairUnit)).toBe(true);
    }
    expect(p.work_units.filter((w) => !isRepairWorkUnit(w)).every((w) => w.key !== "repair-1")).toBe(true);
    expect(WITH_REPAIR.metrics.repairs_by_class?.format).toBe(1);
  });

  it("run の end はバッジ文言とトーンを持つ（budget_exhausted は種類も添える）", () => {
    expect(runEndLabel({ type: "completed" })).toBe("completed");
    expect(runEndLabel({ type: "budget_exhausted", kind: "turns" })).toBe("budget_exhausted(turns)");
    expect(runEndLabel(null)).toBeNull();
    expect(runEndTone({ type: "failed", retryable: false })).toBe("danger");
    expect(runEndTone(null)).toBe("neutral");
  });

  // ---- ADR-0074 D4（Phase F3 quota）: quota が主、定価 USD は参考 ----

  function quotaRow(over: Partial<QuotaUse> = {}): QuotaUse {
    return {
      source: "claude-oauth",
      account: "a",
      window: "five_hour",
      used_pct: 4.0,
      runs: 1,
      method_counts: { measured: 1 },
      ...over,
    };
  }

  it("quota: measured の行は「実測」、値は 1 桁小数 + pt", () => {
    const lines = quotaSummaryLines(metrics({ quota: [quotaRow()] }));
    expect(lines).toEqual(["claude-oauth（a） 5h 4.0pt（実測）"]);
  });

  it("quota: unknown だけの行は「不明」であって「0」ではない", () => {
    const lines = quotaSummaryLines(
      metrics({
        quota: [quotaRow({ used_pct: null, method_counts: { unknown: 1 }, window: "seven_day" })],
      }),
    );
    expect(lines).toEqual(["claude-oauth（a） 7d 不明（一部不明）"]);
  });

  it("quota: estimated/apportioned/free もそれぞれの一言になる", () => {
    expect(quotaSummaryLines(metrics({ quota: [quotaRow({ method_counts: { estimated: 1 } })] }))).toEqual([
      "claude-oauth（a） 5h 4.0pt（推定）",
    ]);
    expect(quotaSummaryLines(metrics({ quota: [quotaRow({ method_counts: { apportioned: 1 } })] }))).toEqual([
      "claude-oauth（a） 5h 4.0pt（按分）",
    ]);
    expect(
      quotaSummaryLines(
        metrics({ quota: [quotaRow({ account: undefined, used_pct: 0, method_counts: { free: 1 } })] }),
      ),
    ).toEqual(["claude-oauth 5h 0.0pt（無料）"]);
  });

  it("quota: 記録が無ければ空配列", () => {
    expect(quotaSummaryLines(metrics())).toEqual([]);
    expect(quotaSummaryLines(metrics({ quota: [] }))).toEqual([]);
  });

  it("定価 USD: 参考値として、不完全なら明示する", () => {
    expect(costReferenceLabel(metrics({ cost_usd: 11.21 }))).toBe("参考 $11.21");
    expect(costReferenceLabel(metrics({ cost_usd: 11.21, cost_usd_complete: false }))).toBe(
      "参考 $11.21（一部のモデルの単価が不明なため過小）",
    );
    expect(costReferenceLabel(metrics())).toBeNull();
  });
});
