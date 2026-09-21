import type { ConsoleReplyStep, Event, TimelineItem } from "~/celeris/types";

/**
 * `/tasks/:id` タイムラインタブ（ADR-0044 D5）の純粋関数。DOM を描画する unit テストがこのリポジトリに
 * 無い（G10-U1）ので、`~/lib/task-changes.ts` / `~/lib/console.ts` と同じ考え方で、判断・組み立てはここに
 * 集める。
 *
 * フェーズ 74（ADR-0055 D2 ラウンド 6）: `worker_progress` イベント（ADR-0048 D2 の構造化フィールド
 * `kind`/`tool`/`summary`/`detail`/`error`）は、これまでタイムラインの 1 行ずつに生の `msg` を出すだけ
 * だった。Console（`~/components/ConsoleBlockItem.tsx::ProgressBlockView`）は同じ構造化フィールドを
 * run ごとに束ねて既定で折り畳む（ADR-0048 D2「既定は折り畳み」）。タイムラインには celeris 側の
 * run 単位の集約が無いので、GUI 側で「連続する worker_progress イベント」を 1 つの折り畳みにまとめる。
 */

type WorkerProgressEvent = Extract<Event, { type: "worker_progress" }>;
export type TimelineWorkerProgressItem = Extract<TimelineItem, { kind: "event" }> & { event: WorkerProgressEvent };

export type TimelineDisplayItem =
  | { kind: "item"; item: TimelineItem }
  | { kind: "progress_group"; items: TimelineWorkerProgressItem[] };

function isWorkerProgressItem(item: TimelineItem): item is TimelineWorkerProgressItem {
  return item.kind === "event" && item.event.type === "worker_progress";
}

/**
 * 連続する `worker_progress` イベントを 1 つの `progress_group` にまとめる（それ以外の種類はそのまま
 * `item`）。1 件だけの `worker_progress` も `progress_group`（`items.length === 1`）にする
 * （タイムラインの見た目をタイプで分けない。折り畳みの有無は呼び出し側が `items.length` で決める）。
 */
export function groupTimelineWorkerProgress(items: readonly TimelineItem[]): TimelineDisplayItem[] {
  const out: TimelineDisplayItem[] = [];
  let buf: TimelineWorkerProgressItem[] = [];
  const flush = () => {
    if (buf.length > 0) {
      out.push({ kind: "progress_group", items: buf });
      buf = [];
    }
  };
  for (const item of items) {
    if (isWorkerProgressItem(item)) {
      buf.push(item);
    } else {
      flush();
      out.push({ kind: "item", item });
    }
  }
  flush();
  return out;
}

/** 折り畳みの一行要約（ADR-0048 D2 の折り畳みの見出しに倣う: 最後の進行の kind + 件数）。時刻は呼び出し側。 */
export function timelineProgressGroupSummary(items: readonly TimelineWorkerProgressItem[]): string {
  const last = items[items.length - 1];
  const lastKind = last?.event.kind ?? "status";
  return `${items.length} 件 ・ 最後: ${lastKind}`;
}

/**
 * `Event`（`worker_progress`）を Console の `ConsoleReplyStep` と同じ形にする（`~/components/
 * ConsoleBlockItem.tsx::ReplyStepRow` をそのまま再利用するため。`~/lib/console.ts::formatRunEventRow`
 * の `summary ?? msg` と同じ規則）。
 */
export function workerProgressStep(event: WorkerProgressEvent): ConsoleReplyStep {
  return {
    kind: event.kind ?? "status",
    text: event.summary ?? event.msg,
    tool: event.tool ?? null,
    error: event.error ?? false,
  };
}
