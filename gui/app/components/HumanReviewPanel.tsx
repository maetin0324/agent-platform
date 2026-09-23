import { Link, useFetcher } from "react-router";
import type { TransitionOutcome } from "~/celeris/action-types";
import type { ApprovalItem, CriterionView, ReviewNote } from "~/celeris/types";
import { TransitionFlash } from "~/components/Flash";
import { Badge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, textareaClass, touchLinkClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Mono } from "~/components/ui/misc";
import { cn } from "~/lib/utils";

/**
 * 人のレビュー待ち（`kind = approval` の承認待ち）の判断材料をタスク詳細にまとめて出す。
 * 材料は `GET /inbox` の `ApprovalItem`（受け入れ条件・直近 run の要約・証拠・成果物）と、
 * タスク詳細が持つ判定（`criteria` の直近判定、`prior_review`）。GUI では再計算しない。
 * 承認・却下は `/inbox` の action に投げる（承認タスク自身の詳細ページでも、レビュー対象の親でも同じ）。
 */
export function HumanReviewPanel({
  items,
  criteria,
  priorReview,
  reviewTaskId,
}: {
  items: ApprovalItem[];
  criteria: CriterionView[];
  priorReview: ReviewNote[];
  /** 判定の材料（成果物・変更）を持つタスク。承認タスク自身の詳細では親、親の詳細ではそれ自身。 */
  reviewTaskId: string;
}) {
  return (
    <section aria-labelledby="human-review-heading" data-testid="human-review-section">
      <Card>
        <CardHeader
          icon="checkCircle"
          tone="warning"
          title={
            <h2 id="human-review-heading" className="text-[0.95rem] font-semibold text-fg">
              人のレビュー待ち
            </h2>
          }
        />
        <CardBody className="space-y-4">
          {items.map((item) => (
            <HumanReviewItem
              key={item.approval.id}
              item={item}
              criteria={criteria}
              priorReview={priorReview}
              reviewTaskId={item.parent?.id ?? reviewTaskId}
            />
          ))}
        </CardBody>
      </Card>
    </section>
  );
}

function HumanReviewItem({
  item,
  criteria,
  priorReview,
  reviewTaskId,
}: {
  item: ApprovalItem;
  criteria: CriterionView[];
  priorReview: ReviewNote[];
  reviewTaskId: string;
}) {
  const fetcher = useFetcher<TransitionOutcome[]>({ key: `task-human-review-${item.approval.id}` });
  const submitting = fetcher.state !== "idle";
  const summary = item.last_run?.outcome_text;
  const failedNotes = priorReview.filter((n) => !n.pass);
  return (
    <div data-testid="human-review-item" className="space-y-3 text-sm">
      <p className="text-fg" data-testid="human-review-criterion">
        <span className="text-fg-muted">
          確認してほしい条件{item.criterion_idx != null ? ` #${item.criterion_idx}` : ""}:{" "}
        </span>
        {item.criterion_text}
      </p>

      {summary && (
        <div data-testid="human-review-summary">
          <p className="text-fg-muted">直近 run の要約</p>
          <p className="mt-1 max-h-64 overflow-y-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-surface-2/40 p-3 text-fg">
            {summary}
          </p>
        </div>
      )}

      {criteria.length > 0 && (
        <div data-testid="human-review-criteria">
          <p className="text-fg-muted">受け入れ条件ごとの判定</p>
          <ul className="mt-1 space-y-1.5">
            {criteria.map((c) => (
              <li key={c.idx} className="flex flex-wrap items-center gap-1.5">
                <Mono>#{c.idx}</Mono>
                <span className="text-fg">{c.text}</span>
                {c.check.type === "human" ? (
                  <Badge tone="warning">
                    {c.approval?.decided ? (c.approval.decided.approved ? "承認済み" : "却下済み") : "人の判断待ち"}
                  </Badge>
                ) : c.latest_verdict ? (
                  <Badge tone={c.latest_verdict.pass ? "success" : "danger"} dot>
                    {c.latest_verdict.pass ? "pass" : "fail"}
                  </Badge>
                ) : (
                  <Badge tone="neutral">未判定</Badge>
                )}
                {c.latest_verdict && (
                  <span className="w-full break-words text-fg-muted sm:w-auto">— {c.latest_verdict.reason}</span>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {failedNotes.length > 0 && (
        <div data-testid="human-review-prior">
          <p className="text-fg-muted">reviewer の指摘（前回までの不合格）</p>
          <ul className="mt-1 space-y-1">
            {failedNotes.map((n) => (
              <li key={`${n.criterion}-${n.reason}`} className="break-words text-fg-muted">
                <Mono>#{n.criterion}</Mono> {n.reason}
              </li>
            ))}
          </ul>
        </div>
      )}

      {item.evidence.length > 0 && (
        <div data-testid="human-review-evidence">
          <p className="text-fg-muted">証拠（実行したコマンド）</p>
          <ul className="mt-1 space-y-2">
            {item.evidence.map((e) => (
              <li key={`${e.criterion}-${e.command ?? ""}`} className="rounded-lg border border-border p-2">
                <p className="break-all font-mono text-xs text-fg">
                  #{e.criterion} {e.command ?? "-"}
                  {e.exit != null ? `（exit ${e.exit}）` : ""}
                </p>
                {e.stdout_tail && (
                  <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-all text-xs text-fg-muted">
                    {e.stdout_tail}
                  </pre>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div data-testid="human-review-artifacts">
        <p className="text-fg-muted">成果物（{item.artifacts.length} 件）</p>
        {item.artifacts.length > 0 && (
          <ul className="mt-1 space-y-0.5">
            {item.artifacts.map((a) => (
              <li key={`${a.name}-${a.sha256}`} className="break-all font-mono text-xs text-fg">
                {a.name}
              </li>
            ))}
          </ul>
        )}
        <div className="mt-2 flex flex-wrap gap-2">
          <Link
            to={`/tasks/${reviewTaskId}?tab=artifacts`}
            data-testid="human-review-artifacts-link"
            className={buttonClass({ variant: "secondary", size: "sm" })}
          >
            <Icon name="folder" />
            成果物の中身を見る
          </Link>
          <Link
            to={`/tasks/${reviewTaskId}?tab=changes`}
            data-testid="human-review-changes-link"
            className={buttonClass({ variant: "secondary", size: "sm" })}
          >
            <Icon name="gitBranch" />
            変更（差分）を見る
          </Link>
        </div>
      </div>

      {item.previous_decisions.length > 0 && (
        <p className="text-fg-muted" data-testid="human-review-previous">
          以前の判定: {item.previous_decisions.map((d) => (d.approved ? "承認" : "却下")).join(", ")}
        </p>
      )}

      <fetcher.Form method="post" action="/inbox" className="flex flex-col gap-2 border-t border-border pt-3">
        <input type="hidden" name="task_id" value={item.approval.id} />
        <input type="hidden" name="expected_status" value="ready" />
        <textarea
          name="note"
          aria-label="判定の note（任意）"
          data-testid="human-review-note"
          rows={2}
          placeholder="判定の note（任意）"
          className={textareaClass}
        />
        <p className={hintClass}>
          却下の note は次の run の prior_review に届きます。承認タスク:{" "}
          <Link to={`/tasks/${item.approval.id}`} className={cn(touchLinkClass, "text-primary hover:underline")}>
            {item.approval.id}
          </Link>
        </p>
        <div className="flex gap-2">
          <Button
            type="submit"
            name="intent"
            value="approve"
            variant="success"
            size="sm"
            disabled={submitting}
            data-testid="human-review-approve"
          >
            <Icon name="check" />
            承認
          </Button>
          <Button
            type="submit"
            name="intent"
            value="reject"
            variant="danger"
            size="sm"
            disabled={submitting}
            data-testid="human-review-reject"
          >
            <Icon name="x" />
            却下
          </Button>
        </div>
      </fetcher.Form>
      {fetcher.data?.map((o) => (
        <TransitionFlash key={`${o.taskId}-${o.intent}`} outcome={o} />
      ))}
    </div>
  );
}
