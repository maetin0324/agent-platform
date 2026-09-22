import { type ChangeEvent, type KeyboardEvent, useEffect, useRef, useState } from "react";
import { useFetcher, useLocation } from "react-router";
import type { ConsoleInstructOutcome } from "~/celeris/action-types";
import { useConsoleComposerContext } from "~/components/ConsoleComposerContext";
import {
  applyMention,
  buildInstructBody,
  findMentionQuery,
  type MentionQuery,
  matchMentionCandidates,
} from "~/lib/console";
import {
  consoleComposerActionFor,
  consoleComposerScopeForLocation,
  isConsoleComposerPathname,
} from "~/lib/console-composer";
import { cn } from "~/lib/utils";
import { orgNodeName, projectName } from "./ConsoleBlockItem";
import { ErrorFlash } from "./Flash";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { textareaClass } from "./ui/form";
import { Icon } from "./ui/Icon";

/**
 * Console の入力欄（ADR-0048 D4「入力欄」、ADR-0055 D2「画面下固定」、ADR-0057「レイアウトレベルへ」）。
 *
 * `~/root.tsx`（layout、`variant="mobile"`。`<Outlet/>` の外＝ページ遷移アニメーション
 * `.animate-fade-in` の外なので、Chromium の containing-block の入れ替わりの影響を構造的に受けない。
 * ADR-0057 参照）と `~/components/Console.tsx`（`variant="desktop"`。デスクトップの見た目は Phase 91 まで
 * と同じ、Console パネルの右カラム内の静的配置）の 2 か所から呼ぶ。両方とも同じ
 * `~/components/ConsoleComposerContext.tsx` を読むので、状態（返信先・org/projects・streaming）は 1 つ。
 * `text`/`mention` はこのコンポーネント自身のローカル state（変体ごとに別インスタンス。返信先など
 * 画面をまたいで保つ必要がある状態だけ Context に置く。ADR-0057 D3）。
 *
 * どのルートの `POST /console/instruct` に送るか・「この案件の文脈で話す」の scope は、`useLocation()` の
 * pathname/search から `~/lib/console-composer.ts` の純粋関数で決める（`~/routes/home.tsx` /
 * `~/routes/org.$id.tsx` の loader と同じ規則。celeris への問い合わせは無い）。対象パスでない画面
 * （`isConsoleComposerPathname` が false）では何も描かない。
 */
export function ConsoleComposer({ variant }: { variant: "mobile" | "desktop" }) {
  const location = useLocation();
  const { pathname, search } = location;
  const { registration, replyTarget, setReplyTarget, setMobileHeight } = useConsoleComposerContext();
  const { org, projects, streaming } = registration;
  const defaultScope = isConsoleComposerPathname(pathname) ? consoleComposerScopeForLocation(pathname, search) : null;

  const fetcher = useFetcher<ConsoleInstructOutcome>();
  const [text, setText] = useState("");
  const [mention, setMention] = useState<MentionQuery | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const submitting = fetcher.state !== "idle";
  const handledMessageId = useRef<string | null>(null);

  // ADR-0057: モバイル版だけ実高さを測って Console 側の spacer（`console-input-spacer`）へ渡す。
  // デスクトップ版は通常フローに参加する（spacer は `lg:hidden` なので不要）。
  useEffect(() => {
    if (variant !== "mobile") return;
    const el = rootRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const height = entry.borderBoxSize?.[0]?.blockSize ?? entry.contentRect.height;
      setMobileHeight(height);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [variant, setMobileHeight]);

  useEffect(() => {
    const outcome = fetcher.data;
    if (outcome?.ok && outcome.accepted.message_id !== handledMessageId.current) {
      handledMessageId.current = outcome.accepted.message_id;
      setText("");
      setReplyTarget(null);
    }
  }, [fetcher.data, setReplyTarget]);

  if (!isConsoleComposerPathname(pathname)) return null;

  function handleChange(e: ChangeEvent<HTMLTextAreaElement>) {
    const value = e.target.value;
    setText(value);
    setMention(findMentionQuery(value, e.target.selectionStart ?? value.length));
  }

  function submit() {
    if (!text.trim() || submitting) return;
    const body = buildInstructBody(text, replyTarget, defaultScope);
    // 送信先は「いま見ているページ」自身（`/` か `/org/:id`）。この composer は `~/root.tsx` からも呼ばれる
    // （ルートの外）ので、既定の action（最寄りのルート）には頼れない（ADR-0057）。`/` は**インデックス
    // ルート**なので、素の pathname のままだと React Router は親（`root`。action を持たない）へ解決し 405
    // になる（Phase 102 の本番不具合）。`~/lib/console-composer.ts::consoleComposerActionFor` が
    // `/` → `/?index` に変換する（`/org/:id` は非インデックスなのでそのまま）。
    fetcher.submit(
      { text: body.text, scope: body.scope ?? "" },
      { method: "post", action: consoleComposerActionFor(pathname) },
    );
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
  // モバイル版は `console-input` をそのまま使う（`scripts/mobile-audit.mjs` の `document.querySelector`
  // 前提はモバイル viewport でしか走らないので、これで唯一の一致になる。デスクトップ版は別の testid にし、
  // 「同じ testid の要素が同時に 2 つ DOM にある」状態を避ける（Playwright の strict mode・単純な
  // querySelector どちらの前提も崩さない。ADR-0057）。
  const testId = variant === "mobile" ? "console-input" : "console-input-desktop";

  return (
    <div
      ref={rootRef}
      data-testid={testId}
      data-console-composer-variant={variant}
      // ADR-0055 D2「入力欄は画面下固定、キーボード表示時に隠れない」（フェーズ 71）。ADR-0057（Phase 92）:
      // モバイル版は常に `~/root.tsx` の `<Outlet/>` の外（`MobileTabBar` と同じ階層）でレンダーされるので、
      // ここでの `fixed` は「アニメーションする祖先の中の fixed」にはもうなり得ない。デスクトップ版は
      // `Console` の右カラムの中で通常フロー（`static`）のまま（見た目は Phase 91 までと不変）。
      className={cn(
        variant === "mobile"
          ? "fixed inset-x-0 bottom-16 z-20 space-y-2 border-t border-border bg-surface/95 px-4 py-3 backdrop-blur-xl lg:hidden"
          : "hidden space-y-2 lg:block",
      )}
    >
      <ErrorFlash error={error} />
      {/* ADR-0054 D3（Phase 68）: 「この案件の文脈で話す」— 案件の画面（`scope=project:<id>`）を見ながら
          打つと、返信先を選んでいなくてもその案件に紐づいた CoS への発言になる（`buildInstructBody` の
          規則 3）。返信先バナーが出ているときは規則 2 が勝つので、二重に出さない。 */}
      {!replyTarget && defaultScope?.startsWith("project:") && (
        <p className="flex items-center gap-2 text-xs text-fg-subtle" data-testid="console-scope-context">
          <Badge tone="info">
            この案件の文脈で話す: {projectName(defaultScope.slice("project:".length), projects) ?? "案件"}
          </Badge>
        </p>
      )}
      {replyTarget && (
        <p className="flex items-center gap-2 text-xs text-fg-subtle" data-testid="console-reply-target">
          <Badge tone="info">
            返信先: {replyTarget.kind === "node" ? orgNodeName(replyTarget.nodeId, org) : "この案件の CoS"}
          </Badge>
          <button
            type="button"
            onClick={() => setReplyTarget(null)}
            className="min-h-11 px-2 underline underline-offset-2"
          >
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
      <div className="flex flex-wrap items-center justify-between gap-2">
        {/* ADR-0054 D2「入力欄は run 中も打てる（キューに入り、run が終わってから次の run になる）」の
            見た目（フェーズ 73）。「送る」が押せることに変わりはない（無効化しない）— 次の run になる
            だけなので、控えめな 1 行で伝えるだけにする。 */}
        {streaming ? (
          <p className="text-sm text-fg-subtle lg:text-xs" data-testid="console-queue-hint">
            送信待ち（前の run が終わってから）
          </p>
        ) : (
          <span />
        )}
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
    </div>
  );
}
