import { useEffect, useRef } from "react";
import type { ConsoleBlock, ConsoleHello } from "~/celeris/types";

/**
 * `/console/stream` の SSE クライアント（ADR-0048 D1、GUI Phase G22）。`~/hooks/useCelerisStream.ts` と同じ
 * 分け方: `createConsoleStreamController` は `EventSource` に依存しないプレーンな JSON 解釈 + カーソルの
 * 追跡で、jsdom 無しにテストできる。`useConsoleStream` はそれを実際の `EventSource` に配線する React フック。
 *
 * `useCelerisStream`（root で 1 本、`task.event`/`daemon`/`reset` を受けたら**全ルートを再検証**）とは別物:
 * Console は D1 が「block ごとに積み増す」ことを求めているので、専用の接続で `console.block` を 1 件ずつ
 * 受け取り、呼び出し側（`~/components/Console.tsx`）がローカルの一覧に足す（`~/lib/console.ts` の
 * `appendConsoleBlock`）。ページ全体の再検証はしない。
 */

export function parseConsoleHello(raw: string): ConsoleHello | null {
  try {
    return JSON.parse(raw) as ConsoleHello;
  } catch {
    return null;
  }
}

export function parseConsoleBlock(raw: string): ConsoleBlock | null {
  try {
    return JSON.parse(raw) as ConsoleBlock;
  } catch {
    return null;
  }
}

export interface ConsoleStreamHandlers {
  onHello?(hello: ConsoleHello): void;
  onBlock(block: ConsoleBlock): void;
}

export interface ConsoleStreamController {
  /** `event: hello` の `data`（生の JSON 文字列）。 */
  handleHello(raw: string): void;
  /** `event: console.block` の `data`（生の JSON 文字列）。 */
  handleBlock(raw: string): void;
  /** いまのカーソル（次に張り直すときの `since`。何も受け取っていなければ最初に渡した値）。 */
  cursor(): string | null;
}

/**
 * `EventSource` に依存しない純粋な制御。壊れた JSON（プロトコルの不一致・切断中の半端なフレーム）は
 * 静かに無視する（1 フレーム壊れても以後のフレームは受け取り続ける）。
 */
export function createConsoleStreamController(
  handlers: ConsoleStreamHandlers,
  initialCursor: string | null = null,
): ConsoleStreamController {
  let cursor = initialCursor;
  return {
    handleHello(raw) {
      const hello = parseConsoleHello(raw);
      if (!hello) return;
      cursor = hello.cursor;
      handlers.onHello?.(hello);
    },
    handleBlock(raw) {
      const block = parseConsoleBlock(raw);
      if (!block) return;
      cursor = block.cursor;
      handlers.onBlock(block);
    },
    cursor() {
      return cursor;
    },
  };
}

const RECONNECT_MS = 1_000;

export interface UseConsoleStreamOptions extends ConsoleStreamHandlers {
  /** `all` / `project:<id>` / `node:<id>`。変わるたびに張り直す。 */
  scope: string;
  /** 最初の接続だけに使う `since`（省略すれば「今」から）。以後の再接続は受け取った最新のカーソルを使う。 */
  since?: string | null;
  /** false なら張らない（未認証時など）。既定 true。 */
  enabled?: boolean;
}

/**
 * `/console/stream?scope=&since=` に `EventSource` を張り、`console.block` のたびに `onBlock` を呼ぶ。
 * 切断（`onerror`）は `EventSource` の既定の自動再接続に任せず、自前で閉じて最新のカーソルから張り直す
 * （既定の再接続だと最初に渡した `since` のままになり、切断中に流れたぶんを取りこぼす）。
 */
export function useConsoleStream(options: UseConsoleStreamOptions): void {
  const { scope, enabled = true } = options;
  const onBlockRef = useRef(options.onBlock);
  const onHelloRef = useRef(options.onHello);
  // `since` は ref で持つ（`useCelerisStream.ts` の `revalidator.revalidate` と同じ考え方: 依存配列に
  // 生の値を挙げると変わるたびに張り直してしまう。`scope` が変わって次に接続するときの最新値を使えれば
  // 十分なので、レンダーのたびに更新するだけにし、effect の依存には入れない）。
  const sinceRef = useRef(options.since ?? null);
  onBlockRef.current = options.onBlock;
  onHelloRef.current = options.onHello;
  sinceRef.current = options.since ?? sinceRef.current;

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    let source: EventSource | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    const controller = createConsoleStreamController(
      {
        onBlock: (block) => onBlockRef.current(block),
        onHello: (hello) => onHelloRef.current?.(hello),
      },
      sinceRef.current,
    );

    function connect() {
      if (cancelled) return;
      const params = new URLSearchParams({ scope });
      const cur = controller.cursor();
      if (cur) params.set("since", cur);
      const es = new EventSource(`/console/stream?${params.toString()}`);
      source = es;
      es.addEventListener("hello", (ev) => controller.handleHello((ev as MessageEvent<string>).data));
      es.addEventListener("console.block", (ev) => controller.handleBlock((ev as MessageEvent<string>).data));
      es.onerror = () => {
        if (cancelled) return;
        es.close();
        retryTimer = setTimeout(connect, RECONNECT_MS);
      };
    }
    connect();

    return () => {
      cancelled = true;
      if (retryTimer !== null) clearTimeout(retryTimer);
      source?.close();
    };
  }, [scope, enabled]);
}
