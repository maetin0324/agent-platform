import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import {
  archiveProject,
  cancelMilestone,
  cancelProject,
  pauseMilestone,
  pauseProject,
  resumeMilestone,
  resumeProject,
  unarchiveProject,
} from "~/celeris/projects-admin.server";
import type { MilestoneStatus, ProjectStatus } from "~/celeris/types";
import {
  archivedQuery,
  milestoneIsPaused,
  milestoneIsTerminal,
  milestoneLifecycleButtons,
  projectIsArchived,
  projectIsPaused,
  projectIsTerminal,
  projectLifecycleButtons,
  readArchivedParam,
} from "~/lib/lifecycle";
import { type MockCeleris, sendJson, sendProblem, serveLifecycle, startMockCeleris } from "../mock-celeris/server";

/**
 * 中止・一時停止・アーカイブ（ADR-0044 D6、docs/celeris-api-v1.md §3.84〜3.91。Phase 55 / G19）。
 *
 * ここで確かめるのは 2 つだけ:
 * 1. **どのボタンをどの状態で出すか**（`~/lib/lifecycle.ts` の純関数）。押せるかどうかの最終判断は
 *    celeris（409 `invalid_transition`）なので、ここは「意味の無いボタンを出さない」ことの確認。
 * 2. **BFF がどの経路に何を送るか**（`~/celeris/projects-admin.server.ts`）。本文は `{}` で、
 *    celeris の応答（`ProjectLifecycle` / `MilestoneLifecycle`）と 404 / 409 / 401 をそのまま通す。
 */

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("projectLifecycleButtons（どのボタンをどの状態で出すか）", () => {
  it("進行中: 一時停止・中止は出る。再開は出ない。アーカイブは出るが押せない（終端ではない）", () => {
    expect(projectLifecycleButtons({ status: "active" })).toEqual({
      pause: true,
      resume: false,
      cancel: true,
      archive: true,
      unarchive: false,
      archiveEnabled: false,
    });
  });

  it("提案中も進行中と同じ（まだ終端ではない）", () => {
    const buttons = projectLifecycleButtons({ status: "proposed" });
    expect(buttons.pause).toBe(true);
    expect(buttons.archiveEnabled).toBe(false);
  });

  it("一時停止中: 「再開」だけになり「一時停止」は消える（もう一度 pause は 409）", () => {
    const buttons = projectLifecycleButtons({ status: "paused" });
    expect(buttons.pause).toBe(false);
    expect(buttons.resume).toBe(true);
    expect(buttons.cancel).toBe(true);
    expect(buttons.archiveEnabled).toBe(false);
  });

  it("完了: 終端なので一時停止は出ず、アーカイブが押せるようになる", () => {
    expect(projectLifecycleButtons({ status: "done" })).toEqual({
      pause: false,
      resume: false,
      cancel: true,
      archive: true,
      unarchive: false,
      archiveEnabled: true,
    });
  });

  it("中止済み: 中止も一時停止も出ない。アーカイブは押せる", () => {
    const buttons = projectLifecycleButtons({ status: "cancelled" });
    expect(buttons.cancel).toBe(false);
    expect(buttons.pause).toBe(false);
    expect(buttons.archiveEnabled).toBe(true);
  });

  it("アーカイブ済み: 「アーカイブ」が消えて「アーカイブ解除」に入れ替わる", () => {
    const buttons = projectLifecycleButtons({ status: "done", archived_at: "2026-09-19T12:00:00Z" });
    expect(buttons.archive).toBe(false);
    expect(buttons.unarchive).toBe(true);
  });

  it("アーカイブできるのは終端（完了・中止）の案件だけ", () => {
    const statuses: ProjectStatus[] = ["proposed", "active", "paused", "done", "cancelled"];
    const enabled = statuses.filter((status) => projectLifecycleButtons({ status }).archiveEnabled);
    expect(enabled).toEqual(["done", "cancelled"]);
  });

  it("archived_at は null / 省略ならアーカイブされていない扱い", () => {
    expect(projectIsArchived({})).toBe(false);
    expect(projectIsArchived({ archived_at: null })).toBe(false);
    expect(projectIsArchived({ archived_at: "2026-09-19T12:00:00Z" })).toBe(true);
  });

  it("終端・一時停止の判定", () => {
    expect(projectIsTerminal("done")).toBe(true);
    expect(projectIsTerminal("cancelled")).toBe(true);
    expect(projectIsTerminal("paused")).toBe(false);
    expect(projectIsPaused({ status: "paused" })).toBe(true);
    expect(projectIsPaused({ status: "active" })).toBe(false);
  });
});

describe("milestoneLifecycleButtons（途中目標のカード）", () => {
  it("進行中: 一時停止・中止が出る", () => {
    expect(milestoneLifecycleButtons({ status: "in_progress" })).toEqual({
      pause: true,
      resume: false,
      cancel: true,
    });
  });

  it("一時停止中: 「再開」だけになる", () => {
    expect(milestoneLifecycleButtons({ status: "paused" })).toEqual({
      pause: false,
      resume: true,
      cancel: true,
    });
  });

  it("中止済み: 何も出ない（もう一度 cancel は 409）", () => {
    expect(milestoneLifecycleButtons({ status: "cancelled" })).toEqual({
      pause: false,
      resume: false,
      cancel: false,
    });
  });

  it("終端（達成・再設計・中止）は一時停止も中止もできない", () => {
    const statuses: MilestoneStatus[] = ["proposed", "approved", "in_progress", "reached", "redesigned", "cancelled"];
    const pausable = statuses.filter((status) => milestoneLifecycleButtons({ status }).pause);
    expect(pausable).toEqual(["proposed", "approved", "in_progress"]);
    // 案件と違い、達成・再設計の途中目標は中止もできない（celeris が 409。§3.84〜3.91 の「終端」の定義）。
    const cancellable = statuses.filter((status) => milestoneLifecycleButtons({ status }).cancel);
    expect(cancellable).toEqual(["proposed", "approved", "in_progress"]);
    expect(milestoneIsTerminal("reached")).toBe(true);
    expect(milestoneIsTerminal("redesigned")).toBe(true);
    expect(milestoneIsPaused({ status: "paused" })).toBe(true);
  });

  it("達成済み: ボタンは出ない（`reached` は終端）", () => {
    expect(milestoneLifecycleButtons({ status: "reached" })).toEqual({
      pause: false,
      resume: false,
      cancel: false,
    });
  });
});

describe("archived の検索パラメータ（`GET /projects?archived=1`）", () => {
  it("?archived=1 のときだけ表示する（既定は隠す = クエリを送らない）", () => {
    expect(readArchivedParam(new URLSearchParams(""))).toBe(false);
    expect(readArchivedParam(new URLSearchParams("archived=1"))).toBe(true);
    expect(readArchivedParam(new URLSearchParams("archived=0"))).toBe(false);
    expect(archivedQuery(true)).toBe("1");
    expect(archivedQuery(false)).toBeUndefined();
  });
});

describe("案件の中止・一時停止・アーカイブ（POST /projects/{id}/…）", () => {
  it("cancel: 空の本文を送り、連鎖で中止されたタスク・途中目標をそのまま返す", async () => {
    serveLifecycle(mock);
    const result = await cancelProject(client, "p1");

    expect(result.ok).toBe(true);
    if (!result.ok || result.op !== "project_cancel") throw new Error(`unexpected: ${JSON.stringify(result)}`);
    expect(result.lifecycle.project.status).toBe("cancelled");
    expect(result.lifecycle.cancelled_tasks).toHaveLength(1);
    expect(result.lifecycle.cancelled_milestones).toEqual(["m1"]);

    const req = mock.requests.find((r) => r.url === "/api/v1/projects/p1/cancel");
    expect(req?.method).toBe("POST");
    expect(req?.body).toBe("{}");
  });

  it("pause / resume: paused_from を含む案件をそのまま返す", async () => {
    serveLifecycle(mock);
    const paused = await pauseProject(client, "p1");
    expect(paused.ok).toBe(true);
    if (!paused.ok || paused.op !== "project_pause") throw new Error("unexpected");
    expect(paused.lifecycle.project.status).toBe("paused");
    expect(paused.lifecycle.project.paused_from).toBe("active");
    expect(paused.lifecycle.cancelled_tasks).toEqual([]);

    const resumed = await resumeProject(client, "p1");
    expect(resumed.ok).toBe(true);
    if (!resumed.ok || resumed.op !== "project_resume") throw new Error("unexpected");
    expect(resumed.lifecycle.project.status).toBe("active");
  });

  it("archive / unarchive: archived_at の有無をそのまま返す（どちらも 200・冪等）", async () => {
    serveLifecycle(mock);
    const archived = await archiveProject(client, "p1");
    if (!archived.ok || archived.op !== "project_archive") throw new Error("unexpected");
    expect(archived.lifecycle.project.archived_at).toBe("2026-09-19T12:00:00Z");

    const unarchived = await unarchiveProject(client, "p1");
    if (!unarchived.ok || unarchived.op !== "project_unarchive") throw new Error("unexpected");
    expect(unarchived.lifecycle.project.archived_at).toBeUndefined();

    // 二度押しも 200（冪等）。
    expect((await archiveProject(client, "p1")).ok).toBe(true);
    expect((await archiveProject(client, "p1")).ok).toBe(true);
  });

  it("409 invalid_transition（非終端の案件の archive）は例外にせず ActionError にする", async () => {
    mock.on("POST", "/api/v1/projects/p1/archive", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "invalid_transition",
        detail: "project p1 (status=active) cannot be archived",
        extra: { trigger: "project_archive" },
      }),
    );
    const result = await archiveProject(client, "p1");
    expect(result).toMatchObject({
      ok: false,
      op: "project_archive",
      error: { status: 409, code: "invalid_transition", conflict: true },
    });
  });

  it("404 project_not_found と 401 unauthorized（管理系）もそのまま返す", async () => {
    mock.on("POST", "/api/v1/projects/missing/pause", (_req, res) =>
      sendProblem(res, { status: 404, code: "project_not_found", detail: "no such project" }),
    );
    mock.on("POST", "/api/v1/projects/p1/pause", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    expect(await pauseProject(client, "missing")).toMatchObject({
      ok: false,
      op: "project_pause",
      error: { status: 404, code: "project_not_found" },
    });
    expect(await pauseProject(client, "p1")).toMatchObject({
      ok: false,
      op: "project_pause",
      error: { status: 401, code: "unauthorized" },
    });
  });

  it("id は URL エンコードして送る", async () => {
    mock.on("POST", "/api/v1/projects/a%2Fb/cancel", (_req, res) => sendJson(res, 200, { project: {} }));
    await cancelProject(client, "a/b");
    expect(mock.requests.some((r) => r.url === "/api/v1/projects/a%2Fb/cancel")).toBe(true);
  });
});

describe("途中目標の中止・一時停止（POST /milestones/{id}/…）", () => {
  it("cancel: 連鎖で中止されたタスクをそのまま返す（案件の状態は変えない）", async () => {
    serveLifecycle(mock);
    const result = await cancelMilestone(client, "m1");
    if (!result.ok || result.op !== "milestone_cancel") throw new Error(`unexpected: ${JSON.stringify(result)}`);
    expect(result.lifecycle.milestone.status).toBe("cancelled");
    expect(result.lifecycle.cancelled_tasks).toHaveLength(1);
    expect(mock.requests.find((r) => r.url === "/api/v1/milestones/m1/cancel")?.body).toBe("{}");
  });

  it("pause / resume: paused_from をそのまま返す", async () => {
    serveLifecycle(mock);
    const paused = await pauseMilestone(client, "m1");
    if (!paused.ok || paused.op !== "milestone_pause") throw new Error("unexpected");
    expect(paused.lifecycle.milestone.status).toBe("paused");
    expect(paused.lifecycle.milestone.paused_from).toBe("in_progress");

    const resumed = await resumeMilestone(client, "m1");
    if (!resumed.ok || resumed.op !== "milestone_resume") throw new Error("unexpected");
    expect(resumed.lifecycle.milestone.status).toBe("in_progress");
  });

  it("404 milestone_not_found / 409 invalid_transition をそのまま返す", async () => {
    mock.on("POST", "/api/v1/milestones/missing/resume", (_req, res) =>
      sendProblem(res, { status: 404, code: "milestone_not_found", detail: "no such milestone" }),
    );
    mock.on("POST", "/api/v1/milestones/m1/resume", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "invalid_transition",
        detail: "milestone m1 (status=in_progress) cannot be resumed",
      }),
    );
    expect(await resumeMilestone(client, "missing")).toMatchObject({
      ok: false,
      op: "milestone_resume",
      error: { status: 404, code: "milestone_not_found" },
    });
    expect(await resumeMilestone(client, "m1")).toMatchObject({
      ok: false,
      op: "milestone_resume",
      error: { status: 409, code: "invalid_transition" },
    });
  });
});
