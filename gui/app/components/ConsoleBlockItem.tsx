import { type FormEvent, type ReactNode, useState } from "react";
import { Link, useFetcher } from "react-router";
import type {
  ApprovalOpOutcome,
  ProjectOpOutcome,
  TaskCommentOutcome,
  TransitionOutcome,
} from "~/celeris/action-types";
import type { ConsoleBlock, EventsPage, OrgNode, Project } from "~/celeris/types";
import { formatRunEventRow, progressSummaryLine, taskLineSummary } from "~/lib/console";
import { isKnowledgeFallback } from "~/lib/knowledge";
import { milestoneDecisionValid } from "~/lib/milestone-review";
import { cn } from "~/lib/utils";
import { MarkdownViewer } from "./MarkdownViewer";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { hintClass, textareaClass, touchLinkClass } from "./ui/form";
import { Icon } from "./ui/Icon";

/**
 * Console（ADR-0048 D1/D4、GUI Phase G22）の 1 ブロック。`kind` ごとに 1 分岐（8 種 + 予約の `knowledge`）。
 * **新しい状態変更のロジックはここに書かない**: 認可・途中目標・タスクへのコメント／回答は、既存の
 * 画面（`/approvals`・`/projects/:id`・`/tasks/:id`）の action をそのまま `fetcher.Form action="/…"` で叩く
 * （React Router のクロスルート fetcher。`~/components/NotificationsWatcher.tsx` / `~/routes/inbox.tsx` の
 * `retryFetcher.Form action={`/tasks/${id}`}` と同じ作法）。celeris への要求の形・検証は呼び先の action が持つ。
 */

export function orgNodeName(id: string, org: readonly OrgNode[]): string {
  return org.find((n) => n.id === id)?.name ?? id;
}

export function projectName(id: string | null | undefined, projects: readonly Project[]): string | null {
  if (!id) return null;
  return projects.find((p) => p.id === id)?.title ?? id;
}

export function ConsoleBlockItem({
  block,
  org,
  projects,
  onReplyToConversation,
}: {
  block: ConsoleBlock;
  org: readonly OrgNode[];
  projects: readonly Project[];
  onReplyToConversation: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
}) {
  switch (block.kind) {
    case "human":
      return <HumanBlockView block={block} org={org} onReply={onReplyToConversation} />;
    case "reply":
      return <ReplyBlockView block={block} org={org} onReply={onReplyToConversation} />;
    case "task":
      return <TaskBlockView block={block} org={org} projects={projects} />;
    case "progress":
      return <ProgressBlockView block={block} />;
    case "question":
      return <QuestionBlockView block={block} org={org} />;
    case "approval":
      return <ApprovalBlockView block={block} org={org} projects={projects} />;
    case "milestone":
      return <MilestoneBlockView block={block} projects={projects} />;
    case "report":
      return <ReportBlockView block={block} org={org} projects={projects} />;
    case "knowledge":
      return <KnowledgeBlockView block={block} />;
    default:
      return null;
  }
}

function BlockShell({
  testId,
  align = "start",
  className,
  children,
}: {
  testId: string;
  align?: "start" | "end";
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      data-testid={testId}
      data-console-block={testId}
      className={cn("flex", align === "end" ? "justify-end" : "justify-start")}
    >
      <div
        className={cn(
          "max-w-[46rem] min-w-0 rounded-xl border border-border bg-surface px-3 py-2 text-sm shadow-xs",
          align === "end" && "bg-primary-soft text-primary-soft-fg border-transparent",
          className,
        )}
      >
        {children}
      </div>
    </div>
  );
}

function ReplyButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      data-testid="console-reply-button"
      className="inline-flex min-h-11 items-center gap-1 px-2 text-sm text-fg-subtle underline underline-offset-2 hover:text-fg"
    >
      <Icon name="send" className="size-3" />
      返信
    </button>
  );
}

function HumanBlockView({
  block,
  org,
  onReply,
}: {
  block: Extract<ConsoleBlock, { kind: "human" }>;
  org: readonly OrgNode[];
  onReply: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
}) {
  return (
    <BlockShell testId="console-block-human" align="end">
      <p className="whitespace-pre-wrap">{block.text}</p>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-[0.7rem] のまま。 */}
      <div className="mt-1 flex items-center justify-between gap-2 text-sm opacity-80 lg:text-[0.7rem]">
        <span>
          {orgNodeName(block.node_id, org)} へ ・ {block.at}
        </span>
        <ReplyButton onClick={() => onReply(block)} />
      </div>
    </BlockShell>
  );
}

function ReplyBlockView({
  block,
  org,
  onReply,
}: {
  block: Extract<ConsoleBlock, { kind: "reply" }>;
  org: readonly OrgNode[];
  onReply: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
}) {
  const result = block.actions_result;
  return (
    <BlockShell testId="console-block-reply">
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <div className="mb-1 text-sm font-medium text-fg-subtle lg:text-xs">{orgNodeName(block.node_id, org)}</div>
      <MarkdownViewer content={block.text} />
      {result && (result.actions_executed?.length || result.actions_failed?.length) ? (
        <div className="mt-2 space-y-1 text-sm lg:text-xs" data-testid="console-actions-result">
          {result.actions_executed?.map((a, i) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: `actions_executed` はサーバの応答そのままで id を持たない
            <p key={i} className="text-success" data-testid="console-action-executed">
              {a.summary}
            </p>
          ))}
          {result.actions_failed?.map((a, i) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: `actions_failed` はサーバの応答そのままで id を持たない
            <p key={i} className="text-danger" data-testid="console-action-failed">
              実行できなかった action（{a.kind}）: {a.reason}
            </p>
          ))}
        </div>
      ) : null}
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-[0.7rem] のまま。 */}
      <div className="mt-1.5 flex flex-wrap items-center justify-between gap-2 text-sm text-fg-subtle lg:text-[0.7rem]">
        <span className="flex items-center gap-2">
          <span>{block.at}</span>
          {block.run_id && block.task_id && (
            <Link to={`/tasks/${block.task_id}`} className={cn(touchLinkClass, "underline underline-offset-2")}>
              この返事を作った run
            </Link>
          )}
        </span>
        <ReplyButton onClick={() => onReply(block)} />
      </div>
    </BlockShell>
  );
}

function TaskBlockView({
  block,
  org,
  projects,
}: {
  block: Extract<ConsoleBlock, { kind: "task" }>;
  org: readonly OrgNode[];
  projects: readonly Project[];
}) {
  const [commenting, setCommenting] = useState(false);
  const [body, setBody] = useState("");
  const fetcher = useFetcher<TaskCommentOutcome>({ key: `console-task-comment-${block.task.task_id}` });
  const submitting = fetcher.state !== "idle";
  const t = block.task;
  const projName = projectName(t.project_id, projects);

  return (
    <BlockShell testId="console-block-task" className="w-full max-w-none">
      <div className="flex flex-wrap items-center gap-2">
        <Icon name="activity" className="size-3.5 text-fg-subtle" />
        <Link to={`/tasks/${t.task_id}`} className={cn(touchLinkClass, "font-medium underline underline-offset-2")}>
          {t.title}
        </Link>
        {projName && <Badge tone="neutral">{projName}</Badge>}
      </div>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="mt-1 text-sm text-fg-subtle lg:text-xs" data-testid="console-task-summary">
        {taskLineSummary(t)}
        {t.assignee && <> ・ {orgNodeName(t.assignee, org)}</>}
      </p>
      <div className="mt-1.5 flex flex-wrap items-center justify-between gap-1 text-sm text-fg-subtle lg:text-[0.7rem]">
        <span>{block.at}</span>
        <ReplyButton onClick={() => setCommenting((v) => !v)} />
      </div>
      {commenting && (
        <fetcher.Form
          method="post"
          action={`/tasks/${t.task_id}`}
          className="mt-2 space-y-2 border-t border-border pt-2"
          onSubmit={() => {
            setCommenting(false);
            setBody("");
          }}
        >
          <input type="hidden" name="intent" value="comment" />
          <textarea
            name="body"
            rows={2}
            value={body}
            onChange={(e) => setBody(e.target.value)}
            placeholder="このタスクへのコメント（走っていれば割り込みます）"
            data-testid="console-task-comment-input"
            className={cn(textareaClass, "w-full")}
          />
          <Button
            type="submit"
            size="xs"
            variant="secondary"
            disabled={submitting}
            data-testid="console-task-comment-send"
          >
            送る
          </Button>
        </fetcher.Form>
      )}
    </BlockShell>
  );
}

function ProgressBlockView({ block }: { block: Extract<ConsoleBlock, { kind: "progress" }> }) {
  const [expanded, setExpanded] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const eventsFetcher = useFetcher<EventsPage>();
  const p = block.progress;

  function toggleAll() {
    const next = !showAll;
    setShowAll(next);
    if (next && eventsFetcher.state === "idle" && !eventsFetcher.data) {
      eventsFetcher.load(`/tasks/${p.task_id}/runs/${p.run_id}/events`);
    }
  }

  return (
    <BlockShell testId="console-block-progress" className="w-full max-w-none">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        data-testid="console-progress-toggle"
        className="flex min-h-11 w-full items-center gap-2 text-left"
      >
        <Icon name={expanded ? "chevronDown" : "chevronRight"} className="size-3.5 shrink-0 text-fg-subtle" />
        <span className="min-w-0 flex-1">
          <span className="font-medium">{block.title}</span>
          {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
          <span className="ml-2 text-sm text-fg-subtle lg:text-xs">{progressSummaryLine(block)}</span>
        </span>
      </button>
      {expanded && (
        <div className="mt-2 space-y-1 border-t border-border pt-2" data-testid="console-progress-detail">
          {p.first.map((line) => (
            <ProgressLineRow
              key={`first-${line.seq}`}
              at={line.at}
              text={line.text}
              tool={line.tool}
              error={line.error}
            />
          ))}
          {p.truncated && <p className={hintClass}>…（省略）…</p>}
          {p.last.map((line) => (
            <ProgressLineRow
              key={`last-${line.seq}`}
              at={line.at}
              text={line.text}
              tool={line.tool}
              error={line.error}
            />
          ))}
          <button
            type="button"
            onClick={toggleAll}
            data-testid="console-progress-show-all"
            className={cn(
              touchLinkClass,
              "text-sm text-fg-subtle underline underline-offset-2 hover:text-fg lg:text-xs",
            )}
          >
            {showAll ? "閉じる" : `すべて見る（${p.count} 件）`}
          </button>
          {showAll && (
            <div className="space-y-1 rounded-lg bg-surface-2 p-2" data-testid="console-progress-all">
              {eventsFetcher.state !== "idle" && !eventsFetcher.data && <p className={hintClass}>読み込み中…</p>}
              {eventsFetcher.data?.items.map((row) => {
                const line = formatRunEventRow(row);
                return (
                  <ProgressLineRow
                    key={line.seq}
                    at={line.at}
                    text={line.label}
                    error={line.error}
                    detail={line.detail}
                  />
                );
              })}
            </div>
          )}
        </div>
      )}
    </BlockShell>
  );
}

function ProgressLineRow({
  at,
  text,
  tool,
  error,
  detail,
}: {
  at: string;
  text: string;
  tool?: string | null;
  error?: boolean;
  detail?: string | null;
}) {
  return (
    <div
      data-testid="console-progress-line"
      className={cn(
        "rounded-md px-2 py-1 font-mono text-xs",
        error ? "bg-danger-soft text-danger-soft-fg" : "bg-surface-2/60",
      )}
    >
      <span className="text-fg-subtle">{at}</span> {tool && <span className="text-fg-subtle">[{tool}]</span>} {text}
      {error && (
        <Badge tone="danger" className="ml-2">
          エラー
        </Badge>
      )}
      {detail && (
        <details className="mt-1">
          <summary className="cursor-pointer text-fg-subtle">詳細</summary>
          <pre className="mt-1 whitespace-pre-wrap break-words text-fg-muted">{detail}</pre>
        </details>
      )}
    </div>
  );
}

function QuestionBlockView({
  block,
  org,
}: {
  block: Extract<ConsoleBlock, { kind: "question" }>;
  org: readonly OrgNode[];
}) {
  const fetcher = useFetcher<TransitionOutcome>({ key: `console-question-${block.task_id}` });
  const submitting = fetcher.state !== "idle";
  return (
    <BlockShell testId="console-block-question" className="w-full max-w-none border-warning-border bg-warning-soft/40">
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="text-sm font-medium text-fg-subtle lg:text-xs">
        {block.node_id ? orgNodeName(block.node_id, org) : "-"} からの質問
      </p>
      <p className="mt-1" data-testid="console-question-text">
        {block.text}
      </p>
      {block.answered ? (
        <p className="mt-2 text-sm text-fg-muted" data-testid="console-question-answer">
          回答: {block.answer}
        </p>
      ) : (
        <fetcher.Form method="post" action={`/tasks/${block.task_id}`} className="mt-2 flex flex-col gap-2">
          <input type="hidden" name="intent" value="answer" />
          <input type="hidden" name="expected_status" value="blocked" />
          <textarea
            name="answer"
            rows={2}
            placeholder="回答"
            data-testid="console-question-answer-input"
            className={cn(textareaClass, "w-full")}
          />
          <Button
            type="submit"
            size="xs"
            variant="primary"
            disabled={submitting}
            data-testid="console-question-answer-send"
            className="w-fit"
          >
            回答する
          </Button>
        </fetcher.Form>
      )}
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-[0.7rem] のまま。 */}
      <p className="mt-1 text-sm text-fg-subtle lg:text-[0.7rem]">{block.at}</p>
    </BlockShell>
  );
}

function ApprovalBlockView({
  block,
  org,
  projects,
}: {
  block: Extract<ConsoleBlock, { kind: "approval" }>;
  org: readonly OrgNode[];
  projects: readonly Project[];
}) {
  const fetcher = useFetcher<ApprovalOpOutcome>({ key: `console-approval-${block.approval.id}` });
  const submitting = fetcher.state !== "idle";
  const a = block.approval;
  const decided = a.decision != null;
  return (
    <BlockShell testId="console-block-approval" className="w-full max-w-none border-warning-border bg-warning-soft/40">
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="flex flex-wrap items-center gap-2 text-sm font-medium text-fg-subtle lg:text-xs">
        {orgNodeName(a.node_id, org)}
        {projectName(a.project_id, projects) && <Badge tone="neutral">{projectName(a.project_id, projects)}</Badge>}
        {a.task_id && (
          <Link to={`/tasks/${a.task_id}`} className={cn(touchLinkClass, "ml-auto underline underline-offset-2")}>
            裏方のタスク
          </Link>
        )}
      </p>
      <div className="mt-1" data-testid="console-approval-question">
        <MarkdownViewer content={a.question} />
      </div>
      {decided ? (
        <p className="mt-2 text-sm text-fg-muted" data-testid="console-approval-decided">
          {a.decision} ・ {a.answer || "-"}
        </p>
      ) : (
        <fetcher.Form method="post" action="/approvals" className="mt-2 space-y-2">
          <input type="hidden" name="intent" value="approval_decide" />
          <input type="hidden" name="id" value={a.id} />
          <textarea
            name="answer"
            rows={2}
            placeholder="答え（「今後ずっと」のときは規則文として書く）"
            data-testid="console-approval-answer"
            className={cn(textareaClass, "w-full")}
          />
          <div className="flex flex-wrap gap-2">
            <Button
              type="submit"
              name="decision"
              value="once"
              size="xs"
              variant="secondary"
              disabled={submitting}
              data-testid="console-approval-once"
            >
              今回だけ
            </Button>
            <Button
              type="submit"
              name="decision"
              value="standing"
              size="xs"
              variant="primary"
              disabled={submitting}
              data-testid="console-approval-standing"
            >
              今後ずっと
            </Button>
            <Button
              type="submit"
              name="decision"
              value="denied"
              size="xs"
              variant="danger"
              disabled={submitting}
              data-testid="console-approval-denied"
            >
              認めない
            </Button>
          </div>
        </fetcher.Form>
      )}
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-[0.7rem] のまま。 */}
      <p className="mt-1 text-sm text-fg-subtle lg:text-[0.7rem]">{block.at}</p>
    </BlockShell>
  );
}

function MilestoneBlockView({
  block,
  projects,
}: {
  block: Extract<ConsoleBlock, { kind: "milestone" }>;
  projects: readonly Project[];
}) {
  const fetcher = useFetcher<ProjectOpOutcome>({ key: `console-milestone-${block.milestone.id}` });
  const submitting = fetcher.state !== "idle";
  const [note, setNote] = useState("");
  const [invalid, setInvalid] = useState(false);
  const m = block.milestone;

  function handleSubmit(e: FormEvent<HTMLFormElement>) {
    const submitter = (e.nativeEvent as SubmitEvent).submitter as HTMLButtonElement | null;
    const decision = (submitter?.value ?? "ok") as "ok" | "discuss" | "ng";
    if (!milestoneDecisionValid(decision, note)) {
      e.preventDefault();
      setInvalid(true);
      return;
    }
    setInvalid(false);
  }

  return (
    <BlockShell testId="console-block-milestone" className="w-full max-w-none border-primary-border bg-primary-soft/30">
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="flex items-center gap-2 text-sm font-medium text-fg-subtle lg:text-xs">
        途中目標の提案
        {projectName(m.project_id, projects) && <Badge tone="neutral">{projectName(m.project_id, projects)}</Badge>}
      </p>
      <p className="mt-1 font-medium" data-testid="console-milestone-title">
        {m.title}
      </p>
      {m.description && <p className="mt-1 whitespace-pre-wrap text-sm text-fg-muted">{m.description}</p>}
      {block.review && (
        <div className="mt-2" data-testid="console-milestone-review">
          <MarkdownViewer content={block.review.text} />
        </div>
      )}
      <fetcher.Form
        method="post"
        action={`/projects/${m.project_id}`}
        onSubmit={handleSubmit}
        className="mt-2 space-y-2"
      >
        <input type="hidden" name="intent" value="milestone_decide" />
        <input type="hidden" name="milestone_id" value={m.id} />
        <textarea
          name="note"
          rows={2}
          value={note}
          onChange={(e) => {
            setNote(e.target.value);
            if (invalid) setInvalid(false);
          }}
          placeholder="一言（議論・ng は必須）"
          data-testid="console-milestone-note"
          className={cn(textareaClass, "w-full")}
        />
        {invalid && (
          <p role="alert" className="text-sm text-danger lg:text-xs" data-testid="console-milestone-note-required">
            議論・ng には一言が要ります。
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          <Button
            type="submit"
            name="decision"
            value="ok"
            size="xs"
            variant="success"
            disabled={submitting}
            data-testid="console-milestone-ok"
          >
            ok
          </Button>
          <Button
            type="submit"
            name="decision"
            value="discuss"
            size="xs"
            variant="secondary"
            disabled={submitting}
            data-testid="console-milestone-discuss"
          >
            議論
          </Button>
          <Button
            type="submit"
            name="decision"
            value="ng"
            size="xs"
            variant="danger"
            disabled={submitting}
            data-testid="console-milestone-ng"
          >
            ng
          </Button>
        </div>
      </fetcher.Form>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-[0.7rem] のまま。 */}
      <p className="mt-1 text-sm text-fg-subtle lg:text-[0.7rem]">{block.at}</p>
    </BlockShell>
  );
}

function ReportBlockView({
  block,
  org,
  projects,
}: {
  block: Extract<ConsoleBlock, { kind: "report" }>;
  org: readonly OrgNode[];
  projects: readonly Project[];
}) {
  const [open, setOpen] = useState(false);
  const r = block.report;
  return (
    <BlockShell testId="console-block-report" className="w-full max-w-none">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="grid min-h-11 w-full grid-cols-[auto_auto_minmax(0,1fr)] items-center gap-2 text-left"
      >
        <Icon name={open ? "chevronDown" : "chevronRight"} className="size-3.5 shrink-0 text-fg-subtle" />
        <Icon name="send" className="size-3.5 text-fg-subtle" />
        <span className="min-w-0 break-words font-medium" data-testid="console-report-headline">
          {r.headline}
        </span>
        {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
        <span className="col-span-3 min-w-0 break-words text-sm text-fg-subtle lg:text-xs">
          {orgNodeName(r.node_id, org)}
        </span>
        {projectName(r.project_id, projects) && (
          <Badge tone="neutral" className="col-span-3 max-w-full justify-self-start truncate">
            {projectName(r.project_id, projects)}
          </Badge>
        )}
      </button>
      {open && r.body && (
        <div className="mt-2 border-t border-border pt-2" data-testid="console-report-body">
          <MarkdownViewer content={r.body} />
        </div>
      )}
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-[0.7rem] のまま。 */}
      <p className="mt-1 text-sm text-fg-subtle lg:text-[0.7rem]">{block.at}</p>
    </BlockShell>
  );
}

function KnowledgeBlockView({ block }: { block: Extract<ConsoleBlock, { kind: "knowledge" }> }) {
  const total = (block.ingested ?? 0) + (block.inbox ?? 0) + (block.discarded ?? 0);
  return (
    <BlockShell testId="console-block-knowledge" className="w-full max-w-none">
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="flex flex-wrap items-center gap-2 text-sm font-medium text-fg-subtle lg:text-xs">
        <Icon name="send" className="size-3.5" />
        <Link to={`/tasks/${block.task_id}`} className={cn(touchLinkClass, "underline underline-offset-2")}>
          {block.task_title}
        </Link>
        {block.state === "failed" && <Badge tone="danger">失敗</Badge>}
        {/* ADR-0052 D2: Qwen に届かず tier cheap の汎用ハーネスで抽出した run。 */}
        {isKnowledgeFallback(block.via) && (
          <Badge tone="neutral" data-testid="console-knowledge-fallback">
            cheap のハーネスで抽出
          </Badge>
        )}
      </p>
      <p className="mt-1 text-sm" data-testid="console-knowledge-line">
        この仕事から知識 {total} 件: 取り込み {block.ingested ?? 0} / 候補 {block.inbox ?? 0} / 破棄{" "}
        {block.discarded ?? 0}
      </p>
      <div className="mt-1.5 flex flex-wrap items-center justify-between gap-1 text-sm text-fg-subtle lg:text-[0.7rem]">
        <span>{block.at}</span>
        {(block.inbox ?? 0) > 0 && (
          <Link
            to="/knowledge/inbox"
            className={cn(touchLinkClass, "underline underline-offset-2")}
            data-testid="console-knowledge-inbox-link"
          >
            知識の候補（_inbox）を見る
          </Link>
        )}
      </div>
    </BlockShell>
  );
}
