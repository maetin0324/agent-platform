import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { TaskdClient } from "~/taskd/client.server";
import { buildProjectTaskSpec, buildTaskEdit, commentOnTask, editTask, reopenTask } from "~/taskd/tasks-admin.server";
import type { CommentEffect } from "~/taskd/types";
import { commentResult, editResult, taskComment } from "../mock-taskd/fixtures";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * ADR-0044 D1 / D2（Phase 53）: `PATCH /tasks/{id}`・`POST /tasks/{id}/comments`・
 * `POST /tasks/{id}/reopen`・案件からの `POST /tasks`。
 * **GUI は検証しない**ので、ここで見るのは「フォーム → 本文の写し」と「taskd のエラーをそのまま返すこと」。
 */

let mock: MockTaskd;
let client: TaskdClient;
const ID = "01BOARDTASK00000000000001";

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

function form(entries: [string, string][]): FormData {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
}

describe("buildTaskEdit（ADR-0044 D1）", () => {
  it("フォームに現れた項目だけを本文に入れる（省略 = 変えない）", () => {
    expect(buildTaskEdit(form([["priority", "P0"]]))).toEqual({ priority: "P0" });
  });

  it("担当・途中目標・役割・アダプタは空文字を null（= 外す）として送る", () => {
    expect(
      buildTaskEdit(
        form([
          ["assignee", ""],
          ["milestone_id", ""],
          ["role", ""],
          ["adapter", ""],
        ]),
      ),
    ).toEqual({ assignee: null, milestone_id: null, role: null, adapter: null });
  });

  it("ラベル・依存は差し替え。空の番兵だけなら空配列（全部外す）になる", () => {
    expect(buildTaskEdit(form([["labels", ""]]))).toEqual({ labels: [] });
    expect(
      buildTaskEdit(
        form([
          ["labels", ""],
          ["labels", "pluvio"],
          ["labels", " survey "],
          ["depends_on", ""],
          ["depends_on", "01AAA"],
        ]),
      ),
    ).toEqual({ labels: ["pluvio", "survey"], depends_on: ["01AAA"] });
  });

  it("数値の欄は数として送り、空欄・非数値は送らない", () => {
    expect(
      buildTaskEdit(
        form([
          ["max_turns", "20"],
          ["max_wall_secs", ""],
          ["max_retries", "abc"],
        ]),
      ),
    ).toEqual({ max_turns: 20 });
  });

  it("`expected_status` は楽観的排他として常に添える（空なら付けない）", () => {
    expect(buildTaskEdit(form([["expected_status", "running"]]))).toEqual({ expected_status: "running" });
    expect(buildTaskEdit(form([["expected_status", ""]]))).toEqual({});
  });

  it("編集フォームの全項目は 1 度の PATCH にまとまる", () => {
    const edit = buildTaskEdit(
      form([
        ["intent", "edit"],
        ["expected_status", "ready"],
        ["title", "関連研究を調べる"],
        ["objective", "3 本読む"],
        ["tier", "frontier"],
        ["priority", "P1"],
        ["category", "research"],
        ["assignee", "research-survey"],
        ["milestone_id", "01MILESTONE0000000000001"],
        ["labels", ""],
        ["labels", "pluvio"],
        ["depends_on", ""],
      ]),
    );
    expect(edit).toEqual({
      expected_status: "ready",
      title: "関連研究を調べる",
      objective: "3 本読む",
      tier: "frontier",
      priority: "P1",
      category: "research",
      assignee: "research-survey",
      milestone_id: "01MILESTONE0000000000001",
      labels: ["pluvio"],
      depends_on: [],
    });
  });
});

describe("editTask（PATCH /tasks/{id}）", () => {
  it("本文をそのまま送り、taskd の `fields`（実際に変わった項目）を返す", async () => {
    mock.on("PATCH", `/api/v1/tasks/${ID}`, (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ priority: "P0", expected_status: "ready" });
      sendJson(res, 200, editResult(["priority"], { priority: 30 }));
    });

    const outcome = await editTask(client, ID, { priority: "P0", expected_status: "ready" });

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) throw new Error("expected success");
    expect(outcome.op).toBe("edit");
    expect(outcome.result.fields).toEqual(["priority"]);
    expect(outcome.result.task.priority).toBe(30);
  });

  it("終端のタスクの 409 は例外にせず conflict の ActionError にする", async () => {
    mock.on("PATCH", `/api/v1/tasks/${ID}`, (_req, res) => {
      sendProblem(res, {
        status: 409,
        code: "invalid_state",
        detail: "cannot edit a done task",
        extra: { task_status: "done" },
      });
    });

    const outcome = await editTask(client, ID, { title: "新しい題名" });

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.status).toBe(409);
    expect(outcome.error.conflict).toBe(true);
    expect(outcome.error.detail).toBe("cannot edit a done task");
  });

  it("422 の `errors[]` は欄ごとの文言として返す（ラベルの形は taskd が決める）", async () => {
    mock.on("PATCH", `/api/v1/tasks/${ID}`, (_req, res) => {
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "validation failed",
        extra: { errors: [{ field: "labels", message: "label must match [a-z0-9-]" }] },
      });
    });

    const outcome = await editTask(client, ID, { labels: ["Pluvio"] });

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.fields.labels).toEqual(["label must match [a-z0-9-]"]);
  });
});

describe("commentOnTask（POST /tasks/{id}/comments。ADR-0044 D2 の表）", () => {
  const cases: { effect: CommentEffect; transition: boolean }[] = [
    { effect: "stored", transition: false },
    { effect: "interrupted", transition: true },
    { effect: "answered", transition: true },
    { effect: "terminal", transition: false },
  ];

  for (const { effect, transition } of cases) {
    it(`effect = ${effect} をそのまま返す`, async () => {
      const result = commentResult({
        effect,
        can_reopen: effect === "terminal",
        transition: transition ? { id: ID, from: "running", to: "ready", reason: "comment" } : null,
      });
      mock.on("POST", `/api/v1/tasks/${ID}/comments`, (_req, res, body) => {
        expect(JSON.parse(body)).toEqual({ body: "止めて" });
        sendJson(res, 201, result);
      });

      const outcome = await commentOnTask(client, ID, form([["body", "止めて"]]));

      expect(outcome.ok).toBe(true);
      if (!outcome.ok) throw new Error("expected success");
      expect(outcome.result.effect).toBe(effect);
      expect(outcome.result.transition ?? null).toEqual(result.transition ?? null);
      expect(outcome.result.can_reopen).toBe(effect === "terminal");
    });
  }

  it("空のコメントは taskd が 422 を返し、その文言をそのまま返す（GUI は弾かない）", async () => {
    mock.on("POST", `/api/v1/tasks/${ID}/comments`, (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ body: "" });
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "validation failed",
        extra: { errors: [{ field: "body", message: "body must not be blank" }] },
      });
    });

    const outcome = await commentOnTask(client, ID, form([["body", ""]]));

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.fields.body).toEqual(["body must not be blank"]);
  });

  it("担当が書いたコメント（author_kind = node）もそのまま読める", async () => {
    mock.on("POST", `/api/v1/tasks/${ID}/comments`, (_req, res) => {
      sendJson(
        res,
        201,
        commentResult({ comment: taskComment({ author_kind: "node", author: "research-survey", run_id: "r1" }) }),
      );
    });

    const outcome = await commentOnTask(client, ID, form([["body", "進捗"]]));

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) throw new Error("expected success");
    expect(outcome.result.comment.author_kind).toBe("node");
    expect(outcome.result.comment.author).toBe("research-survey");
  });
});

describe("reopenTask（POST /tasks/{id}/reopen。ADR-0044 D2）", () => {
  it("`expected_status` を添えて送り、done/failed → ready の遷移を返す", async () => {
    mock.on("POST", `/api/v1/tasks/${ID}/reopen`, (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ expected_status: "failed" });
      sendJson(res, 200, { id: ID, from: "failed", to: "ready", reason: "reopened" });
    });

    const outcome = await reopenTask(client, ID, form([["expected_status", "failed"]]));

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) throw new Error("expected success");
    expect(outcome.result.from).toBe("failed");
    expect(outcome.result.to).toBe("ready");
  });

  it("`expected_status` が無ければ空の本文を送る", async () => {
    mock.on("POST", `/api/v1/tasks/${ID}/reopen`, (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({});
      sendJson(res, 200, { id: ID, from: "done", to: "ready", reason: "reopened" });
    });

    const outcome = await reopenTask(client, ID, form([]));

    expect(outcome.ok).toBe(true);
  });

  it("cancelled は 409（worktree が無いので再開できない）。文言をそのまま返す", async () => {
    mock.on("POST", `/api/v1/tasks/${ID}/reopen`, (_req, res) => {
      sendProblem(res, { status: 409, code: "invalid_transition", detail: "cancelled tasks cannot be reopened" });
    });

    const outcome = await reopenTask(client, ID, form([["expected_status", "cancelled"]]));

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.status).toBe(409);
    expect(outcome.error.conflict).toBe(true);
  });
});

describe("buildProjectTaskSpec（案件・途中目標の「タスクを追加」。ADR-0044 D1）", () => {
  it("案件 id を必ず入れ、`status` は送らない（`POST /tasks` の既定が ready）", () => {
    const spec = buildProjectTaskSpec(
      form([
        ["title", "関連研究を調べる"],
        ["objective", "3 本読む"],
        ["acceptance", "候補が 3 件以上まとまっている"],
      ]),
      "01PROJECT000000000000001",
    );
    expect(spec).toEqual({
      title: "関連研究を調べる",
      objective: "3 本読む",
      acceptance: [{ type: "human", text: "候補が 3 件以上まとまっている" }],
      project_id: "01PROJECT000000000000001",
    });
    expect("status" in spec).toBe(false);
  });

  it("途中目標カードからは milestone_id も入る。空欄は送らない（taskd の既定に任せる）", () => {
    const spec = buildProjectTaskSpec(
      form([
        ["title", "t"],
        ["objective", "o"],
        ["acceptance", "a"],
        ["milestone_id", "01MILESTONE0000000000001"],
        ["assignee", ""],
        ["tier", "frontier"],
        ["priority", "P1"],
        ["category", "research"],
      ]),
      "01PROJECT000000000000001",
    );
    expect(spec.milestone_id).toBe("01MILESTONE0000000000001");
    expect(spec.tier).toBe("frontier");
    expect(spec.priority).toBe("P1");
    expect(spec.category).toBe("research");
    expect("assignee" in spec).toBe(false);
  });

  it("受け入れ条件が空なら空配列で送る（taskd が 422 を返す。GUI では弾かない）", () => {
    const spec = buildProjectTaskSpec(
      form([
        ["title", ""],
        ["objective", ""],
        ["acceptance", ""],
      ]),
      "01PROJECT000000000000001",
    );
    expect(spec.acceptance).toEqual([]);
  });
});
