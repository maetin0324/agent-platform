import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import type { Problem } from "~/taskd/types";
import { defaultHealth } from "./fixtures";

/**
 * プロセス内の偽 taskd（docs/adr/0002 D8）。実 taskd を起動せず、Vitest から `TaskdClient` /
 * `loadHealth` を検証するために使う。127.0.0.1 のポート 0（空きポート）に listen する。外部ネットワークには出ない。
 */

export type MockHandler = (req: IncomingMessage, res: ServerResponse, body: string) => void;

export interface MockRequestRecord {
  method: string;
  /** path + query（例: `/api/v1/tasks?status=ready`） */
  url: string;
  /** ヘッダ名は小文字（Node の `IncomingMessage.headers` そのまま） */
  headers: Record<string, string>;
  body: string;
}

export interface MockTaskd {
  baseUrl: string;
  requests: MockRequestRecord[];
  /** `path` は `/api/v1/...` の完全一致（クエリは含めない）。同じ method + path の再登録で上書き。 */
  on(method: string, path: string, handler: MockHandler): void;
  close(): Promise<void>;
}

export interface StartMockTaskdOptions {
  /** `GET /api/v1/health` の応答を差し替える（既定は `./fixtures` の `defaultHealth`） */
  health?: typeof defaultHealth;
}

const CROCKFORD_BASE32 = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/** ULID 風の 26 文字（テスト用。実 taskd の ULID との一致は保証しない）。 */
function fakeUlid(): string {
  let out = "";
  for (let i = 0; i < 26; i += 1) {
    out += CROCKFORD_BASE32[Math.floor(Math.random() * CROCKFORD_BASE32.length)];
  }
  return out;
}

function commonHeaders(requestId: string): Record<string, string> {
  return {
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
    "x-request-id": requestId,
  };
}

export function sendJson(res: ServerResponse, status: number, body: unknown): void {
  const requestId = fakeUlid();
  res.writeHead(status, {
    ...commonHeaders(requestId),
    "content-type": "application/json; charset=utf-8",
  });
  res.end(JSON.stringify(body));
}

export interface SendProblemOptions {
  status: number;
  code: string;
  detail: string;
  title?: string;
  /** `expected` / `actual` / `errors[]` / `task_status` 等、`code` ごとの付加フィールド */
  extra?: Record<string, unknown>;
}

/** `application/problem+json`（docs/taskd-api-v1.md §1.5）を返す。 */
export function sendProblem(res: ServerResponse, options: SendProblemOptions): void {
  const requestId = fakeUlid();
  const body: Problem = {
    type: `urn:taskd:problem:${options.code}`,
    title: options.title ?? options.code.replaceAll("_", " "),
    status: options.status,
    detail: options.detail,
    code: options.code,
    instance: `urn:taskd:request:${requestId}`,
    ...(options.extra ?? {}),
  };
  res.writeHead(options.status, {
    ...commonHeaders(requestId),
    "content-type": "application/problem+json; charset=utf-8",
  });
  res.end(JSON.stringify(body));
}

/** SSE 応答の先頭（`event: hello`）を書く。以後は呼び出し側が `res.write` で自由に流す（docs/taskd-api-v1.md §4）。 */
export function sendSseHello(res: ServerResponse, data: unknown): void {
  const requestId = fakeUlid();
  res.writeHead(200, {
    ...commonHeaders(requestId),
    "content-type": "text/event-stream",
  });
  res.write(`event: hello\ndata: ${JSON.stringify(data)}\n\n`);
}

function collectBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (chunk: Buffer) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

function headerRecord(req: IncomingMessage): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [name, value] of Object.entries(req.headers)) {
    if (typeof value === "string") out[name] = value;
    else if (Array.isArray(value)) out[name] = value.join(", ");
  }
  return out;
}

export async function startMockTaskd(options: StartMockTaskdOptions = {}): Promise<MockTaskd> {
  const routes = new Map<string, MockHandler>();
  const requests: MockRequestRecord[] = [];

  const routeKey = (method: string, path: string) => `${method.toUpperCase()} ${path}`;

  const on = (method: string, path: string, handler: MockHandler): void => {
    routes.set(routeKey(method, path), handler);
  };

  const server: Server = createServer((req, res) => {
    collectBody(req)
      .then((body) => {
        const method = req.method ?? "GET";
        const rawUrl = req.url ?? "/";
        const pathname = new URL(rawUrl, "http://mock-taskd.invalid").pathname;
        requests.push({ method, url: rawUrl, headers: headerRecord(req), body });
        const handler = routes.get(routeKey(method, pathname));
        if (!handler) {
          sendProblem(res, { status: 404, code: "not_found", detail: `no route for ${method} ${pathname}` });
          return;
        }
        handler(req, res, body);
      })
      .catch(() => {
        // クライアントが送信を中断した等。応答を試みない。
        if (!res.headersSent) res.destroy();
      });
  });

  on("GET", "/api/v1/health", (_req, res) => {
    sendJson(res, 200, options.health ?? defaultHealth);
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => resolve());
  });

  const address = server.address() as AddressInfo;
  const baseUrl = `http://127.0.0.1:${address.port}`;

  const close = (): Promise<void> =>
    new Promise((resolve, reject) => {
      server.closeAllConnections();
      server.close((err) => (err ? reject(err) : resolve()));
    });

  return { baseUrl, requests, on, close };
}
