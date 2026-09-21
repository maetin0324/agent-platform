import { type ChangeEvent, type KeyboardEvent, useEffect, useRef, useState } from "react";
import { useFetcher, useNavigate } from "react-router";
import type { ConsoleInstructOutcome } from "~/celeris/action-types";
import type { ConsoleBlock, OrgNode, Project } from "~/celeris/types";
import { useConsoleStream } from "~/hooks/useConsoleStream";
import {
  appendConsoleBlock,
  applyMention,
  buildInstructBody,
  type ConsoleData,
  type ConsoleScopeKind,
  consoleWaitingCounts,
  findMentionQuery,
  type InstructReplyTarget,
  type MentionQuery,
  matchMentionCandidates,
  parseScope,
  replyTargetForMessageBlock,
  scopeForProject,
} from "~/lib/console";
import { cn } from "~/lib/utils";
import { ConsoleBlockItem, orgNodeName, projectName } from "./ConsoleBlockItem";
import { ErrorFlash } from "./Flash";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card, CardBody } from "./ui/card";
import { hintClass, labelClass, selectClass, textareaClass } from "./ui/form";
import { Icon } from "./ui/Icon";
import { EmptyState, SectionTitle } from "./ui/misc";

/**
 * Console（ADR-0048 D4、GUI Phase G22）。`/`（`scope=all` 既定、`?scope=` で `project:<id>` へ深リンクできる）
 * と `/org/:id`（`scope=node:<id>` 固定）が同じ部品を使う（`docs/adr/0048-console.md` D4 の「ノードの画面は
 * 同じ部品」）。初期表示は loader（`~/celeris/console.server.ts` の `loadConsole`）、以後は
 * `~/hooks/useConsoleStream.ts` が `console.block` を 1 件ずつ足す（D1「Console は…block ごとに積み増す」）。
 */
export function Console({ data }: { data: ConsoleData }) {
  const { scope, org, projects } = data;
  const parsedScope = parseScope(scope);

  // ブロックの一覧はこのコンポーネントのローカル state。scope が変わったとき（新しい画面）だけ
  // loader のページで作り直す。以後の同じ scope での再検証（root の SSE が daemon tick ごとに
  // 全ルートを再検証する。`~/hooks/useCelerisStream.ts`）では作り直さない（展開・入力欄の状態を保つ・
  // 二重取得を避ける。更新は `useConsoleStream` の SSE が担う）。`pageRef` は ref で持ち、effect の
  // 依存には入れない（`~/hooks/useConsoleStream.ts` の `sinceRef` と同じ考え方）。
  const pageRef = useRef(data.page);
  pageRef.current = data.page;
  const seededScope = useRef(scope);
  const [blocks, setBlocks] = useState<ConsoleBlock[]>(data.page.items);
  useEffect(() => {
    if (seededScope.current !== scope) {
      seededScope.current = scope;
      setBlocks(pageRef.current.items);
    }
  }, [scope]);

  useConsoleStream({
    scope,
    since: data.page.next_cursor ?? undefined,
    onBlock: (block) => setBlocks((prev) => appendConsoleBlock(prev, block)),
  });

  const counts = consoleWaitingCounts(blocks);
  const [replyTarget, setReplyTarget] = useState<InstructReplyTarget | null>(null);
  // フェーズ 72（ADR-0055 D2、U7 の解消）: spacer の高さは見積もり（`h-52` 固定）ではなく、
  // `ConsoleInput` 自身の実高さを `ResizeObserver` で測って反映する。返信先バナーの表示・非表示で
  // 高さが変わっても（`ResizeObserver` は border-box の変化を都度拾う）常に過不足なく確保できる。
  const [inputHeight, setInputHeight] = useState<number | null>(null);

  function handleReply(block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) {
    setReplyTarget(replyTargetForMessageBlock(block));
  }

  return (
    <div className="space-y-4" data-testid="console-screen" data-console-scope={scope}>
      <WaitingStrip counts={counts} />
      <div className="grid gap-4 xl:grid-cols-[16rem_minmax(0,1fr)]">
        <ScopePicker parsedScope={parsedScope} org={org} projects={projects} />
        <div className="min-w-0 space-y-3">
          <BlockStream blocks={blocks} org={org} projects={projects} onReply={handleReply} />
          {/* フェーズ 71（ADR-0055 D2）: モバイルは入力欄を下部固定タブの上に `position: fixed` する
              （`ConsoleInput` 自身が `lg:static` で戻る）。フローから抜けた分の高さを、この spacer で
              本文側にあらかじめ確保しておく（無いと固定入力欄が直前のブロックに重なる）。
              フェーズ 72: 高さは `ConsoleInput` から届く実測値（`inputHeight`）。まだ測れていない
              初回描画・SSR は見積もりの `h-52`（13rem）にフォールバックする。 */}
          <div
            aria-hidden="true"
            data-testid="console-input-spacer"
            className="h-52 lg:hidden"
            style={inputHeight != null ? { height: inputHeight } : undefined}
          />
          <ConsoleInput
            org={org}
            replyTarget={replyTarget}
            onClearReply={() => setReplyTarget(null)}
            defaultScope={parsedScope.kind === "all" ? null : scope}
            onHeightChange={setInputHeight}
          />
        </div>
      </div>
    </div>
  );
}

function WaitingStrip({ counts }: { counts: { questions: number; approvals: number; milestones: number } }) {
  const total = counts.questions + counts.approvals + counts.milestones;
  return (
    <div
      data-testid="console-waiting-strip"
      className={cn(
        // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
        "flex flex-wrap items-center gap-3 rounded-xl border px-3 py-2 text-sm shadow-xs backdrop-blur lg:text-xs",
        total > 0 ? "border-warning-border bg-warning-soft/80" : "border-border bg-surface/80",
      )}
    >
      <Icon name="alert" className="size-3.5" />
      <span data-testid="console-waiting-questions">質問 {counts.questions}</span>
      <span data-testid="console-waiting-approvals">認可 {counts.approvals}</span>
      <span data-testid="console-waiting-milestones">途中目標 {counts.milestones}</span>
      {total === 0 && <span className="text-fg-subtle">待ちはありません</span>}
    </div>
  );
}

function ScopePicker({
  parsedScope,
  org,
  projects,
}: {
  parsedScope: { kind: ConsoleScopeKind; id: string | null };
  org: readonly OrgNode[];
  projects: readonly Project[];
}) {
  const navigate = useNavigate();
  const [expanded, setExpanded] = useState(false);
  const [kindDraft, setKindDraft] = useState<ConsoleScopeKind>(parsedScope.kind);
  useEffect(() => setKindDraft(parsedScope.kind), [parsedScope.kind]);

  return (
    <Card data-testid="console-scope-picker">
      <CardBody className="space-y-3 p-3 xl:p-5">
        <button
          type="button"
          className="flex min-h-11 w-full items-center justify-between gap-2 text-left text-sm xl:hidden"
          aria-expanded={expanded}
          aria-controls="console-scope-options"
          onClick={() => setExpanded((value) => !value)}
        >
          <span className="min-w-0 break-words">
            範囲:{" "}
            {parsedScope.kind === "all"
              ? "全体"
              : parsedScope.kind === "project"
                ? (projects.find((p) => p.id === parsedScope.id)?.title ?? "案件")
                : (org.find((n) => n.id === parsedScope.id)?.name ?? "ノード")}
          </span>
          <span className="shrink-0">{expanded ? "閉じる" : "変更"}</span>
        </button>
        <div id="console-scope-options" className={cn("space-y-3 xl:block", expanded ? "block" : "hidden")}>
          <SectionTitle icon="layers" className="text-sm">
            範囲
          </SectionTitle>
          <div>
            <label htmlFor="console-scope-kind" className={labelClass}>
              見る範囲
            </label>
            <select
              id="console-scope-kind"
              data-testid="console-scope-kind"
              value={kindDraft}
              onChange={(e) => {
                const kind = e.target.value as ConsoleScopeKind;
                setKindDraft(kind);
                if (kind === "all") navigate("/");
              }}
              className={cn(selectClass, "mt-1.5 w-full")}
            >
              <option value="all">全体</option>
              <option value="project">案件</option>
              <option value="node">ノード</option>
            </select>
          </div>
          {kindDraft === "project" && (
            <div>
              <label htmlFor="console-scope-project" className={labelClass}>
                どの案件
              </label>
              <select
                id="console-scope-project"
                data-testid="console-scope-project"
                value={parsedScope.kind === "project" ? (parsedScope.id ?? "") : ""}
                onChange={(e) => {
                  if (e.target.value) navigate(`/?scope=${encodeURIComponent(scopeForProject(e.target.value))}`);
                }}
                className={cn(selectClass, "mt-1.5 w-full")}
              >
                <option value="">選ぶ…</option>
                {projects.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.title}
                  </option>
                ))}
              </select>
            </div>
          )}
          {kindDraft === "node" && (
            <div>
              <label htmlFor="console-scope-node" className={labelClass}>
                どのノード
              </label>
              <select
                id="console-scope-node"
                data-testid="console-scope-node"
                value={parsedScope.kind === "node" ? (parsedScope.id ?? "") : ""}
                onChange={(e) => {
                  if (e.target.value) navigate(`/org/${encodeURIComponent(e.target.value)}`);
                }}
                className={cn(selectClass, "mt-1.5 w-full")}
              >
                <option value="">選ぶ…</option>
                {org.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </select>
            </div>
          )}
          <p className={hintClass} data-testid="console-scope-current">
            いま: {scopeLabel(parsedScope, org, projects)}
          </p>
        </div>
      </CardBody>
    </Card>
  );
}

function scopeLabel(
  parsed: { kind: ConsoleScopeKind; id: string | null },
  org: readonly OrgNode[],
  projects: readonly Project[],
): string {
  if (parsed.kind === "all") return "全体";
  if (parsed.kind === "project") return `案件: ${projectName(parsed.id, projects) ?? parsed.id}`;
  return `ノード: ${parsed.id ? orgNodeName(parsed.id, org) : "-"}`;
}

function BlockStream({
  blocks,
  org,
  projects,
  onReply,
}: {
  blocks: ConsoleBlock[];
  org: readonly OrgNode[];
  projects: readonly Project[];
  onReply: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const stickyRef = useRef(true);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    function onScroll() {
      if (!el) return;
      stickyRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 96;
    }
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    const el = containerRef.current;
    if (!el || !stickyRef.current || blocks.length === 0) return;
    el.scrollTop = el.scrollHeight;
  }, [blocks]);

  return (
    <div
      ref={containerRef}
      data-testid="console-stream"
      className="h-[clamp(10rem,calc(100dvh-30rem),36rem)] space-y-2.5 xl:h-[60vh] overflow-y-auto rounded-xl border border-border bg-surface-2/30 p-3"
    >
      {blocks.length === 0 ? (
        <EmptyState icon="message" title="まだ何も流れていません">
          下の欄から話しかけてください。
        </EmptyState>
      ) : (
        blocks.map((b) => (
          <ConsoleBlockItem key={b.cursor} block={b} org={org} projects={projects} onReplyToConversation={onReply} />
        ))
      )}
    </div>
  );
}

function ConsoleInput({
  org,
  replyTarget,
  onClearReply,
  defaultScope,
  onHeightChange,
}: {
  org: readonly OrgNode[];
  replyTarget: InstructReplyTarget | null;
  onClearReply: () => void;
  defaultScope: string | null;
  /** フェーズ 72（U7）: この入力欄の実高さ（border-box）が変わるたびに呼ぶ。親の spacer を正確に保つ。 */
  onHeightChange: (height: number) => void;
}) {
  const fetcher = useFetcher<ConsoleInstructOutcome>();
  const [text, setText] = useState("");
  const [mention, setMention] = useState<MentionQuery | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const submitting = fetcher.state !== "idle";
  const handledMessageId = useRef<string | null>(null);

  // フェーズ 72（U7）: 見積もりの `h-52` をやめ、`ConsoleInput` の実高さを都度測って親（spacer）へ渡す
  // （返信先バナーの表示・非表示、文字入力での行数の変化にも追随する）。`ResizeObserver` が無い環境
  // （テストの node 環境等）では何もしない（spacer は見積もりの `h-52` のまま）。
  useEffect(() => {
    const el = rootRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const height = entry.borderBoxSize?.[0]?.blockSize ?? entry.contentRect.height;
      onHeightChange(height);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [onHeightChange]);

  useEffect(() => {
    const outcome = fetcher.data;
    if (outcome?.ok && outcome.accepted.message_id !== handledMessageId.current) {
      handledMessageId.current = outcome.accepted.message_id;
      setText("");
      onClearReply();
    }
  }, [fetcher.data, onClearReply]);

  function handleChange(e: ChangeEvent<HTMLTextAreaElement>) {
    const value = e.target.value;
    setText(value);
    setMention(findMentionQuery(value, e.target.selectionStart ?? value.length));
  }

  function submit() {
    if (!text.trim() || submitting) return;
    const body = buildInstructBody(text, replyTarget, defaultScope);
    fetcher.submit({ text: body.text, scope: body.scope ?? "" }, { method: "post" });
  }

  function handleKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
    // タッチ端末では Enter は改行。送信は明示ボタンから。
    if (
      e.key === "Enter" &&
      !e.shiftKey &&
      !mention &&
      !window.matchMedia("(max-width: 1023px), (pointer: coarse)").matches
    ) {
      e.preventDefault();
      submit();
    }
    if (e.key === "Escape" && mention) setMention(null);
  }

  function pickMention(nodeId: string) {
    if (!mention) return;
    const applied = applyMention(text, mention, nodeId);
    setText(applied.text);
    setMention(null);
    requestAnimationFrame(() => {
      textareaRef.current?.focus();
      textareaRef.current?.setSelectionRange(applied.cursor, applied.cursor);
    });
  }

  const candidates = mention
    ? matchMentionCandidates(
        mention.query,
        org.map((n) => ({ id: n.id, name: n.name })),
      )
    : [];
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : undefined;

  return (
    <div
      ref={rootRef}
      data-testid="console-input"
      // ADR-0055 D2「入力欄は画面下固定、キーボード表示時に隠れない」（フェーズ 71）。
      // モバイルは下部固定タブ（`h-16` + `env(safe-area-inset-bottom)`。`~/root.tsx`）のすぐ上に
      // `position: fixed` する（タブバー自身が safe-area を確保しているので、ここでは重ねない）。
      // `lg:` でデスクトップは元の通常フロー（`static`）に戻す。
      className="fixed inset-x-0 bottom-16 z-20 space-y-2 border-t border-border bg-surface/95 px-4 py-3 backdrop-blur-xl lg:static lg:inset-auto lg:border-0 lg:bg-transparent lg:px-0 lg:py-0 lg:backdrop-blur-none"
    >
      <ErrorFlash error={error} />
      {replyTarget && (
        <p className="flex items-center gap-2 text-xs text-fg-subtle" data-testid="console-reply-target">
          <Badge tone="info">
            返信先: {replyTarget.kind === "node" ? orgNodeName(replyTarget.nodeId, org) : "この案件の CoS"}
          </Badge>
          <button type="button" onClick={onClearReply} className="min-h-11 px-2 underline underline-offset-2">
            やめる
          </button>
        </p>
      )}
      <div className="relative">
        <textarea
          ref={textareaRef}
          rows={3}
          value={text}
          onChange={handleChange}
          onKeyDown={handleKeyDown}
          aria-label="CoS へのメッセージ"
          placeholder="CoS や @node-id に話しかける"
          data-testid="console-text"
          className={cn(textareaClass, "w-full text-base")}
        />
        {mention && candidates.length > 0 && (
          <ul
            data-testid="console-mention-list"
            className="absolute bottom-full left-0 z-20 mb-1 w-64 overflow-hidden rounded-lg border border-border bg-surface shadow-md"
          >
            {candidates.map((c) => (
              <li key={c.id}>
                <button
                  type="button"
                  data-testid="console-mention-item"
                  onMouseDown={(e) => {
                    e.preventDefault();
                    pickMention(c.id);
                  }}
                  className="flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-sm hover:bg-surface-2"
                >
                  <span>{c.name}</span>
                  <span className="text-xs text-fg-subtle">@{c.id}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      <Button
        type="button"
        variant="primary"
        size="sm"
        disabled={submitting || !text.trim()}
        onClick={submit}
        data-testid="console-send"
        className="min-h-11 min-w-24"
      >
        <Icon name="send" />
        送る
      </Button>
    </div>
  );
}
