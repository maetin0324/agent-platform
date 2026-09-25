import type {
  Checkpoint,
  ExecutionMetrics,
  ExecutionPhase,
  ExecutionPlanOverview,
  ExecutionPlanVersionSummary,
  ExecutionView,
  ExecutionWorkUnitView,
  RunEnd,
  WorkUnitKind,
  WorkUnitStatus,
} from "~/celeris/types";
import type { Tone } from "~/components/ui/tone";

/**
 * タスク詳細の「実行」節（celeris ADR-0072 D19/D20、`TaskDetail.execution`）の表示用の純粋関数。
 * celeris が記録・集計した値をそのまま並べるだけで、GUI 側では判定・集計を再計算しない。
 */

export const EXECUTION_SECTION_LABEL = "実行";

const NONE = "—";

export const EXECUTION_PHASE_LABEL: Record<ExecutionPhase, string> = {
  planning: "計画中",
  executing: "実行中",
  repairing: "修復中",
  verifying: "検証中",
};

export const EXECUTION_PHASE_TONE: Record<ExecutionPhase, Tone> = {
  planning: "info",
  executing: "primary",
  repairing: "warning",
  verifying: "teal",
};

export const WORK_UNIT_STATUS_TONE: Record<WorkUnitStatus, Tone> = {
  pending: "neutral",
  ready: "info",
  needs_continuation: "warning",
  running: "primary",
  done: "success",
  failed: "danger",
  blocked: "warning",
  superseded: "neutral",
  cancelled: "neutral",
};

export const WORK_UNIT_KIND_LABEL: Record<WorkUnitKind, string> = {
  investigate: "調査",
  design: "設計",
  implement: "実装",
  test: "テスト",
  release: "リリース",
  repair: "修復",
  other: "その他",
};

const RUN_END_LABEL: Record<RunEnd["type"], string> = {
  completed: "completed",
  yielded: "yielded",
  budget_exhausted: "budget_exhausted",
  question: "question",
  failed: "failed",
  harness_error: "harness_error",
  cancelled: "cancelled",
};

export const RUN_END_TONE: Record<RunEnd["type"], Tone> = {
  completed: "success",
  yielded: "info",
  budget_exhausted: "warning",
  question: "info",
  failed: "danger",
  harness_error: "danger",
  cancelled: "neutral",
};

/** run の終わり方のバッジ文言（`budget_exhausted` は種類も添える）。無ければ `null`（導入前の run）。 */
export function runEndLabel(end: RunEnd | null | undefined): string | null {
  if (!end) return null;
  if (end.type === "budget_exhausted") return `budget_exhausted(${end.kind})`;
  return RUN_END_LABEL[end.type];
}

export function runEndTone(end: RunEnd | null | undefined): Tone {
  return end ? RUN_END_TONE[end.type] : "neutral";
}

/** D20: 計画の無いタスクの 1 行要約。 */
export function directExecutionSummary(metrics: ExecutionMetrics): string {
  const runs = metrics.runs_by_role?.worker ?? 0;
  return `直接実行（Run ${runs} 回、continuation ${metrics.continuations} 回）`;
}

/**
 * 今どの WorkUnit を見せるか（celeris `next_work_unit` の優先順位と同じ考え方:
 * running → needs_continuation → ready → blocked。すべて無ければ最後の done）。
 */
export function currentWorkUnit(plan: ExecutionPlanOverview): ExecutionWorkUnitView | null {
  const byStatus = (status: WorkUnitStatus): ExecutionWorkUnitView | null =>
    [...plan.work_units].filter((w) => w.status === status).sort((a, b) => a.seq - b.seq)[0] ?? null;
  return (
    byStatus("running") ??
    byStatus("needs_continuation") ??
    byStatus("ready") ??
    byStatus("blocked") ??
    [...plan.work_units].sort((a, b) => b.seq - a.seq)[0] ??
    null
  );
}

/** D20 の見出し: 「3 / 6 WorkUnits 完了 · 現在: <title> · Run #N · <model> · <harness> · <status>」。 */
export function planSummaryLine(plan: ExecutionPlanOverview, metrics: ExecutionMetrics): string {
  const done = metrics.work_units_done;
  const total = plan.work_units.length;
  const base = `${done} / ${total} WorkUnits 完了`;
  const current = currentWorkUnit(plan);
  if (!current) return base;
  return [
    base,
    `現在: ${current.title}`,
    `Run #${current.runs + 1}`,
    current.model ?? NONE,
    current.harness ?? NONE,
    current.status,
  ].join(" · ");
}

/** checkpoint の折り畳みの見出し（1 行要約）。 */
export function checkpointSummary(cp: Checkpoint | null | undefined): string {
  if (!cp) return "checkpoint なし";
  return `${cp.next_action}（remaining ${(cp.remaining ?? []).length} 件・completed ${(cp.completed ?? []).length} 件）`;
}

/** 版の履歴の 1 行（版・出自・状態・理由）。 */
export function planVersionLabel(v: ExecutionPlanVersionSummary): string {
  const reason = v.reason ? ` — ${v.reason}` : "";
  return `v${v.version}（${v.origin} / ${v.status}）${reason}`;
}

/** repair WU の印（D16「repair WU の印」）。 */
export function isRepairWorkUnit(wu: ExecutionWorkUnitView): boolean {
  return wu.kind === "repair";
}

/** gate の判定の 1 行（無ければ `null`）。 */
export function gateModeLabel(execution: ExecutionView | null | undefined): string | null {
  const gate = execution?.gate;
  if (!gate) return null;
  const shadow = gate.shadow ? "（shadow）" : "";
  return `${gate.mode}${shadow} — ${gate.rule_id}`;
}
