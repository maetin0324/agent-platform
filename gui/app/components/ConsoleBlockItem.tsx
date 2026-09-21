import { type FormEvent, type ReactNode, useState } from "react";
import { Link, useFetcher } from "react-router";
import type {
  ApprovalOpOutcome,
  ProjectOpOutcome,
  TaskCommentOutcome,
  TransitionOutcome,
} from "~/celeris/action-types";
import type { ConsoleBlock, ConsoleReplyStep, EventsPage, OrgNode, Project } from "~/celeris/types";
import {
  firstLine,
  formatRunEventRow,
  hasMoreThanFirstLine,
  progressSummaryLine,
  taskLineSummary,
} from "~/lib/console";
import { shortId, truncateLabel } from "~/lib/format";
import { isKnowledgeFallback } from "~/lib/knowledge";
import { milestoneDecisionValid } from "~/lib/milestone-review";
import { cn } from "~/lib/utils";
import { MarkdownViewer } from "./MarkdownViewer";
import { Badge, StatusBadge } from "./ui/badge";
import { Button } from "./ui/button";
import { hintClass, textareaClass, touchLinkClass } from "./ui/form";
import { Icon, type IconName } from "./ui/Icon";
import { Mono } from "./ui/misc";

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

/**
 * ブロック先頭の帯（フェーズ 71、ADR-0055 D2「状態はバッジ 1 語 + 色。理由・詳細は行の下か開閉に」の
 * 精神を Console にも: 誰 / いつ を吹き出しの先頭にまとめ、本文中に「at」を重複させない）。
 * `align="end"`（人の発言）は右寄せ、それ以外（CoS 側）は左寄せの帯にする。
 */
function BlockHeader({
  icon,
  who,
  at,
  align = "start",
}: {
  icon: IconName;
  who: ReactNode;
  at: string;
  align?: "start" | "end";
}) {
  return (
    // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
    <div
      className={cn(
        "mb-1.5 flex items-center gap-2 text-sm font-medium text-fg-subtle lg:text-xs",
        align === "end" ? "flex-row-reverse justify-between opacity-80" : "justify-between",
      )}
    >
      <span className="flex min-w-0 items-center gap-1.5">
        <Icon name={icon} className="size-3.5 shrink-0" />
        <span className="min-w-0 truncate">{who}</span>
      </span>
      <span className="shrink-0">{at}</span>
    </div>
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
      <BlockHeader icon="send" who={`${orgNodeName(block.node_id, org)} へ`} at={block.at} align="end" />
      <p className="whitespace-pre-wrap">{block.text}</p>
      <div className="mt-1 flex justify-end">
        <ReplyButton onClick={() => onReply(block)} />
      </div>
    </BlockShell>
  );
}

/**
 * ADR-0054 D2（Phase 68）: 育つ返事の中の 1 手（`tool_use`/`tool_result`）を 1 行に。
 * フェーズ 73（ADR-0055 D2 ラウンド 5、Claude Code / Codex ライクな磨き）:
 * - `tool_use` は道具名を太字にし、要約は長ければ省略して `title` に全文を残す（`truncateLabel`）。
 * - `tool_result` は既定で畳み、1 行目だけを `<summary>` に見せる（`firstLine`）。中身が 1 行しか
 *   無ければ `<details>` にせず素の行のまま（開いても閉じても同じものが見えるだけの空の三角を出さない）。
 * - 長い id・パス・URL（空白の無いトークン）が 393px を飛び出さないよう、`.markdown` と同じ
 *   `overflow-wrap: anywhere`（`break-words`＝`overflow-wrap: break-word` より min-content の計算にも
 *   効くので、詰まったフレックス行でも確実に折り返す）にする。
 */
function ReplyStepRow({ step }: { step: ConsoleReplyStep }) {
  const toneClass = step.error ? "bg-danger-soft text-danger-soft-fg" : "bg-surface-2/60";
  const errorBadge = step.error && (
    <Badge tone="danger" className="ml-2 shrink-0">
      エラー
    </Badge>
  );

  if (step.kind === "tool_result") {
    const first = firstLine(step.text);
    const more = hasMoreThanFirstLine(step.text);
    if (!more) {
      return (
        <div
          data-testid="console-reply-step"
          className={cn("rounded-md px-2 py-1 font-mono text-xs leading-snug [overflow-wrap:anywhere]", toneClass)}
        >
          <span className="text-fg-subtle">→ </span>
          {first}
          {errorBadge}
        </div>
      );
    }
    return (
      <details data-testid="console-reply-step" className={cn("rounded-md px-2 py-1 font-mono text-xs", toneClass)}>
        <summary className="cursor-pointer leading-snug [overflow-wrap:anywhere] marker:text-fg-subtle">
          <span className="text-fg-subtle">→ </span>
          {first}
          <span className="ml-1 text-fg-subtle">…</span>
          {errorBadge}
        </summary>
        <pre className="mt-1 whitespace-pre-wrap [overflow-wrap:anywhere] text-fg-muted">{step.text}</pre>
      </details>
    );
  }

  return (
    <div
      data-testid="console-reply-step"
      className={cn("rounded-md px-2 py-1 font-mono text-xs leading-snug [overflow-wrap:anywhere]", toneClass)}
    >
      {step.tool && <span className="font-semibold">[{step.tool}] </span>}
      <span title={step.text}>{truncateLabel(step.text, 90)}</span>
      {errorBadge}
    </div>
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
  // ADR-0054 D2（Phase 68）: run 中は「育つ返事」（考え中…の 1 行 → tool call の行 → 本文）、
  // 完了すると確定した本文だけの、これまでどおりの吹き出しになる。
  const streaming = block.state === "streaming";
  const steps = block.steps ?? [];
  return (
    <BlockShell testId="console-block-reply">
      <BlockHeader
        icon="message"
        who={
          streaming ? (
            <span className="flex items-center gap-1.5">
              {orgNodeName(block.node_id, org)}
              {/* CSS だけの控えめな点滅（`prefers-reduced-motion: reduce` では `motion-reduce:` で止める。
                  ADR-0055 D2 ラウンド 5）。 */}
              <span
                className="inline-block size-1.5 animate-pulse rounded-full bg-primary motion-reduce:animate-none"
                aria-hidden="true"
                data-testid="console-reply-streaming-dot"
              />
            </span>
          ) : (
            orgNodeName(block.node_id, org)
          )
        }
        at={block.at}
      />
      {streaming && (
        <p className="mb-1.5 text-sm text-fg-subtle italic lg:text-xs" data-testid="console-reply-thinking">
          {block.thinking || "考え中…"}
        </p>
      )}
      {streaming && steps.length > 0 && (
        <div className="mb-2 space-y-1" data-testid="console-reply-steps">
          {steps.map((step, i) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: `steps` はサーバの積み上げで安定した id を持たない
            <ReplyStepRow key={i} step={step} />
          ))}
        </div>
      )}
      {block.text && <MarkdownViewer content={block.text} />}
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
      {!streaming && (
        <div className="mt-1.5 flex flex-wrap items-center justify-between gap-2">
          {block.run_id && block.task_id ? (
            <Link to={`/tasks/${block.task_id}`} className={cn(touchLinkClass, "text-sm underline underline-offset-2")}>
              この返事を作った run
            </Link>
          ) : (
            <span />
          )}
          <ReplyButton onClick={() => onReply(block)} />
        </div>
      )}
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
    // フェーズ 71: task ブロックは CoS の吹き出しの直下に付く「カード」として、通常の吹き出しより
    // 一目盛りコンパクトに（他の CoS 側ブロックと同じ左寄せの列に積む。ADR-0054 D2 の
    // 「作ったタスクは task ブロックとして返事の直下に出る」の見た目）。
    <BlockShell testId="console-block-task" className="w-full max-w-none bg-surface-2/40 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <Icon name="activity" className="size-3.5 shrink-0 text-fg-subtle" />
        <Link
          to={`/tasks/${t.task_id}`}
          className={cn(touchLinkClass, "min-w-0 flex-1 font-medium underline underline-offset-2")}
        >
          {t.title}
        </Link>
        {/* ADR-0055 D1-3: 状態は 1 語のバッジ（`to` = 遷移先の状態）。理由・経過は下の行へ。 */}
        <StatusBadge status={t.to} />
      </div>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <p className="mt-1 text-sm text-fg-subtle lg:text-xs" data-testid="console-task-summary">
        {taskLineSummary(t)}
        {t.assignee && <> ・ {orgNodeName(t.assignee, org)}</>}
        {projName && <> ・ {projName}</>}
      </p>
      <div className="mt-1.5 flex flex-wrap items-center justify-between gap-1 text-sm text-fg-subtle lg:text-[0.7rem]">
        <span className="flex items-center gap-2">
          {/* ADR-0055 D2: 長い id は末尾だけ、全文は title 属性。 */}
          <Mono title={t.task_id}>{shortId(t.task_id)}</Mono>
          <span>{block.at}</span>
        </span>
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
      <BlockHeader
        icon="alert"
        who={`${block.node_id ? orgNodeName(block.node_id, org) : "-"} からの質問`}
        at={block.at}
      />
      <p data-testid="console-question-text">{block.text}</p>
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
      <BlockHeader icon="shield" who={orgNodeName(a.node_id, org)} at={block.at} />
      <div className="flex flex-wrap items-center gap-2">
        {projectName(a.project_id, projects) && <Badge tone="neutral">{projectName(a.project_id, projects)}</Badge>}
        {a.task_id && (
          <Link to={`/tasks/${a.task_id}`} className={cn(touchLinkClass, "ml-auto underline underline-offset-2")}>
            裏方のタスク
          </Link>
        )}
      </div>
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
      <BlockHeader icon="target" who="途中目標の提案" at={block.at} />
      {projectName(m.project_id, projects) && <Badge tone="neutral">{projectName(m.project_id, projects)}</Badge>}
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
      <BlockHeader icon="send" who={orgNodeName(r.node_id, org)} at={block.at} />
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="grid min-h-11 w-full grid-cols-[auto_minmax(0,1fr)] items-center gap-2 text-left"
      >
        <Icon name={open ? "chevronDown" : "chevronRight"} className="size-3.5 shrink-0 text-fg-subtle" />
        <span className="min-w-0 break-words font-medium" data-testid="console-report-headline">
          {r.headline}
        </span>
        {projectName(r.project_id, projects) && (
          <Badge
            tone="neutral"
            className="col-span-2 max-w-full justify-self-start truncate"
            title={projectName(r.project_id, projects) ?? undefined}
          >
            {projectName(r.project_id, projects)}
          </Badge>
        )}
      </button>
      {open && r.body && (
        <div className="mt-2 border-t border-border pt-2" data-testid="console-report-body">
          <MarkdownViewer content={r.body} />
        </div>
      )}
    </BlockShell>
  );
}

function KnowledgeBlockView({ block }: { block: Extract<ConsoleBlock, { kind: "knowledge" }> }) {
  const total = (block.ingested ?? 0) + (block.inbox ?? 0) + (block.discarded ?? 0);
  return (
    <BlockShell testId="console-block-knowledge" className="w-full max-w-none">
      <BlockHeader icon="send" who={block.task_title} at={block.at} />
      <div className="flex flex-wrap items-center gap-2">
        <Link to={`/tasks/${block.task_id}`} className={cn(touchLinkClass, "text-sm underline underline-offset-2")}>
          このタスクを見る
        </Link>
        {block.state === "failed" && <Badge tone="danger">失敗</Badge>}
        {/* ADR-0052 D2: Qwen に届かず tier cheap の汎用ハーネスで抽出した run。 */}
        {isKnowledgeFallback(block.via) && (
          <Badge tone="neutral" data-testid="console-knowledge-fallback">
            cheap のハーネスで抽出
          </Badge>
        )}
      </div>
      <p className="mt-1 text-sm" data-testid="console-knowledge-line">
        この仕事から知識 {total} 件: 取り込み {block.ingested ?? 0} / 候補 {block.inbox ?? 0} / 破棄{" "}
        {block.discarded ?? 0}
      </p>
      <div className="mt-1.5 flex flex-wrap items-center justify-end gap-1 text-sm text-fg-subtle lg:text-[0.7rem]">
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
