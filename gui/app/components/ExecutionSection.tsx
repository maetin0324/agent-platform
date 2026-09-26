import type { ExecutionView, ExecutionWorkUnitView } from "~/celeris/types";
import { Badge, RoleLabel } from "~/components/ui/badge";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { tableClass, tdClass, thClass, theadClass } from "~/components/ui/form";
import { Mono } from "~/components/ui/misc";
import {
  checkpointSummary as checkpointSummaryLine,
  costReferenceLabel,
  currentWorkUnit,
  directExecutionSummary,
  EXECUTION_SECTION_LABEL,
  gateModeLabel,
  isRepairWorkUnit,
  parallelSummaryLine,
  planSummaryLine,
  planVersionLabel,
  quotaSummaryLines,
  shortCommit,
  WORK_UNIT_KIND_LABEL,
  WORK_UNIT_STATUS_TONE,
  workUnitGroups,
} from "~/lib/task-execution";
import { cn } from "~/lib/utils";

/**
 * タスク詳細の「実行」節（celeris ADR-0072 D19/D20）。計画の無い Task は「直接実行」の 1 行だけ、
 * 計画のある Task は WU の表・版の履歴（replan）を出す。`detail.execution` が `null`（events に
 * E-phase の活動が無い古いタスク）なら何も描かない（D23 の後方互換）。
 */
export function ExecutionSection({ execution }: { execution: ExecutionView | null | undefined }) {
  if (!execution) return null;
  const { plan, metrics } = execution;
  const gate = gateModeLabel(execution);
  // ADR-0074 D4（Phase F3 quota）: quota が主指標、定価 USD は参考（GUI (j)）。
  const quotaLines = quotaSummaryLines(metrics);
  const costLabel = costReferenceLabel(metrics);

  return (
    <section aria-labelledby="execution-heading" data-testid="execution-section">
      <Card>
        <CardHeader
          icon="zap"
          tone="primary"
          title={
            <h2 id="execution-heading" className="text-[0.95rem] font-semibold text-fg">
              {EXECUTION_SECTION_LABEL}
            </h2>
          }
          description={
            <span data-testid="execution-summary">
              {plan ? planSummaryLine(plan, metrics) : directExecutionSummary(metrics)}
            </span>
          }
        />
        <CardBody className="space-y-4">
          {gate && (
            <p className="text-sm text-fg-muted" data-testid="execution-gate">
              gate: {gate}
            </p>
          )}
          {(quotaLines.length > 0 || costLabel) && (
            <div data-testid="execution-quota" className="text-sm text-fg-muted">
              <p className="font-medium text-fg-subtle lg:text-xs">quota 消費</p>
              {quotaLines.length > 0 ? (
                <ul className="mt-1 space-y-0.5">
                  {quotaLines.map((line) => (
                    <li key={line} data-testid="execution-quota-line">
                      {line}
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="mt-1 text-fg-subtle" data-testid="execution-quota-none">
                  quota の記録はありません。
                </p>
              )}
              {costLabel && (
                <p className="mt-1 text-fg-subtle" data-testid="execution-cost-reference">
                  {costLabel}
                </p>
              )}
            </div>
          )}
          {!plan ? (
            <p className="text-sm text-fg-muted" data-testid="execution-no-plan">
              計画はありません（暗黙の WorkUnit で直接実行）。
            </p>
          ) : (
            <>
              {parallelSummaryLine(plan) && (
                <p className="text-sm text-fg-muted" data-testid="execution-parallel">
                  {parallelSummaryLine(plan)}
                </p>
              )}
              {/* ADR-0072 D20（Phase E5）: モバイル幅（393px）は表ではなくカードの一覧に折り返す
                  （`max-sm:`。`~/routes/projects.tsx` の案件一覧と同じ技法）。表のまま横スクロールさせると、
                  この中の `<details>`（checkpoint の折り畳み）へのキーボードフォーカスがブラウザの
                  ネイティブな「要素を可視領域へ」で横スクロールを動かし、タッチのスワイプ検査
                  （`mobile-audit` の `touch-scroll`）と競合する。カードにすれば横スクロール自体が無い。 */}
              <div className="overflow-x-auto sm:rounded-lg sm:border sm:border-border">
                <table className={cn(tableClass, "max-sm:block")} data-testid="work-unit-table">
                  <thead className={cn(theadClass, "max-sm:hidden")}>
                    <tr>
                      <th className={thClass}>key</th>
                      <th className={thClass}>工程</th>
                      <th className={thClass}>title</th>
                      <th className={thClass}>status</th>
                      <th className={thClass}>依存</th>
                      <th className={thClass}>担当</th>
                      <th className={thClass}>harness</th>
                      <th className={thClass}>model / lane</th>
                      <th className={thClass}>run</th>
                      <th className={thClass}>continuation</th>
                      <th className={thClass}>retry</th>
                      <th className={thClass}>checkpoint / 理由</th>
                    </tr>
                  </thead>
                  {/* ADR-0074 D1（Phase F2b）: v2 の計画は工程ごとに見出しを付けてまとめる。 */}
                  {workUnitGroups(plan).map((group) => (
                    <tbody key={group.key || "all"} className="max-sm:block" data-testid="work-unit-group">
                      {group.label && (
                        <tr className="max-sm:block" data-testid="work-unit-phase-heading">
                          <th
                            colSpan={12}
                            scope="colgroup"
                            className={cn(
                              thClass,
                              "bg-surface-2/60 text-left max-sm:block max-sm:border-t max-sm:border-border max-sm:px-3",
                            )}
                          >
                            {group.label}
                          </th>
                        </tr>
                      )}
                      {group.units.map((wu) => (
                        <WorkUnitRow key={wu.id} wu={wu} isCurrent={currentWorkUnit(plan)?.id === wu.id} />
                      ))}
                    </tbody>
                  ))}
                </table>
              </div>

              {plan.versions.length > 1 && (
                <div data-testid="execution-plan-versions">
                  <p className="text-sm font-medium text-fg-subtle lg:text-xs">計画の版（replan の履歴）</p>
                  <ul className="mt-1 space-y-0.5 text-sm text-fg">
                    {plan.versions.map((v) => (
                      <li key={v.id} data-testid="execution-plan-version" className="break-words">
                        {planVersionLabel(v)}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </>
          )}
        </CardBody>
      </Card>
    </section>
  );
}

/** モバイルのカード表示で、値の前に付ける列名（`~/routes/projects.tsx` の案件一覧と同じ技法）。 */
function CellLabel({ children }: { children: string }) {
  return <span className="text-fg-subtle sm:hidden">{children}: </span>;
}

const cardCellClass =
  "max-sm:block max-sm:border-0 max-sm:px-1 max-sm:first:col-span-2 max-sm:first:pl-1 max-sm:last:col-span-2 max-sm:last:pr-1 max-sm:break-words";

function WorkUnitRow({ wu, isCurrent }: { wu: ExecutionWorkUnitView; isCurrent: boolean }) {
  const repair = isRepairWorkUnit(wu);
  return (
    <tr
      data-testid="work-unit-row"
      className={cn(
        "max-sm:grid max-sm:grid-cols-2 max-sm:border-t max-sm:border-border max-sm:p-3",
        isCurrent && "bg-surface-2/60",
      )}
    >
      <td className={cn(tdClass, "font-mono text-xs", cardCellClass)}>
        <Mono>{wu.key}</Mono>
        {repair && (
          <Badge tone="warning" className="ml-1.5" data-testid="work-unit-repair-badge">
            repair
          </Badge>
        )}
        {wu.branch && (
          <span className="mt-0.5 block break-all text-fg-subtle" data-testid="work-unit-branch">
            <CellLabel>ブランチ</CellLabel>
            <Mono>{wu.branch}</Mono>
            {shortCommit(wu.head_commit ?? wu.integrated_commit) && (
              <span className="ml-1">@{shortCommit(wu.head_commit ?? wu.integrated_commit)}</span>
            )}
          </span>
        )}
      </td>
      <td className={cn(tdClass, cardCellClass)} data-testid="work-unit-phase">
        <CellLabel>工程</CellLabel>
        {wu.phase ?? "-"}
      </td>
      <td className={cn(tdClass, cardCellClass)}>
        <span className="break-words">{wu.title}</span>
        <RoleLabel role={WORK_UNIT_KIND_LABEL[wu.kind] ?? wu.kind} className="ml-1.5" />
      </td>
      <td className={cn(tdClass, cardCellClass)}>
        <CellLabel>status</CellLabel>
        <Badge tone={WORK_UNIT_STATUS_TONE[wu.status]} dot data-testid="work-unit-status">
          {wu.status}
        </Badge>
        {wu.blocked_reason && <span className="ml-1.5 text-fg-subtle">（{wu.blocked_reason}）</span>}
        {wu.running_run_id && (
          <span className="mt-0.5 block break-all text-fg-subtle" data-testid="work-unit-running-run">
            run <Mono>{wu.running_run_id}</Mono>
          </span>
        )}
      </td>
      <td className={cn(tdClass, cardCellClass)}>
        <CellLabel>依存</CellLabel>
        {wu.depends_on.length > 0 ? wu.depends_on.join(", ") : "-"}
      </td>
      <td className={cn(tdClass, cardCellClass)}>
        <CellLabel>担当</CellLabel>
        {wu.assignee ?? "-"}
      </td>
      <td className={cn(tdClass, cardCellClass)}>
        <CellLabel>harness</CellLabel>
        {wu.harness ?? "-"}
      </td>
      <td className={cn(tdClass, cardCellClass)}>
        <CellLabel>model / lane</CellLabel>
        {[wu.model, wu.lane].filter(Boolean).join(" / ") || "-"}
      </td>
      <td className={cn(tdClass, "tabular-nums", cardCellClass)}>
        <CellLabel>run</CellLabel>
        {wu.runs}
      </td>
      <td className={cn(tdClass, "tabular-nums", cardCellClass)}>
        <CellLabel>continuation</CellLabel>
        {wu.continuations}
      </td>
      <td className={cn(tdClass, "tabular-nums", cardCellClass)}>
        <CellLabel>retry</CellLabel>
        {wu.retries}
      </td>
      <td className={cn(tdClass, cardCellClass)}>
        {/* ADR-0055 D1-4: 本文 14px 以上。モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
        <details data-testid="work-unit-checkpoint" className="text-sm text-fg-muted lg:text-xs">
          <summary className="cursor-pointer select-none break-words">
            {checkpointSummaryLine(wu.last_checkpoint)}
          </summary>
          {wu.last_checkpoint && (
            <pre className="mt-1 max-h-48 overflow-y-auto whitespace-pre-wrap break-words rounded-md bg-surface-2 p-2 text-[0.7rem]">
              {JSON.stringify(wu.last_checkpoint, null, 2)}
            </pre>
          )}
        </details>
        {wu.last_reason && (
          <p className="mt-1 break-words text-sm text-fg-muted lg:text-xs" data-testid="work-unit-last-reason">
            {wu.last_reason}
          </p>
        )}
      </td>
    </tr>
  );
}
