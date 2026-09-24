import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { CelerisError } from "~/celeris/errors";
import type {
  ArtifactList,
  CommentList,
  EventsPage,
  TaskDetail,
  TaskRoutingView,
  Timeline,
  TreeView,
} from "~/celeris/types";
import { loadTaskDetail } from "~/routes/tasks.$id";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

const taskDetail: TaskDetail = {
  task: {
    id: "T1",
    kind: "execute",
    status: "running",
    title: "do the thing",
    objective: "do it",
    priority: 5,
    attempts: 1,
    created_at: "2026-09-15T00:00:00Z",
    updated_at: "2026-09-15T00:00:01Z",
    acceptance: [],
    depends_on: [],
    inputs: [],
    worker_hint: { tier: "standard" },
    budget: { max_retries: 3, max_turns: 10, max_wall_secs: 600 },
    workspace: { kind: "local", path: "." },
  },
  // ADR-0044 D3（Phase 53）: `priority`（5）を P0〜P3 に丸めたもの。
  priority_label: "P3",
  workspace_dir: "/tmp/ws/T1",
  timers: {
    now: "2026-09-15T00:00:02Z",
    consecutive_requeues: 0,
    consecutive_reviewer_requeues: 0,
    max_requeues: 3,
  },
  criteria: [],
  runs: [],
  prior_review: [],
  answers: [],
  latest_question: null,
  approvals: [],
  dependencies: [],
  dependents: [],
  children: [],
  actions: ["cancel", "edit"],
  worker_run_hint: null,
  delegated: [],
};

const eventsPage: EventsPage = {
  has_more: false,
  items: [
    {
      id: 1,
      seq: 0,
      task_id: "T1",
      ts: "2026-09-15T00:00:00Z",
      event: { type: "created", task: taskDetail.task },
    },
  ],
};

const artifactList: ArtifactList = { items: [] };

/** ADR-0044 D5: 時刻の昇順で 1 本（できごと・コメント・委譲・リリース…）。 */
const timeline: Timeline = {
  task_id: "T1",
  items: [
    { kind: "event", at: "2026-09-15T00:00:00Z", seq: 0, event: { type: "created", task: taskDetail.task } },
    {
      kind: "comment",
      at: "2026-09-15T00:00:03Z",
      comment: {
        id: "01CMT0000000000000000001",
        task_id: "T1",
        author_kind: "human",
        body: "先に関連研究を読んでください",
        created_at: "2026-09-15T00:00:03Z",
      },
    },
  ],
};

const comments: CommentList = { items: [timeline.items[1].kind === "comment" ? timeline.items[1].comment : never()] };

function never(): never {
  throw new Error("fixture broken");
}

/** ADR-0043 D6: `GET /tasks/{id}/tree`（「ファイル」タブのときだけ引く）。 */
const treeView: TreeView = {
  repo: "code",
  path: "",
  repos: [{ name: "code", kind: "git", dir: "/tmp/ws/T1/repos/code" }],
  entries: [{ name: "README.md", path: "README.md", kind: "file", size: 12 }],
};

/** 5 本の読み取り（詳細・イベント・成果物・タイムライン・コメント）を登録する。 */
function serveTask(id = "T1") {
  mock.on("GET", `/api/v1/tasks/${id}`, (_req, res) => sendJson(res, 200, taskDetail));
  mock.on("GET", `/api/v1/tasks/${id}/events`, (_req, res) => sendJson(res, 200, eventsPage));
  mock.on("GET", `/api/v1/tasks/${id}/artifacts`, (_req, res) => sendJson(res, 200, artifactList));
  mock.on("GET", `/api/v1/tasks/${id}/timeline`, (_req, res) => sendJson(res, 200, timeline));
  mock.on("GET", `/api/v1/tasks/${id}/comments`, (_req, res) => sendJson(res, 200, comments));
}

describe("loadTaskDetail", () => {
  it("詳細・イベント・成果物・タイムライン・コメントを引いて、そのまま返す（ADR-0044 D5）", async () => {
    serveTask();
    // 編集フォームの選択肢（ADR-0044 D1）。案件に属さないタスクなので `GET /org` だけ引く。
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));

    const result = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1"));

    // 案件・担当・途中目標（監査 M2）。この fixture のタスクは案件にも担当にも属さないので全部 null。
    expect(result).toEqual({
      detail: taskDetail,
      events: eventsPage,
      artifacts: artifactList,
      timeline,
      comments,
      // フェーズ 74（ADR-0055 D2 ラウンド 6）: タイムラインの相対時刻表示の基準時刻（loader が読み込んだ時刻）。
      fetchedAt: expect.any(String),
      org: [],
      milestones: [],
      // ADR-0046 D3（Phase 59）: `GET /config` を登録していないので落ちて `[]`（自由記述の欄になる）。
      genres: [],
      // ADR-0046 D5（Phase 59）: `assigned` イベントが無いので null。
      assignedEvent: null,
      // celeris ADR-0069 D5: `GET /tasks/{id}/routing` を登録していないので落ちて null（パネルを出さない）。
      routing: null,
      // ADR-0043 D6（Phase 52 + 53 のマージ）: 作業ツリーは `?tab=files` のときだけ引く。
      files: null,
      // マージ（Phase 54）: 「変更」タブを見ていないので引かない（`?tab=changes` のときだけ）。
      changes: null,
      // running のタスクは人のレビュー待ちではないので `GET /inbox` を引かない。
      humanReview: [],
      place: {
        projectId: null,
        projectTitle: null,
        milestoneTitle: null,
        assigneeId: null,
        assigneeName: null,
      },
    });
    expect(mock.requests.some((r) => r.url.startsWith("/api/v1/inbox"))).toBe(false);
    for (const path of ["", "/events", "/artifacts", "/timeline", "/comments"]) {
      expect(
        mock.requests.some((r) => r.method === "GET" && r.url.startsWith(`/api/v1/tasks/T1${path}`)),
        `GET /tasks/T1${path}`,
      ).toBe(true);
    }
    // 「ファイル」タブを見ていないので `GET /tasks/{id}/tree` は叩かない。
    expect(mock.requests.some((r) => r.url.startsWith("/api/v1/tasks/T1/tree"))).toBe(false);
  });

  it("GET /tasks/{id}/routing の監査をそのまま routing に載せる（celeris ADR-0069 D5）", async () => {
    serveTask();
    const routing: TaskRoutingView = {
      task_id: "T1",
      routing: { tier_source: "hint", dropped_assignee: "research" },
      runs: [{ task_id: "T1", run_id: "R1", org_node: "coding", harness: "coding", lane: "standard", model: "m" }],
    };
    mock.on("GET", "/api/v1/tasks/T1/routing", (_req, res) => sendJson(res, 200, routing));

    const result = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1"));
    expect(result.routing).toEqual(routing);
  });

  it("reviewing / 承認タスクは GET /inbox の未決の承認のうち自分か子のものだけ humanReview に載せる", async () => {
    const reviewing: TaskDetail = { ...taskDetail, task: { ...taskDetail.task, status: "reviewing" } };
    mock.on("GET", "/api/v1/tasks/T1", (_req, res) => sendJson(res, 200, reviewing));
    mock.on("GET", "/api/v1/tasks/T1/events", (_req, res) => sendJson(res, 200, eventsPage));
    mock.on("GET", "/api/v1/tasks/T1/artifacts", (_req, res) => sendJson(res, 200, artifactList));
    mock.on("GET", "/api/v1/tasks/T1/timeline", (_req, res) => sendJson(res, 200, timeline));
    mock.on("GET", "/api/v1/tasks/T1/comments", (_req, res) => sendJson(res, 200, comments));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));
    const item = (id: string, parent: string) => ({
      approval: { id, title: "Approval", kind: "approval", status: "ready", actions: ["approve", "reject"] },
      parent: { id: parent, title: "P", kind: "execute", status: "reviewing", actions: [] },
      criterion_text: "someone signs off",
      criterion_idx: 0,
      requested_at: "2026-09-15T00:00:00Z",
      evidence: [],
      other_verdicts: [],
      artifacts: [],
      knowledge_pages: [],
      previous_decisions: [],
    });
    mock.on("GET", "/api/v1/inbox", (_req, res) =>
      sendJson(res, 200, {
        approvals: [item("A1", "T1"), item("A2", "OTHER")],
        questions: [],
        drafts: [],
        attention: [],
        counts: { approvals: 2, attention: 0, by_status: {}, drafts: 0, questions: 0 },
      }),
    );

    const result = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1"));
    expect(result.humanReview.map((a) => a.approval.id)).toEqual(["A1"]);
  });

  it("GET /inbox が落ちても画面は出す（humanReview は空）", async () => {
    const reviewing: TaskDetail = { ...taskDetail, task: { ...taskDetail.task, status: "reviewing" } };
    mock.on("GET", "/api/v1/tasks/T1", (_req, res) => sendJson(res, 200, reviewing));
    mock.on("GET", "/api/v1/tasks/T1/events", (_req, res) => sendJson(res, 200, eventsPage));
    mock.on("GET", "/api/v1/tasks/T1/artifacts", (_req, res) => sendJson(res, 200, artifactList));
    mock.on("GET", "/api/v1/tasks/T1/timeline", (_req, res) => sendJson(res, 200, timeline));
    mock.on("GET", "/api/v1/tasks/T1/comments", (_req, res) => sendJson(res, 200, comments));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));
    mock.on("GET", "/api/v1/inbox", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));
    const result = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1"));
    expect(result.humanReview).toEqual([]);
  });

  /**
   * ADR-0046 D3 / D5（Phase 59 / G21）: `GET /config` の `genres` がハーネスの選択肢になり、
   * `assigned` イベントが「なぜこの担当か」の `assignedEvent` になる（直近の 1 件）。
   */
  it("genres は GET /config の genres[].id、assignedEvent は最新の assigned イベント", async () => {
    serveTask();
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));
    mock.on("GET", "/api/v1/config", (_req, res) =>
      sendJson(res, 200, { genres: [{ id: "coding" }, { id: "literature" }] }),
    );
    mock.on("GET", "/api/v1/tasks/T1/events", (_req, res) =>
      sendJson(res, 200, {
        has_more: false,
        items: [
          {
            id: 1,
            seq: 0,
            task_id: "T1",
            ts: "2026-09-15T00:00:00Z",
            event: { type: "created", task: taskDetail.task },
          },
          {
            id: 2,
            seq: 1,
            task_id: "T1",
            ts: "2026-09-15T00:00:01Z",
            event: { type: "assigned", node: "engineering", score: 2, reason: "harness coding / skill の重なり 2 件" },
          },
        ],
      } satisfies EventsPage),
    );

    const result = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1"));
    expect(result.genres).toEqual(["coding", "literature"]);
    expect(result.assignedEvent).toEqual({
      node: "engineering",
      score: 2,
      reason: "harness coding / skill の重なり 2 件",
    });
  });

  /**
   * ADR-0043 D6 + ADR-0044 D5（Phase 52 + 53 のマージ）: `?tab=files` のときだけ作業ツリーを引き、
   * 失敗（403 / 404）はページを落とさずタブの中の文言になる。
   */
  it("`?tab=files` のときだけ作業ツリーを引く（403 / 404 はタブの中の文言にする）", async () => {
    serveTask();
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));
    mock.on("GET", "/api/v1/tasks/T1/tree", (_req, res) => sendJson(res, 200, treeView));

    const ok = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1?tab=files"));
    expect(ok.files).toEqual({
      data: { taskId: "T1", tree: treeView, file: null, fileError: null, filePath: null },
      error: null,
    });

    // 作業ツリーが無いタスク（404 `file_not_found`）でも他のタブは出る。
    mock.on("GET", "/api/v1/tasks/T1/tree", (_req, res) =>
      sendProblem(res, { status: 404, code: "file_not_found", detail: "no worktree yet" }),
    );
    const missing = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1?tab=files"));
    expect(missing.files?.data).toBeNull();
    expect(missing.files?.error?.status).toBe(404);
    expect(missing.detail).toEqual(taskDetail);
  });

  it("`GET /org` が落ちても画面は出す（担当のプルダウンが空になるだけ。ADR-0044 D1）", async () => {
    serveTask();
    mock.on("GET", "/api/v1/org", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));

    const result = await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1"));

    expect(result.org).toEqual([]);
    expect(result.timeline.items).toHaveLength(2);
  });

  it("forwards the `types` search param to GET /tasks/{id}/events", async () => {
    serveTask();
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] }));

    await loadTaskDetail(client, "T1", new Request("http://gui.invalid/tasks/T1?types=transitioned"));

    const eventsReq = mock.requests.find((r) => r.url.startsWith("/api/v1/tasks/T1/events"));
    expect(eventsReq?.url).toBe("/api/v1/tasks/T1/events?types=transitioned");
  });

  it("throws CelerisError with status 404 and code task_not_found when the task does not exist", async () => {
    for (const path of ["", "/events", "/artifacts", "/timeline", "/comments"]) {
      mock.on("GET", `/api/v1/tasks/MISSING${path}`, (_req, res) => {
        sendProblem(res, { status: 404, code: "task_not_found", detail: "task MISSING not found" });
      });
    }

    let error: unknown;
    try {
      await loadTaskDetail(client, "MISSING", new Request("http://gui.invalid/tasks/MISSING"));
    } catch (e) {
      error = e;
    }

    expect(error).toBeInstanceOf(CelerisError);
    const celerisError = error as CelerisError;
    expect(celerisError.status).toBe(404);
    expect(celerisError.code).toBe("task_not_found");
  });
});
