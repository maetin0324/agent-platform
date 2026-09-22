import { useEffect, useMemo, useRef, useState } from "react";
import { useFetcher, useNavigate, useNavigation } from "react-router";
import type { ConsoleNewConversationOutcome } from "~/celeris/action-types";
import type { ConsoleBlock, McpClient, OrgNode, Project } from "~/celeris/types";
import { ConsoleComposer } from "~/components/ConsoleComposer";
import { useConsoleComposerContext, useRegisterConsoleComposer } from "~/components/ConsoleComposerContext";
import { useConsoleStream } from "~/hooks/useConsoleStream";
import {
  appendConsoleBlock,
  type ConsoleData,
  type ConsoleScopeKind,
  consoleWaitingCounts,
  hasStreamingReply,
  parseScope,
  replyTargetForMessageBlock,
  scopeForProject,
  shouldStickToBottom,
} from "~/lib/console";
import { cn } from "~/lib/utils";
import { ConsoleBlockItem, orgNodeName, projectName } from "./ConsoleBlockItem";
import { Button } from "./ui/button";
import { Card, CardBody } from "./ui/card";
import { hintClass, labelClass, selectClass } from "./ui/form";
import { Icon } from "./ui/Icon";
import { EmptyState, SectionTitle } from "./ui/misc";
import { Skeleton } from "./ui/skeleton";

/**
 * Console（ADR-0048 D4、GUI Phase G22）。`/`（`scope=all` 既定、`?scope=` で `project:<id>` へ深リンクできる）
 * と `/org/:id`（`scope=node:<id>` 固定）が同じ部品を使う（`docs/adr/0048-console.md` D4 の「ノードの画面は
 * 同じ部品」）。初期表示は loader（`~/celeris/console.server.ts` の `loadConsole`）、以後は
 * `~/hooks/useConsoleStream.ts` が `console.block` を 1 件ずつ足す（D1「Console は…block ごとに積み増す」）。
 */
export function Console({ data }: { data: ConsoleData }) {
  const { scope, org, projects, mcpClients, fetchedAt } = data;
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
  const streaming = hasStreamingReply(blocks);
  // ADR-0057（Phase 92）: 入力欄（composer）は `~/root.tsx`（レイアウトレベル、`<Outlet/>` の外）に
  // 移した。ここでは org/projects/streaming をその composer へ登録するだけ（`replyTarget` も composer 側の
  // Context が持つ）。`registration` は値が変わったときだけ新しい参照になるよう `useMemo` する
  // （`useRegisterConsoleComposer` のコメント参照。さもないと毎レンダー返信先が消える）。
  const registration = useMemo(() => ({ org, projects, streaming }), [org, projects, streaming]);
  useRegisterConsoleComposer(registration);
  const { setReplyTarget } = useConsoleComposerContext();
  // Phase 77（ADR-0055 D3「体感速度」）: `/`・`/org/:id` の間を移動すると scope が変わり、loader が
  // 新しい Console データを取りに行く。その間は直前の画面のブロックがそのまま残るだけなので、この画面
  // （Console を持つ 2 つの経路のどちらか）への遷移が pending の間は `BlockStream` をスケルトンに差し替える。
  const navigation = useNavigation();
  const isConsoleNavigationPending =
    navigation.state === "loading" &&
    (navigation.location?.pathname === "/" || (navigation.location?.pathname.startsWith("/org/") ?? false));

  function handleReply(block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) {
    setReplyTarget(replyTargetForMessageBlock(block));
  }

  return (
    <div className="space-y-4" data-testid="console-screen" data-console-scope={scope}>
      {/* Phase 76（ADR-0055 D1 拡張、画面の骨格）: Console（`/`・`/org/:id`）は他の画面と違い
          `~/components/ui/misc.tsx::PageHeader`（既定 `h1`）を使わないので、可視な見出しが 0 個になって
          いた。見た目は変えずに（既存のデザインに `h1` の見た目を足したくない）構造だけ足す `sr-only`。 */}
      <h1 className="sr-only">Console</h1>
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-0 flex-1">
          <WaitingStrip counts={counts} />
        </div>
        {/* フェーズ 73（ADR-0055 D2 ラウンド 5）: モバイルは「送る」だけを主役のボタンにしたいので、
            低頻度の「新しい会話」はここでは「その他」の開閉メニューに収める（`lg:` は従来どおり常時表示）。 */}
        <NewConversationMenu />
      </div>
      <div className="grid gap-4 xl:grid-cols-[16rem_minmax(0,1fr)]">
        <ScopePicker parsedScope={parsedScope} org={org} projects={projects} />
        <div className="min-w-0 space-y-3">
          <BlockStream
            blocks={blocks}
            org={org}
            projects={projects}
            mcpClients={mcpClients}
            fetchedAt={fetchedAt}
            onReply={handleReply}
            loading={isConsoleNavigationPending}
          />
          {/* フェーズ 71（ADR-0055 D2）: モバイルは入力欄を下部固定タブの上に `position: fixed` する。
              フローから抜けた分の高さを、この spacer で本文側にあらかじめ確保しておく（無いと固定入力欄が
              直前のブロックに重なる）。フェーズ 72: 高さは composer から届く実測値。ADR-0057（Phase 92）:
              composer 自体は `~/root.tsx` に移った（`ConsoleComposerContext` の `mobileHeight` 経由で届く）が、
              spacer はここに残す（BlockStream の内容が固定 composer の下に隠れないようにするのはこの画面の
              責務のため）。まだ測れていない初回描画・SSR は見積もりの `h-52`（13rem）にフォールバックする。 */}
          <ConsoleInputSpacer />
          {/* ADR-0057（Phase 92）: デスクトップだけここに composer を描く（`Console` パネルの右カラム内、
              見た目は Phase 91 まで不変）。モバイル版は `~/root.tsx` が `<Outlet/>` の外で描く。 */}
          <ConsoleComposer variant="desktop" />
        </div>
      </div>
    </div>
  );
}

/** 上記コメント参照。`mobileHeight` は `ConsoleComposerContext`（composer 自身が `ResizeObserver` で測る）。 */
function ConsoleInputSpacer() {
  const { mobileHeight } = useConsoleComposerContext();
  return (
    <div
      aria-hidden="true"
      data-testid="console-input-spacer"
      className="h-52 lg:hidden"
      style={mobileHeight != null ? { height: mobileHeight } : undefined}
    />
  );
}

/**
 * ADR-0054 D1/D3（Phase 67/68）: CoS の継続セッションを捨てる（`POST /console/new-conversation`）。
 * 確認は `window.confirm`（ブラウザ以外では聞かずそのまま送る。`~/components/ProjectRepos.tsx` の
 * 「削除」と同じ作り）。過去のやり取り自体は消えない（次に CoS へ話しかけたときの前置きが全量に戻るだけ）。
 * `onSubmitted` はメニューに収めたとき（`NewConversationMenu`）に、押した直後にメニューを閉じるため。
 */
function NewConversationButton({ onSubmitted }: { onSubmitted?: () => void }) {
  const fetcher = useFetcher<ConsoleNewConversationOutcome>();
  const submitting = fetcher.state !== "idle";
  const done = fetcher.data?.ok === true;
  return (
    <fetcher.Form method="post" action="/console/new-conversation" className="shrink-0">
      <Button
        type="submit"
        variant="secondary"
        size="sm"
        disabled={submitting}
        data-testid="console-new-conversation"
        className="w-full justify-start lg:w-auto lg:justify-center"
        onClick={(e) => {
          if (typeof window !== "undefined" && typeof window.confirm === "function") {
            if (!window.confirm("CoS との会話をリセットします（過去のやり取りは消えません）。よろしいですか？")) {
              e.preventDefault();
              return;
            }
          }
          onSubmitted?.();
        }}
      >
        <Icon name="message" />
        {done ? "新しい会話にしました" : "新しい会話"}
      </Button>
    </fetcher.Form>
  );
}

/**
 * フェーズ 73（ADR-0055 D2 ラウンド 5）: モバイルでは主役のボタンを「送る」1 つに絞りたいので、
 * 使う頻度が低い「新しい会話」は「その他」の開閉メニュー（`console-overflow-*`）に収める
 * （`~/root.tsx` の `MobileOtherSheet` と同じ「押すと開く・背景ボタンで閉じる」作り）。
 * `lg:` はこれまでどおりインラインの secondary ボタンのまま（デスクトップの見た目は変えない）。
 */
function NewConversationMenu() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <div className="hidden lg:block">
        <NewConversationButton />
      </div>
      <div className="relative shrink-0 lg:hidden">
        <button
          type="button"
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label="その他の操作"
          data-testid="console-overflow-trigger"
          onClick={() => setOpen((v) => !v)}
          className="grid size-11 place-items-center rounded-lg border border-border bg-surface text-fg-subtle hover:bg-surface-2"
        >
          <Icon name="more" className="size-4" />
        </button>
        {open && (
          <>
            <button
              type="button"
              aria-label="閉じる"
              onClick={() => setOpen(false)}
              className="fixed inset-0 z-10 cursor-default"
            />
            <div
              role="menu"
              data-testid="console-overflow-menu"
              className="absolute right-0 z-20 mt-2 w-56 rounded-lg border border-border bg-surface p-1.5 shadow-md"
            >
              <NewConversationButton onSubmitted={() => setOpen(false)} />
            </div>
          </>
        )}
      </div>
    </>
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
  mcpClients,
  fetchedAt,
  onReply,
  loading = false,
}: {
  blocks: ConsoleBlock[];
  org: readonly OrgNode[];
  projects: readonly Project[];
  mcpClients: readonly McpClient[];
  fetchedAt: string;
  onReply: (block: Extract<ConsoleBlock, { kind: "human" | "reply" }>) => void;
  /** Phase 77: 遷移が pending の間、中身をスケルトンに差し替える（枠の高さ・`data-testid` は変えない）。 */
  loading?: boolean;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  // `stickyRef` は「いま最新に張り付いているか」を、次に新しいブロックが来た瞬間に読むための ref
  // （effect の依存に入れると新しいブロックごとに listener を張り直すことになるので分けている）。
  // `stuck` は同じ値を state としても持ち、「最新へ」のジャンプピル（フェーズ 73）の表示・非表示に使う。
  const stickyRef = useRef(true);
  const [stuck, setStuck] = useState(true);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    function onScroll() {
      if (!el) return;
      const next = shouldStickToBottom(el.scrollHeight, el.scrollTop, el.clientHeight);
      stickyRef.current = next;
      setStuck(next);
    }
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    const el = containerRef.current;
    if (!el || !stickyRef.current || blocks.length === 0) return;
    el.scrollTop = el.scrollHeight;
  }, [blocks]);

  function jumpToLatest() {
    const el = containerRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    stickyRef.current = true;
    setStuck(true);
  }

  return (
    <div className="relative">
      <div
        ref={containerRef}
        data-testid="console-stream"
        aria-busy={loading || undefined}
        className="h-[clamp(10rem,calc(100dvh-30rem),36rem)] space-y-2.5 xl:h-[60vh] overflow-y-auto rounded-xl border border-border bg-surface-2/30 p-3"
      >
        {loading ? (
          <ConsoleStreamSkeleton />
        ) : blocks.length === 0 ? (
          <EmptyState icon="message" title="まだ何も流れていません">
            下の欄から話しかけてください。
          </EmptyState>
        ) : (
          blocks.map((b) => (
            <ConsoleBlockItem
              key={b.cursor}
              block={b}
              org={org}
              projects={projects}
              mcpClients={mcpClients}
              fetchedAt={fetchedAt}
              onReplyToConversation={onReply}
            />
          ))
        )}
      </div>
      {/* フェーズ 73（ADR-0055 D2 ラウンド 5）: 人が上にスクロールして読んでいる間は自動で追いかけない
          （`shouldStickToBottom`）。追いかけていない間だけ、下端に戻るピルを出す。 */}
      {!stuck && blocks.length > 0 && (
        <button
          type="button"
          onClick={jumpToLatest}
          data-testid="console-jump-to-latest"
          className="absolute inset-x-0 bottom-2 mx-auto min-h-11 w-fit rounded-full border border-border bg-surface px-4 text-sm font-medium text-fg shadow-md hover:bg-surface-2"
        >
          <span className="inline-flex items-center gap-1.5">
            <Icon name="chevronDown" className="size-3.5" />
            最新へ
          </span>
        </button>
      )}
    </div>
  );
}

/**
 * `console-stream` が pending の間のプレースホルダ（Phase 77、ADR-0055 D3「体感速度」）。枠自体は
 * `h-[clamp(...)]` で固定なので、これに差し替えてもレイアウトはガタつかない。人・返事のブロックを
 * 3 つぶん並べた見た目にして、「何か流れてきそうだ」という形を残す。
 */
function ConsoleStreamSkeleton() {
  return (
    <div aria-hidden="true" data-testid="console-stream-skeleton" className="space-y-2.5">
      {["a", "b", "c"].map((id) => (
        <div key={id} className="ml-auto max-w-[85%] space-y-1.5 rounded-xl bg-surface p-3">
          <Skeleton className="h-3 w-16" />
          <Skeleton className="h-3.5 w-56 max-w-full" />
        </div>
      ))}
    </div>
  );
}
