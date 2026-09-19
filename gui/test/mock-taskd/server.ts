import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import type {
  CommentResult,
  EditResult,
  MilestoneLifecycle,
  Problem,
  Project,
  ProjectLifecycle,
  ProjectList,
  TaskComment,
  TaskList,
  TaskSummary,
  Timeline,
} from "~/taskd/types";
import {
  commentResult,
  defaultHealth,
  editResult,
  milestone,
  milestoneLifecycle,
  project,
  projectLifecycle,
  taskComment,
  taskRef,
  taskSummary,
  timeline,
} from "./fixtures";

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

/**
 * ADR-0044（Phase 53）のタスク管理の経路をまとめて登録する:
 * `GET /tasks`（新しいフィルタつき）・`GET/POST /tasks/{id}/comments`・`GET /tasks/{id}/timeline`・
 * `PATCH /tasks/{id}`・`POST /tasks/{id}/reopen`。
 *
 * **絞り込みは taskd の仕事**なので、ここでは「受け取ったクエリをそのまま `mock.requests` に残す」だけで
 * 実際のフィルタはしない（GUI 側がクエリをどう組み立てたかを検証するのが目的）。
 */
export interface TaskManagementOptions {
  taskId?: string;
  items?: TaskSummary[];
  timeline?: Timeline;
  comments?: TaskComment[];
  /** `POST /tasks/{id}/comments` の応答（ADR-0044 D2 の `effect` を差し替えるため）。 */
  comment?: CommentResult;
  /** `PATCH /tasks/{id}` の応答。 */
  edit?: EditResult;
}

export function serveTaskManagement(mock: MockTaskd, options: TaskManagementOptions = {}): void {
  const id = options.taskId ?? "01BOARDTASK00000000000001";
  const items = options.items ?? [taskSummary()];
  mock.on("GET", "/api/v1/tasks", (_req, res) => {
    const list: TaskList = { items, total: items.length, counts_by_status: {}, next_cursor: null };
    sendJson(res, 200, list);
  });
  mock.on("GET", `/api/v1/tasks/${id}/timeline`, (_req, res) => {
    sendJson(res, 200, options.timeline ?? timeline([], id));
  });
  mock.on("GET", `/api/v1/tasks/${id}/comments`, (_req, res) => {
    sendJson(res, 200, { items: options.comments ?? [taskComment({ task_id: id })] });
  });
  mock.on("POST", `/api/v1/tasks/${id}/comments`, (_req, res) => {
    sendJson(res, 201, options.comment ?? commentResult());
  });
  mock.on("PATCH", `/api/v1/tasks/${id}`, (_req, res) => {
    sendJson(res, 200, options.edit ?? editResult());
  });
  mock.on("POST", `/api/v1/tasks/${id}/reopen`, (_req, res) => {
    sendJson(res, 200, { id, from: "failed", to: "ready", reason: "reopened" });
  });
}

/**
 * 中止・一時停止・アーカイブ（ADR-0044 D6、docs/taskd-api-v1.md §3.84〜3.91。Phase 55 / G19）の 8 経路を
 * まとめて登録する。**どれも 200**（`archive` / `unarchive` は冪等なので二度押しでも 200）。
 *
 * 状態遷移は taskd の仕事なので、ここでは**操作ごとに決め打ちの応答**を返すだけ
 * （`cancel` だけ `cancelled_*` に中身を入れる）。GUI がどの経路にどの本文を送ったかは
 * `mock.requests` で検証する。個別の応答を差し替えたいときは `projects` / `milestones` に渡す。
 */
export interface LifecycleOptions {
  projectId?: string;
  milestoneId?: string;
  /** 操作名 → `ProjectLifecycle`。省略した操作は既定（`fixtures.ts` の `projectLifecycle`）。 */
  projects?: Partial<Record<"cancel" | "pause" | "resume" | "archive" | "unarchive", ProjectLifecycle>>;
  /** 操作名 → `MilestoneLifecycle`。 */
  milestones?: Partial<Record<"cancel" | "pause" | "resume", MilestoneLifecycle>>;
}

export function serveLifecycle(mock: MockTaskd, options: LifecycleOptions = {}): void {
  const projectId = options.projectId ?? "p1";
  const milestoneId = options.milestoneId ?? "m1";
  const projectDefaults: Record<string, ProjectLifecycle> = {
    cancel: projectLifecycle({
      project: project({ status: "cancelled" }),
      cancelled_tasks: [taskRef()],
      cancelled_milestones: [milestoneId],
    }),
    pause: projectLifecycle({ project: project({ status: "paused", paused_from: "active" }) }),
    resume: projectLifecycle({ project: project({ status: "active" }) }),
    archive: projectLifecycle({ project: project({ status: "done", archived_at: "2026-09-19T12:00:00Z" }) }),
    unarchive: projectLifecycle({ project: project({ status: "done" }) }),
  };
  const milestoneDefaults: Record<string, MilestoneLifecycle> = {
    cancel: milestoneLifecycle({ milestone: milestone({ status: "cancelled" }), cancelled_tasks: [taskRef()] }),
    pause: milestoneLifecycle({ milestone: milestone({ status: "paused", paused_from: "in_progress" }) }),
    resume: milestoneLifecycle({ milestone: milestone({ status: "in_progress" }) }),
  };
  for (const op of ["cancel", "pause", "resume", "archive", "unarchive"] as const) {
    mock.on("POST", `/api/v1/projects/${projectId}/${op}`, (_req, res) => {
      sendJson(res, 200, options.projects?.[op] ?? projectDefaults[op]);
    });
  }
  for (const op of ["cancel", "pause", "resume"] as const) {
    mock.on("POST", `/api/v1/milestones/${milestoneId}/${op}`, (_req, res) => {
      sendJson(res, 200, options.milestones?.[op] ?? milestoneDefaults[op]);
    });
  }
}

/**
 * `GET /projects` の `?archived=1`（ADR-0044 D6）。**隠す・出すは taskd の仕事**なので、
 * ここでは「クエリに `archived=1` が付いていたらアーカイブ済みも返す」という最小限の振る舞いだけ真似て、
 * GUI がクエリを付けたかどうかを `mock.requests` で検証できるようにする。
 */
export interface ProjectListOptions {
  /** アーカイブされていない案件（既定でも返る）。 */
  items?: Project[];
  /** アーカイブ済みの案件（`?archived=1` のときだけ返る）。 */
  archived?: Project[];
}

export function serveProjectList(mock: MockTaskd, options: ProjectListOptions = {}): void {
  const items = options.items ?? [project()];
  const archived = options.archived ?? [];
  mock.on("GET", "/api/v1/projects", (req, res) => {
    const url = new URL(req.url ?? "/", "http://mock-taskd.invalid");
    const showArchived = ["1", "true"].includes(url.searchParams.get("archived") ?? "");
    sendJson(res, 200, { items: showArchived ? [...items, ...archived] : items } satisfies ProjectList);
  });
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
