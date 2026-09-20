import { useEffect } from "react";
import { useRevalidator } from "react-router";

/**
 * SSE クライアント（docs/DESIGN.md §6.3 の 3、docs/adr/0004-g1-decisions.md D2）。
 * `createStreamController` は `EventSource` に依存しないプレーンなデバウンス制御で、jsdom 無しにテストできる。
 * `useCelerisStream` はそれを `/events` の `EventSource` に配線する React フック。
 */

const DEFAULT_DEBOUNCE_MS = 250;
const STREAM_EVENT_TYPES = ["task.event", "daemon", "reset"] as const;

export interface StreamController {
  /** イベント種別を受け取る。タイマーが無ければ `debounceMs` 後に 1 回 `revalidate()` を呼ぶタイマーを開始する。 */
  notify(eventType: string): void;
  /** タイマーを止める。以後 `notify` が呼ばれても `revalidate` は呼ばれない。 */
  dispose(): void;
}

/**
 * `EventSource` や DOM に依存しない純粋なデバウンス制御（テストは `vi.useFakeTimers()` で行う）。
 *
 * `notify` のたびにタイマーを**リセットしない**（純粋な trailing debounce にしない）。`daemon` イベントは
 * tick のたびに届く（fixture では `tick_ms = 200ms` で `debounceMs`（既定 250ms）より短い）ため、リセット式だと
 * デーモンが動き続ける限りタイマーが一生満了せず `revalidate` が呼ばれない不具合になる（実機で確認済み）。
 * 代わりに「タイマーが無ければ開始する」方式にし、`debounceMs` ごとに高々 1 回発火するスロットルにする。
 */
export function createStreamController(revalidate: () => void, debounceMs = DEFAULT_DEBOUNCE_MS): StreamController {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  return {
    notify(_eventType: string) {
      if (disposed || timer !== null) return;
      timer = setTimeout(() => {
        timer = null;
        revalidate();
      }, debounceMs);
    },
    dispose() {
      disposed = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
    },
  };
}

export interface UseCelerisStreamOptions {
  taskId?: string | null;
  /** false なら張らない（未認証で /login を描画しているとき）。既定 true */
  enabled?: boolean;
}

/** `/events` に対する `EventSource` を張り、`task.event` / `daemon` / `reset` を受けたら再検証する。root で 1 回だけ呼ぶ。 */
export function useCelerisStream(options?: UseCelerisStreamOptions): void {
  const revalidator = useRevalidator();
  const taskId = options?.taskId;

  const enabled = options?.enabled ?? true;

  useEffect(() => {
    // 未認証（/login 描画中）は `/events` が 401 になるだけなので張らない（docs/adr/0008 D1）
    if (!enabled) return;
    const url = taskId ? `/events?task_id=${encodeURIComponent(taskId)}` : "/events";
    const eventSource = new EventSource(url);
    const controller = createStreamController(() => revalidator.revalidate());

    const listeners = STREAM_EVENT_TYPES.map((eventType) => {
      const listener = () => controller.notify(eventType);
      eventSource.addEventListener(eventType, listener);
      return { eventType, listener };
    });

    return () => {
      for (const { eventType, listener } of listeners) {
        eventSource.removeEventListener(eventType, listener);
      }
      eventSource.close();
      controller.dispose();
    };
    // `revalidator` は `useRevalidator()` が返す安定した参照ではないため依存から外す（再接続は `taskId` の変化だけで十分）。
  }, [taskId, enabled, revalidator.revalidate]);
}
