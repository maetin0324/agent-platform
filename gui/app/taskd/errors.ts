import type { ActionError } from "./action-types";

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

/**
 * ErrorBoundary に渡す構造化データ（docs/adr/0004-g1-decisions.md D6）。
 * React Router の本番ビルドは、loader が投げた「素の Error」を ErrorBoundary に渡す前に
 * 汎用の 500（`Unexpected Server Error`）にサニタイズする。`TaskdUnavailable` / `TaskdError` を
 * そのまま投げても本番では判別できないため、`Response` として投げて `isRouteErrorResponse` /
 * `error.data` 経由で判別できるようにする（`Response` は例外的にサニタイズされない）。
 */
export interface TaskdRouteErrorData {
  kind: "unavailable" | "taskd_error";
  baseUrl?: string;
  status?: number;
  code?: string;
  detail?: string;
}

/**
 * `TaskdUnavailable` / `TaskdError` を、loader から投げる（または resource route がそのまま返す）ための
 * `Response` に変換する。それ以外の例外はそのまま re-throw する（真に予期しないエラーは 500 のままでよい）。
 */
export function taskdErrorResponse(e: unknown): Response {
  if (isTaskdUnavailable(e)) {
    const data: TaskdRouteErrorData = { kind: "unavailable", baseUrl: e.baseUrl };
    return new Response(JSON.stringify(data), { status: 503, headers: { "Content-Type": "application/json" } });
  }
  if (e instanceof TaskdError) {
    const data: TaskdRouteErrorData = { kind: "taskd_error", status: e.status, code: e.code, detail: e.detail };
    return new Response(JSON.stringify(data), { status: e.status, headers: { "Content-Type": "application/json" } });
  }
  throw e;
}

/** `TaskdError` / `TaskdUnavailable` を `ActionError` にする。それ以外は re-throw（本当に予期しないエラー）。 */
export function toActionError(e: unknown): ActionError {
  if (isTaskdUnavailable(e)) {
    return {
      status: 503,
      code: "unavailable",
      detail: `taskd に接続できません（${e.baseUrl}）`,
      conflict: false,
      fields: {},
      messages: [],
    };
  }
  if (e instanceof TaskdError) {
    const fields: Record<string, string[]> = {};
    const messages: string[] = [];
    const errors = e.extra.errors;
    if (Array.isArray(errors)) {
      for (const item of errors) {
        if (!item || typeof item !== "object") continue;
        const message = (item as { message?: unknown }).message;
        if (typeof message !== "string") continue;
        messages.push(message);
        const field = (item as { field?: unknown }).field;
        if (typeof field === "string" && field) {
          fields[field] ??= [];
          fields[field].push(message);
        }
      }
    }
    return {
      status: e.status,
      code: e.code,
      detail: e.detail,
      conflict: e.status === 409,
      fields,
      messages,
    };
  }
  throw e;
}
