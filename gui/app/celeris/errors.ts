import type { ActionError } from "./action-types";

/**
 * CelerisClient のエラー型（docs/DESIGN.md §6.3）。サーバ・クライアント両方から import できる（Node 専用 API を使わない）。
 * - CelerisError: celeris が application/problem+json（docs/celeris-api-v1.md §1.5）で返したエラー
 * - CelerisUnavailable: 接続拒否・タイムアウト等で celeris に届かなかった
 */
export interface CelerisErrorInit {
  status: number;
  code: string;
  detail: string;
  title?: string;
  type?: string;
  instance?: string;
  extra?: Record<string, unknown>;
}

export class CelerisError extends Error {
  override readonly name = "CelerisError";
  readonly status: number;
  readonly code: string;
  readonly detail: string;
  readonly title: string | undefined;
  readonly type: string | undefined;
  readonly instance: string | undefined;
  /** Problem の追加フィールド（`expected` / `actual` / `errors[]` / `task_status` など） */
  readonly extra: Record<string, unknown>;

  constructor(init: CelerisErrorInit) {
    super(`celeris ${init.status} ${init.code}: ${init.detail}`);
    this.status = init.status;
    this.code = init.code;
    this.detail = init.detail;
    this.title = init.title;
    this.type = init.type;
    this.instance = init.instance;
    this.extra = init.extra ?? {};
  }
}

export class CelerisUnavailable extends Error {
  override readonly name = "CelerisUnavailable";
  readonly baseUrl: string;

  constructor(baseUrl: string, cause?: unknown) {
    const reason = cause instanceof Error ? cause.message : String(cause ?? "unknown");
    super(`celeris unavailable at ${baseUrl}: ${reason}`, { cause });
    this.baseUrl = baseUrl;
  }
}

export function isCelerisUnavailable(e: unknown): e is CelerisUnavailable {
  return e instanceof CelerisUnavailable || (e instanceof Error && e.name === "CelerisUnavailable");
}

/**
 * ErrorBoundary に渡す構造化データ（docs/adr/0004-g1-decisions.md D6）。
 * React Router の本番ビルドは、loader が投げた「素の Error」を ErrorBoundary に渡す前に
 * 汎用の 500（`Unexpected Server Error`）にサニタイズする。`CelerisUnavailable` / `CelerisError` を
 * そのまま投げても本番では判別できないため、`Response` として投げて `isRouteErrorResponse` /
 * `error.data` 経由で判別できるようにする（`Response` は例外的にサニタイズされない）。
 */
export interface CelerisRouteErrorData {
  kind: "unavailable" | "celeris_error";
  baseUrl?: string;
  status?: number;
  code?: string;
  detail?: string;
}

/**
 * `CelerisUnavailable` / `CelerisError` を、loader から投げる（または resource route がそのまま返す）ための
 * `Response` に変換する。それ以外の例外はそのまま re-throw する（真に予期しないエラーは 500 のままでよい）。
 */
export function celerisErrorResponse(e: unknown): Response {
  if (isCelerisUnavailable(e)) {
    const data: CelerisRouteErrorData = { kind: "unavailable", baseUrl: e.baseUrl };
    return new Response(JSON.stringify(data), { status: 503, headers: { "Content-Type": "application/json" } });
  }
  if (e instanceof CelerisError) {
    const data: CelerisRouteErrorData = { kind: "celeris_error", status: e.status, code: e.code, detail: e.detail };
    return new Response(JSON.stringify(data), { status: e.status, headers: { "Content-Type": "application/json" } });
  }
  throw e;
}

/** `CelerisError` / `CelerisUnavailable` を `ActionError` にする。それ以外は re-throw（本当に予期しないエラー）。 */
export function toActionError(e: unknown): ActionError {
  if (isCelerisUnavailable(e)) {
    return {
      status: 503,
      code: "unavailable",
      detail: `celeris に接続できません（${e.baseUrl}）`,
      conflict: false,
      fields: {},
      messages: [],
    };
  }
  if (e instanceof CelerisError) {
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
