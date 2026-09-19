import type {
  CommentResult,
  EditResult,
  Health,
  ReleaseChanges,
  ReleaseItem,
  ReleasePromoteAccepted,
  Releases,
  Task,
  TaskComment,
  TaskSummary,
  Timeline,
  TimelineItem,
} from "~/taskd/types";

/**
 * `GET /health` の既定応答（docs/taskd-api-v1.md §3.1）。`Health` 型で宣言することで形を検証する。
 */
export const defaultHealth: Health = {
  api_version: "1",
  schema_version: 4,
  taskd_version: "0.1.0",
  instance_id: "01MOCKTASKDINSTANCE00001",
  started_at: "2026-09-15T00:00:00Z",
  now: "2026-09-15T00:00:01Z",
  release: "dev",
  mode: "normal",
  role: "active",
  db: {
    journal_mode: "wal",
    busy_timeout_ms: 5000,
  },
};

/**
 * `GET /releases` の 1 件（ADR-0040 D6、docs/taskd-api-v1.md §3.66）。検証済み・ライブ引き継ぎ可・
 * current ではない（＝昇格できる）状態が既定。テストは `releaseItem({...})` で上書きする。
 */
export function releaseItem(overrides: Partial<ReleaseItem> = {}): ReleaseItem {
  return {
    sha12: "aaaaaaaaaaaa",
    ref: "main",
    built_at: "2026-09-19T00:00:00Z",
    schema_version: 11,
    gate_ok: true,
    verify: { ok: true, live_ok: true, at: "2026-09-19T01:00:00Z" },
    promoted_at: null,
    on_main: true,
    changes: null,
    is_current: false,
    is_previous: false,
    promoting: false,
    ...overrides,
  };
}

/**
 * `ReleaseItem.changes`（ADR-0041 D4、Phase G15）。既定は**安全に関わる変更が無い**差分。
 * 赤いバッジと sha12 入力を試すテストは `releaseChanges({sensitive: [...]})` で上書きする。
 */
export function releaseChanges(overrides: Partial<ReleaseChanges> = {}): ReleaseChanges {
  return {
    base: "aaaaaaaaaaaa",
    stale: false,
    commit_count: 2,
    file_count: 3,
    sensitive: [],
    commits: [
      { sha: "1111111111111111111111111111111111111111", subject: "phase 50: 検証の直列化" },
      { sha: "2222222222222222222222222222222222222222", subject: "adr-0041" },
    ],
    ...overrides,
  };
}

/** `GET /releases` の既定応答（current が 1 つ、引き継ぎは走っていない）。 */
export const defaultReleases: Releases = {
  current: "aaaaaaaaaaaa",
  previous: null,
  running: { release: "aaaaaaaaaaaa", role: "active", instance_id: "01MOCKTASKDINSTANCE00001" },
  instances: [
    {
      instance_id: "01MOCKTASKDINSTANCE00001",
      release: "aaaaaaaaaaaa",
      pid: 111,
      role: "active",
      started_at: "2026-09-19T00:00:00Z",
      heartbeat_at: "2026-09-19T02:00:00Z",
    },
  ],
  items: [
    releaseItem({
      sha12: "bbbbbbbbbbbb",
      built_at: "2026-09-19T08:00:00Z",
      verify: null,
      on_main: false,
      changes: releaseChanges(),
    }),
    releaseItem({ is_current: true, promoted_at: "2026-09-19T02:00:00Z" }),
  ],
};

/**
 * ADR-0044（Phase 53）のタスク管理の fixture。ボード・タイムライン・コメント・編集で使う。
 * どれも taskd が返す形（`app/taskd/types.ts`）で宣言してあるので、型が変わればここで気づく。
 */

/** ボードのカード 1 枚（`GET /tasks` の 1 行）。 */
export function taskSummary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "01BOARDTASK00000000000001",
    parent_id: null,
    kind: "execute",
    status: "ready",
    title: "関連研究を調べる",
    priority: 10,
    priority_label: "P2",
    tier: "standard",
    adapter: null,
    attempts: 0,
    max_retries: 2,
    depends_on: [],
    created_at: "2026-09-19T00:00:00Z",
    updated_at: "2026-09-19T00:00:01Z",
    lease_expires_at: null,
    backoff_until: null,
    children: 0,
    pending_children: 0,
    conversation: false,
    actions: ["cancel", "edit"],
    labels: [],
    category: "other",
    ...overrides,
  };
}

/** `PATCH /tasks/{id}` が写す先の `Task`（ADR-0044 D1）。 */
export function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "01BOARDTASK00000000000001",
    kind: "execute",
    status: "ready",
    title: "関連研究を調べる",
    objective: "3 本読む",
    priority: 10,
    attempts: 0,
    created_at: "2026-09-19T00:00:00Z",
    updated_at: "2026-09-19T00:00:02Z",
    acceptance: [],
    depends_on: [],
    inputs: [],
    worker_hint: { tier: "standard" },
    budget: { max_retries: 2, max_turns: 10, max_wall_secs: 600 },
    workspace: { kind: "local", path: "." },
    labels: [],
    category: "other",
    ...overrides,
  };
}

/** `PATCH /tasks/{id}` の既定応答（`fields` は**実際に変わった項目**）。 */
export function editResult(fields: string[] = ["priority"], overrides: Partial<Task> = {}): EditResult {
  return { task: task(overrides), fields };
}

/** `task_comments` の 1 行（ADR-0044 D2）。 */
export function taskComment(overrides: Partial<TaskComment> = {}): TaskComment {
  return {
    id: "01COMMENT0000000000000001",
    task_id: "01BOARDTASK00000000000001",
    author_kind: "human",
    body: "先に関連研究を 3 本だけ読んでください",
    created_at: "2026-09-19T00:01:00Z",
    ...overrides,
  };
}

/**
 * `POST /tasks/{id}/comments` の応答。`effect` ごとに `transition` / `can_reopen` の付き方が変わる
 * （ADR-0044 D2 の表）。
 */
export function commentResult(overrides: Partial<CommentResult> = {}): CommentResult {
  return { comment: taskComment(), effect: "stored", can_reopen: false, ...overrides };
}

/** `GET /tasks/{id}/timeline` の既定応答（ADR-0044 D5。時刻の昇順）。 */
export function timeline(items: TimelineItem[] = [], taskId = "01BOARDTASK00000000000001"): Timeline {
  return {
    task_id: taskId,
    items:
      items.length > 0
        ? items
        : [
            {
              kind: "event",
              at: "2026-09-19T00:00:00Z",
              seq: 0,
              event: { type: "transitioned", from: "draft", to: "ready", reason: "accepted" },
            },
            { kind: "comment", at: "2026-09-19T00:01:00Z", comment: taskComment() },
            {
              kind: "release",
              at: "2026-09-19T00:02:00Z",
              sha12: "aaaaaaaaaaaa",
              commits: ["1111111111111111111111111111111111111111"],
            },
          ],
  };
}

/** `POST /releases/{sha12}/promote` の既定応答（202）。 */
export const defaultReleasePromoteAccepted: ReleasePromoteAccepted = {
  sha12: "bbbbbbbbbbbb",
  log: "/home/mock/taskd/releases/bbbbbbbbbbbb/promote.log",
  started_at: "2026-09-19T10:00:00Z",
  // ADR-0041 D4: 既定は「いま動いている版に同梱の promote.sh で昇格した」。
  script_from: "current",
};
