import { readFileSync } from "node:fs";
import { TaskdError, TaskdUnavailable } from "./errors";
import type { Health, Problem } from "./types";

/**
 * taskd HTTP API v1 のクライアント（docs/DESIGN.md §6.3、docs/taskd-api-v1.md）。BFF（loader / action / resource route）だけが使う。
 * - `api-v1.schema.json` から生成した型（./types）だけを使う。応答の実行時検証はしない（docs/adr/0002 D9）
 * - application/problem+json は TaskdError に、接続失敗・タイムアウトは TaskdUnavailable にする
 * - トークンは `TASKD_API_TOKEN_FILE` から起動時に読み、メモリに持つ。ブラウザには渡さない（§8.1）
 * - `Host` / `Origin` は転送しない（fetch は URL から Host を組み立て、Node の fetch は Origin を送らない）
 */

export type QueryValue = string | number | boolean | null | undefined | readonly (string | number)[];
export type Query = Record<string, QueryValue>;

export interface TaskdClientOptions {
  /** 既定 http://127.0.0.1:7710（`/api/v1` は付けない） */
  baseUrl?: string;
  token?: string | null;
  /** get / post / file の接続〜応答ヘッダまでの上限。stream には適用しない */
  timeoutMs?: number;
  fetchImpl?: typeof fetch;
}

export interface RequestOptions {
  query?: Query;
  signal?: AbortSignal;
}

export interface StreamOptions {
  taskId?: string | null;
  lastEventId?: string | null;
  signal?: AbortSignal;
}

export interface FileOptions {
  /** `Range: bytes=a-b` をそのまま転送 */
  range?: string | null;
  offset?: number;
  length?: number;
  download?: boolean;
  signal?: AbortSignal;
}

export const DEFAULT_TASKD_API_URL = "http://127.0.0.1:7710";
const DEFAULT_TIMEOUT_MS = 15_000;

export class TaskdClient {
  readonly baseUrl: string;
  readonly #token: string | null;
  readonly #timeoutMs: number;
  readonly #fetch: typeof fetch;

  constructor(options: TaskdClientOptions = {}) {
    this.baseUrl = (options.baseUrl ?? DEFAULT_TASKD_API_URL).replace(/\/+$/, "");
    this.#token = options.token ?? null;
    this.#timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
    this.#fetch = options.fetchImpl ?? fetch;
  }

  /** `TASKD_API_URL` / `TASKD_API_TOKEN_FILE` から作る。トークンファイルが指定されていて読めない・空なら例外（起動時に気づく）。 */
  static fromEnv(env: NodeJS.ProcessEnv = process.env): TaskdClient {
    const baseUrl = env.TASKD_API_URL || DEFAULT_TASKD_API_URL;
    let token: string | null = null;
    const tokenFile = env.TASKD_API_TOKEN_FILE;
    if (tokenFile) {
      token = readFileSync(tokenFile, "utf8").trim();
      if (!token) throw new Error(`TASKD_API_TOKEN_FILE (${tokenFile}) is empty`);
    }
    return new TaskdClient({ baseUrl, token });
  }

  get hasToken(): boolean {
    return this.#token !== null;
  }

  url(path: string, query?: Query): URL {
    const u = new URL(`${this.baseUrl}/api/v1${path.startsWith("/") ? path : `/${path}`}`);
    if (query) {
      for (const [k, v] of Object.entries(query)) {
        if (v === undefined || v === null) continue;
        if (Array.isArray(v)) for (const item of v) u.searchParams.append(k, String(item));
        else u.searchParams.set(k, String(v));
      }
    }
    return u;
  }

  #headers(extra: Record<string, string>): Headers {
    const h = new Headers(extra);
    if (this.#token) h.set("Authorization", `Bearer ${this.#token}`);
    return h;
  }

  async #send(url: URL, init: RequestInit, timeoutMs: number | null): Promise<Response> {
    const signals: AbortSignal[] = [];
    if (init.signal) signals.push(init.signal);
    if (timeoutMs !== null) signals.push(AbortSignal.timeout(timeoutMs));
    const signal = signals.length > 0 ? AbortSignal.any(signals) : undefined;
    let res: Response;
    try {
      res = await this.#fetch(url, { ...init, signal, redirect: "manual" });
    } catch (e) {
      throw new TaskdUnavailable(this.baseUrl, e);
    }
    if (!res.ok) throw await problemFromResponse(res);
    return res;
  }

  async get<T>(path: string, options: RequestOptions = {}): Promise<T> {
    const res = await this.#send(
      this.url(path, options.query),
      { method: "GET", headers: this.#headers({ Accept: "application/json" }), signal: options.signal },
      this.#timeoutMs,
    );
    return (await res.json()) as T;
  }

  async post<T>(path: string, body: unknown = {}, options: RequestOptions = {}): Promise<T> {
    const res = await this.#send(
      this.url(path, options.query),
      {
        method: "POST",
        headers: this.#headers({ Accept: "application/json", "Content-Type": "application/json" }),
        body: JSON.stringify(body ?? {}),
        signal: options.signal,
      },
      this.#timeoutMs,
    );
    return (await res.json()) as T;
  }

  async patch<T>(path: string, body: unknown = {}, options: RequestOptions = {}): Promise<T> {
    const res = await this.#send(
      this.url(path, options.query),
      {
        method: "PATCH",
        headers: this.#headers({ Accept: "application/json", "Content-Type": "application/json" }),
        body: JSON.stringify(body ?? {}),
        signal: options.signal,
      },
      this.#timeoutMs,
    );
    return (await res.json()) as T;
  }

  async delete<T>(path: string, options: RequestOptions = {}): Promise<T> {
    const res = await this.#send(
      this.url(path, options.query),
      { method: "DELETE", headers: this.#headers({ Accept: "application/json" }), signal: options.signal },
      this.#timeoutMs,
    );
    return (await res.json()) as T;
  }

  /** `GET /stream`（SSE）。応答をそのまま返す（body は ReadableStream）。タイムアウトは掛けない。切断は signal で行う。 */
  async stream(options: StreamOptions = {}): Promise<Response> {
    const extra: Record<string, string> = { Accept: "text/event-stream" };
    if (options.lastEventId) extra["Last-Event-ID"] = options.lastEventId;
    return this.#send(
      this.url("/stream", { task_id: options.taskId ?? undefined }),
      { method: "GET", headers: this.#headers(extra), signal: options.signal },
      null,
    );
  }

  /** ファイル系（run のログ・result・成果物本体）。応答をヘッダごと返す。 */
  async file(path: string, options: FileOptions = {}): Promise<Response> {
    const extra: Record<string, string> = { Accept: "*/*" };
    if (options.range) extra.Range = options.range;
    return this.#send(
      this.url(path, { offset: options.offset, length: options.length, download: options.download ? 1 : undefined }),
      { method: "GET", headers: this.#headers(extra), signal: options.signal },
      this.#timeoutMs,
    );
  }

  health(options: RequestOptions = {}): Promise<Health> {
    return this.get<Health>("/health", options);
  }
}

async function problemFromResponse(res: Response): Promise<TaskdError> {
  const contentType = res.headers.get("content-type") ?? "";
  if (/application\/(problem\+)?json/i.test(contentType)) {
    try {
      const p = (await res.json()) as Partial<Problem> & Record<string, unknown>;
      const { type, title, status, detail, code, instance, ...extra } = p;
      return new TaskdError({
        status: typeof status === "number" ? status : res.status,
        code: typeof code === "string" ? code : "unknown",
        detail: typeof detail === "string" ? detail : res.statusText,
        title: typeof title === "string" ? title : undefined,
        type: typeof type === "string" ? type : undefined,
        instance: typeof instance === "string" ? instance : undefined,
        extra,
      });
    } catch {
      // JSON として読めない: 下の text 扱いへ
    }
  }
  let text = "";
  try {
    text = (await res.text()).slice(0, 200);
  } catch {
    text = "";
  }
  return new TaskdError({
    status: res.status,
    code: "unknown",
    detail: text || res.statusText || `HTTP ${res.status}`,
  });
}

let shared: TaskdClient | undefined;

/** プロセスで 1 つの TaskdClient（環境変数から構成）。 */
export function getTaskdClient(): TaskdClient {
  shared ??= TaskdClient.fromEnv();
  return shared;
}
