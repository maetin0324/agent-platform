import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadTaskDetail } from "~/routes/tasks.$id";
import { TaskdClient } from "~/taskd/client.server";
import { TaskdError } from "~/taskd/errors";
import type { ArtifactList, CommentList, EventsPage, TaskDetail, Timeline, TreeView } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
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
      org: [],
      milestones: [],
      // ADR-0043 D6（Phase 52 + 53 のマージ）: 作業ツリーは `?tab=files` のときだけ引く。
      files: null,
      // マージ（Phase 54）: 「変更」タブを見ていないので引かない（`?tab=changes` のときだけ）。
      changes: null,
      place: {
        projectId: null,
        projectTitle: null,
        milestoneTitle: null,
        assigneeId: null,
        assigneeName: null,
      },
    });
    for (const path of ["", "/events", "/artifacts", "/timeline", "/comments"]) {
      expect(
        mock.requests.some((r) => r.method === "GET" && r.url.startsWith(`/api/v1/tasks/T1${path}`)),
        `GET /tasks/T1${path}`,
      ).toBe(true);
    }
    // 「ファイル」タブを見ていないので `GET /tasks/{id}/tree` は叩かない。
    expect(mock.requests.some((r) => r.url.startsWith("/api/v1/tasks/T1/tree"))).toBe(false);
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

  it("throws TaskdError with status 404 and code task_not_found when the task does not exist", async () => {
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

    expect(error).toBeInstanceOf(TaskdError);
    const taskdError = error as TaskdError;
    expect(taskdError.status).toBe(404);
    expect(taskdError.code).toBe("task_not_found");
  });
});
