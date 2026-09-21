import { type FormEvent, type ReactNode, useState } from "react";
import { Link, useFetcher } from "react-router";
import type {
  ApprovalOpOutcome,
  ProjectOpOutcome,
  TaskCommentOutcome,
  TransitionOutcome,
} from "~/celeris/action-types";
import type { ConsoleBlock, ConsoleReplyStep, EventsPage, McpClient, OrgNode, Project } from "~/celeris/types";
import {
  firstLine,
  formatRunEventRow,
  hasMoreThanFirstLine,
  progressSummaryLine,
  TOOL_SUMMARY_MAX_LENGTH,
  taskLineSummary,
  toolSummaryTruncated,
} from "~/lib/console";
import { shortId, truncateLabel } from "~/lib/format";
import { isKnowledgeFallback } from "~/lib/knowledge";
import { resolveMcpAuthorLabel } from "~/lib/mcp";
import { milestoneDecisionValid } from "~/lib/milestone-review";
import { relativeTimeLabel } from "~/lib/reports";
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
  mcpClients = [],
  fetchedAt,
  onReplyToConversation,
}: {
  block: ConsoleBlock;
  org: readonly OrgNode[];
  projects: readonly Project[];
  /** ADR-0056 D2（Phase 78/80）: human ブロックの `author`（`mcp:<client_id>`）の名前解決に使う
   * （`~/lib/mcp.ts::resolveMcpAuthorLabel`）。省略すれば id をそのまま出す（未取得時のフォールバック）。 */
  mcpClients?: readonly McpClient[];
  /** ADR-0055 D2 ラウンド 6: 相対時刻表示（`relativeTimeLabel`）の基準時刻。`~/lib/console.ts::ConsoleData.fetchedAt`。 */
  fetchedAt: string;
  onReplyToConversation: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
}) {
  switch (block.kind) {
    case "human":
      return (
        <HumanBlockView
          block={block}
          org={org}
          mcpClients={mcpClients}
          fetchedAt={fetchedAt}
          onReply={onReplyToConversation}
        />
      );
    case "reply":
      return <ReplyBlockView block={block} org={org} fetchedAt={fetchedAt} onReply={onReplyToConversation} />;
    case "task":
      return <TaskBlockView block={block} org={org} projects={projects} fetchedAt={fetchedAt} />;
    case "progress":
      return <ProgressBlockView block={block} />;
    case "question":
      return <QuestionBlockView block={block} org={org} fetchedAt={fetchedAt} />;
    case "approval":
      return <ApprovalBlockView block={block} org={org} projects={projects} fetchedAt={fetchedAt} />;
    case "milestone":
      return <MilestoneBlockView block={block} projects={projects} fetchedAt={fetchedAt} />;
    case "report":
      return <ReportBlockView block={block} org={org} projects={projects} fetchedAt={fetchedAt} />;
    case "knowledge":
      return <KnowledgeBlockView block={block} fetchedAt={fetchedAt} />;
    default:
      return null;
  }
}

function BlockShell({
  testId,
  align = "start",
  className,
  /** Phase 76（ADR-0055 D1 拡張、ライブリージョン）: 育つ返事（`ReplyBlockView`）が run 中の間 `true`。
   * 中身を `aria-live="polite"` にはしない（tool_use/tool_result の 1 手ごとに読み上げが騒がしくなる
   * のを避けるため、live は「考え中…」の行と確定テキストだけに個別で付ける）が、吹き出し全体は
   * 更新中であることを支援技術に伝える。 */
  busy,
  children,
}: {
  testId: string;
  align?: "start" | "end";
  className?: string;
  busy?: boolean;
  children: ReactNode;
}) {
  return (
    <div
      data-testid={testId}
      data-console-block={testId}
      className={cn("flex", align === "end" ? "justify-end" : "justify-start")}
    >
      <div
        aria-busy={busy || undefined}
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
 * フェーズ 74（ADR-0055 D2 ラウンド 6）: `at` は celeris が返す生の ISO をそのまま出していたが
 * （393px には長すぎ、`/approvals`・`/artifacts` の相対表示と揃っていなかった）、`relativeTimeLabel`
 * （`~/lib/reports.ts`。他画面と同じ 1 つの純粋関数）に揃え、絶対時刻は `title` に残す。
 */
function BlockHeader({
  icon,
  who,
  atIso,
  fetchedAt,
  align = "start",
}: {
  icon: IconName;
  who: ReactNode;
  atIso: string;
  fetchedAt: string;
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
      <span className="shrink-0" title={atIso}>
        {relativeTimeLabel(atIso, fetchedAt)}
      </span>
    </div>
  );
}

function HumanBlockView({
  block,
  org,
  mcpClients,
  fetchedAt,
  onReply,
}: {
  block: Extract<ConsoleBlock, { kind: "human" }>;
  org: readonly OrgNode[];
  mcpClients: readonly McpClient[];
  fetchedAt: string;
  onReply: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
}) {
  // ADR-0056 D2（Phase 78/80）: MCP 経由の発言（`author = mcp:<client_id>`）だけ「外部（<name>）」の
  // 帯を出す。人の発言（`author` が無い）は今までどおり帯を出さない。名前は `GET /mcp/clients` から
  // 解決し、まだ取れていない・見つからない客は id にフォールバックする（`~/lib/mcp.ts`）。
  const authorLabel = resolveMcpAuthorLabel(block.author, mcpClients);
  return (
    <BlockShell testId="console-block-human" align="end">
      <BlockHeader
        icon="send"
        who={`${orgNodeName(block.node_id, org)} へ`}
        atIso={block.at}
        fetchedAt={fetchedAt}
        align="end"
      />
      {authorLabel && (
        <div className="mb-1 flex justify-end">
          <Badge tone="info" data-testid="console-human-author">
            {authorLabel}
          </Badge>
        </div>
      )}
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
 * - `tool_result` は既定で畳み、1 行目だけを見せる（`firstLine`）。中身が 1 行しか無ければ展開できる
 *   行にせず素の行のまま（開いても閉じても同じものが見えるだけの空の三角を出さない）。
 * - 長い id・パス・URL（空白の無いトークン）が 393px を飛び出さないよう、`.markdown` と同じ
 *   `overflow-wrap: anywhere`（`break-words`＝`overflow-wrap: break-word` より min-content の計算にも
 *   効くので、詰まったフレックス行でも確実に折り返す）にする。
 * フェーズ 74（ADR-0055 D2 ラウンド 6、U-G29-2 / P-G29-2 の解消）: `tool_use` も `tool_result` も
 * 省略・折り畳みがあるときは同じ「行全体をタップで開閉」の作りにした（`<details>` はネイティブに開閉
 * できるが `aria-expanded` を持たない。スマホには hover が無く「長押しで `title` を見る」しか手段が
 * 無かったので、`button` + `aria-expanded` の開閉に揃え、キーボード（Enter/Space）でも操作できる
 * ようにした）。ボタンは `min-h-11 w-full` で 44px のタップ領域を確保する。
 * `~/routes/tasks.$id.tsx` のタイムライン（ADR-0048 D2、フェーズ 74）もこのコンポーネントをそのまま
 * 再利用する（「同じ step 行を使う」= 見た目を合わせる、が目的なので export する）。
 */
export function ReplyStepRow({ step }: { step: ConsoleReplyStep }) {
  const [expanded, setExpanded] = useState(false);
  const toneClass = step.error ? "bg-danger-soft text-danger-soft-fg" : "bg-surface-2/60";
  const errorBadge = step.error && (
    <Badge tone="danger" className="ml-2 shrink-0">
      エラー
    </Badge>
  );

  if (step.kind === "tool_result") {
    const first = firstLine(step.text);
    const expandable = hasMoreThanFirstLine(step.text);
    if (!expandable) {
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
      <div data-testid="console-reply-step" className={cn("rounded-md font-mono text-xs", toneClass)}>
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
          data-testid="console-reply-step-toggle"
          className="flex min-h-11 w-full items-start gap-1 px-2 py-1 text-left leading-snug [overflow-wrap:anywhere]"
        >
          <Icon name={expanded ? "chevronDown" : "chevronRight"} className="mt-0.5 size-3 shrink-0 text-fg-subtle" />
          <span className="min-w-0 flex-1">
            <span className="text-fg-subtle">→ </span>
            {first}
            {!expanded && <span className="ml-1 text-fg-subtle">…</span>}
          </span>
          {errorBadge}
        </button>
        {expanded && (
          <pre
            className="mx-2 mb-1.5 whitespace-pre-wrap [overflow-wrap:anywhere] text-fg-muted"
            data-testid="console-reply-step-body"
          >
            {step.text}
          </pre>
        )}
      </div>
    );
  }

  if (!toolSummaryTruncated(step.text)) {
    return (
      <div
        data-testid="console-reply-step"
        className={cn("rounded-md px-2 py-1 font-mono text-xs leading-snug [overflow-wrap:anywhere]", toneClass)}
      >
        {step.tool && <span className="font-semibold">[{step.tool}] </span>}
        {step.text}
        {errorBadge}
      </div>
    );
  }

  return (
    <div data-testid="console-reply-step" className={cn("rounded-md font-mono text-xs", toneClass)}>
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        aria-expanded={expanded}
        title={expanded ? undefined : step.text}
        data-testid="console-reply-step-toggle"
        className="flex min-h-11 w-full items-start gap-1 px-2 py-1 text-left leading-snug [overflow-wrap:anywhere]"
      >
        <Icon name={expanded ? "chevronDown" : "chevronRight"} className="mt-0.5 size-3 shrink-0 text-fg-subtle" />
        <span className="min-w-0 flex-1">
          {step.tool && <span className="font-semibold">[{step.tool}] </span>}
          {expanded ? step.text : truncateLabel(step.text, TOOL_SUMMARY_MAX_LENGTH)}
        </span>
        {errorBadge}
      </button>
    </div>
  );
}

function ReplyBlockView({
  block,
  org,
  fetchedAt,
  onReply,
}: {
  block: Extract<ConsoleBlock, { kind: "reply" }>;
  org: readonly OrgNode[];
  fetchedAt: string;
  onReply: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
}) {
  const result = block.actions_result;
  // ADR-0054 D2（Phase 68）: run 中は「育つ返事」（考え中…の 1 行 → tool call の行 → 本文）、
  // 完了すると確定した本文だけの、これまでどおりの吹き出しになる。
  const streaming = block.state === "streaming";
  const steps = block.steps ?? [];
  return (
    <BlockShell testId="console-block-reply" busy={streaming}>
      <BlockHeader
        icon="message"
        who={
          streaming ? (
            <span className="flex items-center gap-1.5">
              {orgNodeName(block.node_id, org)}
              {/* CSS だけの控えめな点滅（`prefers-reduced-motion: reduce` では `motion-reduce:` で止める。
                  ADR-0055 D2 ラウンド 5）。視覚だけの合図なので `aria-hidden`（状態は `busy`/`aria-live` 側で伝える）。 */}
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
        atIso={block.at}
        fetchedAt={fetchedAt}
      />
      {/* Phase 76（ライブリージョン）: 「考え中…」の 1 行は置き換え式（celeris 側が最新の 1 行だけを送る）
          なので、そのまま `aria-live="polite"` にしても読み上げは 1 回分で済む。`tool_use`/`tool_result`
          の 1 手ごと（下の `console-reply-steps`）はここに含めない（騒がしくなるため。ADR-0055 D2 の
          「一度に 1 つずつ」の精神を読み上げにも適用）。 */}
      {streaming && (
        <p
          aria-live="polite"
          className="mb-1.5 text-sm text-fg-subtle italic lg:text-xs"
          data-testid="console-reply-thinking"
        >
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
      {/* 確定していく本文（`text` は run 中も「ここまでの積み上げ」。ADR-0054 D2）だけを live にする。 */}
      {block.text && <MarkdownViewer content={block.text} live={streaming} />}
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
  fetchedAt,
}: {
  block: Extract<ConsoleBlock, { kind: "task" }>;
  org: readonly OrgNode[];
  projects: readonly Project[];
  fetchedAt: string;
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
        {/* ADR-0055 D1-3: 状態は 1 語のバッジ（`to` = 遷移先の状態）。理由・経過は下の行へ。
            Phase 76: Console はこのブロック自体が SSE で生きたまま更新される画面なので `role="status"`
            を付ける（`~/lib/format.ts` 等、ページ単位でしか変わらない一覧・詳細画面のバッジは付けない。
            全部に付けると読み上げが多すぎて「騒がしくしない」方針に反するため、範囲を Console に絞る）。 */}
        <StatusBadge status={t.to} role="status" />
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
          <span title={block.at}>{relativeTimeLabel(block.at, fetchedAt)}</span>
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
  fetchedAt,
}: {
  block: Extract<ConsoleBlock, { kind: "question" }>;
  org: readonly OrgNode[];
  fetchedAt: string;
}) {
  const fetcher = useFetcher<TransitionOutcome>({ key: `console-question-${block.task_id}` });
  const submitting = fetcher.state !== "idle";
  return (
    <BlockShell testId="console-block-question" className="w-full max-w-none border-warning-border bg-warning-soft/40">
      <BlockHeader
        icon="alert"
        who={`${block.node_id ? orgNodeName(block.node_id, org) : "-"} からの質問`}
        atIso={block.at}
        fetchedAt={fetchedAt}
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
  fetchedAt,
}: {
  block: Extract<ConsoleBlock, { kind: "approval" }>;
  org: readonly OrgNode[];
  projects: readonly Project[];
  fetchedAt: string;
}) {
  const fetcher = useFetcher<ApprovalOpOutcome>({ key: `console-approval-${block.approval.id}` });
  const submitting = fetcher.state !== "idle";
  const a = block.approval;
  const decided = a.decision != null;
  return (
    <BlockShell testId="console-block-approval" className="w-full max-w-none border-warning-border bg-warning-soft/40">
      <BlockHeader icon="shield" who={orgNodeName(a.node_id, org)} atIso={block.at} fetchedAt={fetchedAt} />
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
  fetchedAt,
}: {
  block: Extract<ConsoleBlock, { kind: "milestone" }>;
  projects: readonly Project[];
  fetchedAt: string;
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
      <BlockHeader icon="target" who="途中目標の提案" atIso={block.at} fetchedAt={fetchedAt} />
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
  fetchedAt,
}: {
  block: Extract<ConsoleBlock, { kind: "report" }>;
  org: readonly OrgNode[];
  projects: readonly Project[];
  fetchedAt: string;
}) {
  const [open, setOpen] = useState(false);
  const r = block.report;
  return (
    <BlockShell testId="console-block-report" className="w-full max-w-none">
      <BlockHeader icon="send" who={orgNodeName(r.node_id, org)} atIso={block.at} fetchedAt={fetchedAt} />
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

function KnowledgeBlockView({
  block,
  fetchedAt,
}: {
  block: Extract<ConsoleBlock, { kind: "knowledge" }>;
  fetchedAt: string;
}) {
  const total = (block.ingested ?? 0) + (block.inbox ?? 0) + (block.discarded ?? 0);
  return (
    <BlockShell testId="console-block-knowledge" className="w-full max-w-none">
      <BlockHeader icon="send" who={block.task_title} atIso={block.at} fetchedAt={fetchedAt} />
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
