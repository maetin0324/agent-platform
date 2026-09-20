import type { Status, TaskSummary } from "~/celeris/types";

/**
 * ボード（`/board`）の純粋なヘルパー（ADR-0044 D3 / D4）。
 *
 * ここにあるのは **celeris が決めた対応をそのまま写す表** と、**URL ↔ フィルタの読み書き**だけ。
 * 判断（どの状態からどこへ動けるか、どの run を起こすか）は一切しない。
 * 優先度の対応（P0 = 30 / P1 = 20 / P2 = 10 / P3 = 0、丸めは `>= 30 → P0`…）は
 * celeris の `task_core::PRIORITY_LABELS` / `priority_label` と同じ規則。API は `priority_label` を
 * 返すので画面はラベルだけを扱えばよいが、**並べ替えのために整数が要る**ので写しを置く
 * （`docs/adr/0044-task-management.md` D3）。
 */

/** ADR-0044 D3 の優先度ラベル（大きいほど先。P0 が最優先）。 */
export const PRIORITY_LABELS = ["P0", "P1", "P2", "P3"] as const;
export type PriorityLabel = (typeof PRIORITY_LABELS)[number];

/** ADR-0044 D3: ラベル → `Task.priority`（`i32`）。 */
export const PRIORITY_VALUES: Record<PriorityLabel, number> = { P0: 30, P1: 20, P2: 10, P3: 0 };

/** 既定の優先度（celeris の `task_core::DEFAULT_PRIORITY` = 10 = P2）。 */
export const DEFAULT_PRIORITY = PRIORITY_VALUES.P2;

export function isPriorityLabel(v: unknown): v is PriorityLabel {
  return typeof v === "string" && (PRIORITY_LABELS as readonly string[]).includes(v);
}

/**
 * ラベル（`"P1"`）を `Task.priority` の整数に写す。知らない値は既定（P2 = 10）。
 * celeris の `priority_from_label` と同じ（大文字小文字は区別しない）。
 */
export function priorityValue(label: string): number {
  const upper = label.trim().toUpperCase();
  return isPriorityLabel(upper) ? PRIORITY_VALUES[upper] : DEFAULT_PRIORITY;
}

/**
 * `Task.priority`（`i32`）を P0〜P3 に丸める。celeris の `priority_label` と**同じ境界**
 * （30 以上 = P0、20 以上 = P1、10 以上 = P2、それ未満 = P3）。
 * 一覧・詳細は celeris が返す `priority_label` をそのまま出すので、これを使うのは
 * `priority_label` が無い古い応答の保険と、並べ替え前の正規化だけ。
 */
export function priorityLabelOf(priority: number): PriorityLabel {
  if (priority >= PRIORITY_VALUES.P0) return "P0";
  if (priority >= PRIORITY_VALUES.P1) return "P1";
  if (priority >= PRIORITY_VALUES.P2) return "P2";
  return "P3";
}

/** 行（`TaskSummary`）の優先度ラベル。celeris の `priority_label` があればそれを使う。 */
export function summaryPriorityLabel(item: Pick<TaskSummary, "priority" | "priority_label">): PriorityLabel {
  return isPriorityLabel(item.priority_label) ? item.priority_label : priorityLabelOf(item.priority);
}

/** ADR-0044 D4 のボードの列。**状態は状態機械の仕事**なので、ドラッグでの列跨ぎはしない（表示の束ね方だけ）。 */
export type BoardColumnId = "waiting" | "in_progress" | "blocked" | "done" | "failed" | "cancelled";

export const BOARD_COLUMNS: readonly { id: BoardColumnId; statuses: readonly Status[] }[] = [
  { id: "waiting", statuses: ["draft", "ready"] },
  { id: "in_progress", statuses: ["running", "reviewing"] },
  { id: "blocked", statuses: ["blocked"] },
  { id: "done", statuses: ["done"] },
  { id: "failed", statuses: ["failed"] },
  { id: "cancelled", statuses: ["cancelled"] },
] as const;

const COLUMN_OF_STATUS: Record<string, BoardColumnId> = Object.fromEntries(
  BOARD_COLUMNS.flatMap((col) => col.statuses.map((s) => [s, col.id] as const)),
);

/** 状態 → 列。知らない状態（将来 celeris が足したもの）は `null`（どの列にも出さない）。 */
export function boardColumnOf(status: Status | string): BoardColumnId | null {
  return COLUMN_OF_STATUS[status] ?? null;
}

/**
 * 列の中の並び（ADR-0044 D4「並べ替えは優先度で」）: 優先度の降順 → `created_at` の昇順。
 * 同点は id で決定的にする（SSE の再検証で順が揺れないように）。
 */
export function compareBoardCards(a: TaskSummary, b: TaskSummary): number {
  const pa = PRIORITY_VALUES[summaryPriorityLabel(a)];
  const pb = PRIORITY_VALUES[summaryPriorityLabel(b)];
  if (pa !== pb) return pb - pa;
  if (a.created_at !== b.created_at) return a.created_at < b.created_at ? -1 : 1;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/** タスクを 6 列に束ねて、各列を `compareBoardCards` で並べる。 */
export function groupByColumn(items: readonly TaskSummary[]): Record<BoardColumnId, TaskSummary[]> {
  const out: Record<BoardColumnId, TaskSummary[]> = {
    waiting: [],
    in_progress: [],
    blocked: [],
    done: [],
    failed: [],
    cancelled: [],
  };
  for (const item of items) {
    const column = boardColumnOf(item.status);
    if (column) out[column].push(item);
  }
  for (const column of Object.keys(out) as BoardColumnId[]) {
    out[column].sort(compareBoardCards);
  }
  return out;
}

/**
 * ADR-0044 D4 のフィルタ。**URL がそのまま状態**なので、`GET /tasks` のクエリ名と 1:1 にする
 * （`label` / `category` / `tier` / `priority` は繰り返し可、`project` / `assignee` / `milestone` / `q` は単一）。
 */
export interface BoardFilter {
  project: string | null;
  labels: string[];
  categories: string[];
  assignee: string | null;
  milestone: string | null;
  tiers: string[];
  priorities: string[];
  q: string | null;
}

export const EMPTY_BOARD_FILTER: BoardFilter = {
  project: null,
  labels: [],
  categories: [],
  assignee: null,
  milestone: null,
  tiers: [],
  priorities: [],
  q: null,
};

function single(params: URLSearchParams, name: string): string | null {
  const v = params.get(name);
  return v === null || v === "" ? null : v;
}

function repeated(params: URLSearchParams, name: string): string[] {
  return params.getAll(name).filter((v) => v !== "");
}

/** `URLSearchParams` → `BoardFilter`（空文字の欄は「指定なし」として落とす）。 */
export function parseBoardFilter(params: URLSearchParams): BoardFilter {
  return {
    project: single(params, "project"),
    labels: repeated(params, "label"),
    categories: repeated(params, "category"),
    assignee: single(params, "assignee"),
    milestone: single(params, "milestone"),
    tiers: repeated(params, "tier"),
    priorities: repeated(params, "priority"),
    q: single(params, "q"),
  };
}

/** `BoardFilter` → `URLSearchParams`（`parseBoardFilter` の逆。指定なしの欄は付けない）。 */
export function boardFilterToParams(filter: BoardFilter): URLSearchParams {
  const params = new URLSearchParams();
  if (filter.project) params.set("project", filter.project);
  for (const label of filter.labels) params.append("label", label);
  for (const category of filter.categories) params.append("category", category);
  if (filter.assignee) params.set("assignee", filter.assignee);
  if (filter.milestone) params.set("milestone", filter.milestone);
  for (const tier of filter.tiers) params.append("tier", tier);
  for (const priority of filter.priorities) params.append("priority", priority);
  if (filter.q) params.set("q", filter.q);
  return params;
}

/**
 * `BoardFilter` → `CelerisClient.get` の `query`（`GET /tasks`。ADR-0044 D4）。
 * 値はそのまま転送する（GUI 側で検証しない。知らない値は celeris が 400 を返し、その文言を画面に出す）。
 */
export function boardFilterToQuery(filter: BoardFilter): Record<string, string | string[] | undefined> {
  return {
    project: filter.project ?? undefined,
    label: filter.labels,
    category: filter.categories,
    assignee: filter.assignee ?? undefined,
    milestone: filter.milestone ?? undefined,
    tier: filter.tiers,
    priority: filter.priorities,
    q: filter.q ?? undefined,
  };
}

/** フィルタが 1 つでも指定されているか（空のボードの案内文の出し分けに使う）。 */
export function boardFilterIsEmpty(filter: BoardFilter): boolean {
  return boardFilterToParams(filter).toString() === "";
}

/**
 * ADR-0044 D3 のラベルの形（小文字・`[a-z0-9-]`）。**検証の正は celeris**（422 の文言をそのまま出す）で、
 * ここは入力補助（チップを足す前に弾いてやり直させる）にだけ使う。
 */
const LABEL_RE = /^[a-z0-9-]+$/;

export function isValidLabel(label: string): boolean {
  return LABEL_RE.test(label);
}

/** ADR-0044 D3: ラベルは最大 8 個。 */
export const MAX_LABELS = 8;
