/**
 * TaskdClient のエラー型（docs/DESIGN.md §6.3）。サーバ・クライアント両方から import できる（Node 専用 API を使わない）。
 * - TaskdError: taskd が application/problem+json（docs/taskd-api-v1.md §1.5）で返したエラー
 * - TaskdUnavailable: 接続拒否・タイムアウト等で taskd に届かなかった
 */
export interface TaskdErrorInit {
  status: number;
  code: string;
  detail: string;
  title?: string;
  type?: string;
  instance?: string;
  extra?: Record<string, unknown>;
}

export class TaskdError extends Error {
  override readonly name = "TaskdError";
  readonly status: number;
  readonly code: string;
  readonly detail: string;
  readonly title: string | undefined;
  readonly type: string | undefined;
  readonly instance: string | undefined;
  /** Problem の追加フィールド（`expected` / `actual` / `errors[]` / `task_status` など） */
  readonly extra: Record<string, unknown>;

  constructor(init: TaskdErrorInit) {
    super(`taskd ${init.status} ${init.code}: ${init.detail}`);
    this.status = init.status;
    this.code = init.code;
    this.detail = init.detail;
    this.title = init.title;
    this.type = init.type;
    this.instance = init.instance;
    this.extra = init.extra ?? {};
  }
}

export class TaskdUnavailable extends Error {
  override readonly name = "TaskdUnavailable";
  readonly baseUrl: string;

  constructor(baseUrl: string, cause?: unknown) {
    const reason = cause instanceof Error ? cause.message : String(cause ?? "unknown");
    super(`taskd unavailable at ${baseUrl}: ${reason}`, { cause });
    this.baseUrl = baseUrl;
  }
}

export function isTaskdUnavailable(e: unknown): e is TaskdUnavailable {
  return e instanceof TaskdUnavailable || (e instanceof Error && e.name === "TaskdUnavailable");
}
