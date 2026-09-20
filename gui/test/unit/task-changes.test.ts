import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import {
  integrateChange,
  loadTaskChanges,
  mergePullRequest,
  readIntegrateBody,
  readTaskChangesQuery,
} from "~/celeris/task-changes";
import type { ProjectDetail } from "~/celeris/types";
import {
  CHANGES_MISSING_LABEL,
  changedFileStatusLabel,
  DIFF_TRUNCATED_LABEL,
  integrateMergeLabel,
  integrationMethodLabel,
  integrationStateLabel,
  NO_CHANGES_LABEL,
  prUnavailableReason,
} from "~/lib/labels";
import {
  changedFileStatusTone,
  fileDeltaChip,
  integrationStateTone,
  parseDiff,
  shortSha,
  statChip,
  taskChangesHref,
} from "~/lib/task-changes";
import { loadProjectDetail } from "~/routes/projects.$id";
import {
  changeDiffView,
  changesView,
  defaultProjectIntegrations,
  integrateResult,
  repoChangesView,
  taskIntegration,
} from "../mock-celeris/fixtures";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

/**
 * 変更の取り込み（ADR-0043 D5、celeris Phase 54 / G18）。DOM を描画する unit テストが無い（G10-U1）ので、
 * 表示の判断は `~/lib/task-changes.ts` / `~/lib/labels.ts` の純粋関数、取得と送信は
 * `~/celeris/task-changes.ts` で見る（`test/unit/task-files.test.ts` と同じ作り）。
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

describe("parseDiff（色を付けるためだけの分類）", () => {
  it("差分が無ければ空", () => {
    expect(parseDiff("")).toEqual([]);
  });

  it("見出し・hunk・追加・削除・文脈を分ける（`+++` / `---` はファイル名なので meta）", () => {
    const lines = parseDiff(changeDiffView().diff);
    expect(lines.map((l) => l.kind)).toEqual(["meta", "meta", "meta", "meta", "hunk", "ctx", "del", "add", "ctx"]);
    expect(lines[6].text).toBe("-    old();");
    expect(lines[7].text).toBe("+    new();");
  });

  it("末尾の改行で余分な空行を作らない", () => {
    expect(parseDiff("a\n")).toEqual([{ kind: "ctx", text: "a" }]);
    expect(parseDiff("a\n\n")).toEqual([
      { kind: "ctx", text: "a" },
      { kind: "ctx", text: "" },
    ]);
  });

  it("`\\ No newline at end of file` と binary の断りは meta", () => {
    expect(parseDiff("\\ No newline at end of file")[0].kind).toBe("meta");
    expect(parseDiff("Binary files a/x.png and b/x.png differ")[0].kind).toBe("meta");
  });
});

describe("短い sha・stat・増減", () => {
  it("sha は 12 桁まで（空は `-`）", () => {
    expect(shortSha("9602b596826c9f0f3b1c")).toBe("9602b596826c");
    expect(shortSha("abc")).toBe("abc");
    expect(shortSha("")).toBe("-");
    expect(shortSha(null)).toBe("-");
  });

  it("stat は ファイル数 +追加 −削除", () => {
    expect(statChip({ files: 3, additions: 42, deletions: 12 })).toBe("3 ファイル +42 −12");
    expect(statChip({ files: 0, additions: 0, deletions: 0 })).toBe("0 ファイル +0 −0");
  });

  it("バイナリは行数を出さない（git が出さないため）", () => {
    expect(fileDeltaChip({ path: "src/lib.rs", status: "M", additions: 12, deletions: 4 })).toBe("+12 −4");
    expect(fileDeltaChip({ path: "a.png", status: "A", additions: 0, deletions: 0, binary: true })).toBe("バイナリ");
  });
});

describe("言葉（知らない値は素のまま）", () => {
  it("status の文字", () => {
    expect(changedFileStatusLabel("A")).toBe("追加");
    expect(changedFileStatusLabel("M")).toBe("変更");
    expect(changedFileStatusLabel("D")).toBe("削除");
    expect(changedFileStatusLabel("?")).toBe("未追跡");
    expect(changedFileStatusLabel("T")).toBe("種類が変わった");
    expect(changedFileStatusLabel("R")).toBe("R");
  });

  it("取り込みの方法と行方", () => {
    expect(integrationMethodLabel("merge")).toBe("取り込み");
    expect(integrationMethodLabel("pr")).toBe("PR");
    expect(integrationMethodLabel("discard")).toBe("破棄");
    expect(integrationMethodLabel("rebase")).toBe("rebase");
    expect(integrationStateLabel("done")).toBe("取り込み済み");
    expect(integrationStateLabel("open")).toBe("PR 公開中");
    expect(integrationStateLabel("merged")).toBe("merge 済み");
    expect(integrationStateLabel("closed")).toBe("閉じた");
    expect(integrationStateLabel("conflict")).toBe("衝突");
    expect(integrationStateLabel("failed")).toBe("失敗");
    expect(integrationStateLabel("queued")).toBe("queued");
  });

  it("「取り込む」ボタンは celeris が返した default_branch を使う（main とは限らない）", () => {
    expect(integrateMergeLabel("main")).toBe("main に取り込む");
    expect(integrateMergeLabel("develop")).toBe("develop に取り込む");
  });

  it("PR を作れない理由（`origin` / `gh` は celeris が判定した値）", () => {
    expect(prUnavailableReason(true, true)).toBeNull();
    expect(prUnavailableReason(false, true)).toBe("origin リモートが無いので PR を作れません");
    expect(prUnavailableReason(true, false)).toBe("gh が使えない（PATH に無い・未認証）ので PR を作れません");
    expect(prUnavailableReason(false, false)).toBe("origin リモートが無く、gh も使えないので PR を作れません");
  });

  it("`missing` / 変更なし / 途中で切った の文言", () => {
    expect(CHANGES_MISSING_LABEL).toBe("取り込み済み・中止済み（作業ツリーもブランチもありません）");
    expect(NO_CHANGES_LABEL).toBe("変更なし");
    expect(DIFF_TRUNCATED_LABEL).toBe("途中で切りました（200 KiB）");
  });

  it("色は役割だけ（知らない値は中立）", () => {
    expect(changedFileStatusTone("A")).toBe("success");
    expect(changedFileStatusTone("D")).toBe("danger");
    expect(changedFileStatusTone("R")).toBe("neutral");
    expect(integrationStateTone("conflict")).toBe("warning");
    expect(integrationStateTone("failed")).toBe("danger");
    expect(integrationStateTone("queued")).toBe("neutral");
  });
});

describe("taskChangesHref / readTaskChangesQuery", () => {
  it("空の値はクエリに出さない", () => {
    expect(taskChangesHref("t1")).toBe("/tasks/t1/changes");
    expect(taskChangesHref("t1", { repo: "benchfs" })).toBe("/tasks/t1/changes?repo=benchfs");
    expect(taskChangesHref("t1", { repo: "benchfs", file: "src/lib.rs" })).toBe(
      "/tasks/t1/changes?repo=benchfs&file=src%2Flib.rs",
    );
  });

  it("?repo=&file= をそのまま読む（無ければ null）", () => {
    expect(readTaskChangesQuery(new Request("http://gui.invalid/tasks/t1/changes"))).toEqual({
      repo: null,
      file: null,
    });
    expect(
      readTaskChangesQuery(new Request("http://gui.invalid/tasks/t1/changes?repo=benchfs&file=src%2Flib.rs")),
    ).toEqual({ repo: "benchfs", file: "src/lib.rs" });
  });
});

describe("loadTaskChanges (GET /tasks/{id}/changes)", () => {
  it("一覧: celeris の並びと値をそのまま返す（ファイルを選ばなければ差分は引かない）", async () => {
    const changes = changesView();
    mock.on("GET", "/api/v1/tasks/t1/changes", (_req, res) => sendJson(res, 200, changes));

    const result = await loadTaskChanges(client, "t1", {});

    expect(result.changes).toEqual(changes);
    expect(result.changes.repos[0].files.map((f) => f.path)).toEqual(["src/lib.rs", "src/new.rs", "docs/old.md"]);
    expect(result.diff).toBeNull();
    expect(result.diffError).toBeNull();
    expect(result.diffRepo).toBeNull();
    expect(result.diffPath).toBeNull();
    expect(mock.requests).toHaveLength(1);
  });

  it("ファイルを選ぶと GET .../changes/{repo}/diff?path= を引く", async () => {
    mock.on("GET", "/api/v1/tasks/t1/changes", (_req, res) => sendJson(res, 200, changesView()));
    mock.on("GET", "/api/v1/tasks/t1/changes/benchfs/diff", (_req, res) => sendJson(res, 200, changeDiffView()));

    const result = await loadTaskChanges(client, "t1", { repo: "benchfs", file: "src/lib.rs" });

    expect(result.diffRepo).toBe("benchfs");
    expect(result.diffPath).toBe("src/lib.rs");
    expect(result.diff?.diff).toContain("@@ -1,3 +1,3 @@");
    expect(result.diffError).toBeNull();
    const diffReq = mock.requests.find((r) => r.url.startsWith("/api/v1/tasks/t1/changes/benchfs/diff"));
    expect(new URL(diffReq?.url ?? "", "http://mock-celeris.invalid").searchParams.get("path")).toBe("src/lib.rs");
  });

  it("repo だけ・file だけのときは差分を引かない（400 `path` 必須を踏みに行かない）", async () => {
    mock.on("GET", "/api/v1/tasks/t1/changes", (_req, res) => sendJson(res, 200, changesView()));

    expect((await loadTaskChanges(client, "t1", { repo: "benchfs" })).diff).toBeNull();
    expect((await loadTaskChanges(client, "t1", { file: "src/lib.rs" })).diff).toBeNull();
    expect(mock.requests.every((r) => !r.url.includes("/diff"))).toBe(true);
  });

  it("`truncated` はそのまま通す（200 KiB で切られた差分）", async () => {
    mock.on("GET", "/api/v1/tasks/t1/changes", (_req, res) => sendJson(res, 200, changesView()));
    mock.on("GET", "/api/v1/tasks/t1/changes/benchfs/diff", (_req, res) =>
      sendJson(res, 200, changeDiffView({ truncated: true })),
    );

    const result = await loadTaskChanges(client, "t1", { repo: "benchfs", file: "src/lib.rs" });

    expect(result.diff?.truncated).toBe(true);
  });

  it("差分の 403 / 404 は一覧を出したまま celeris の文言を diffError に載せる", async () => {
    mock.on("GET", "/api/v1/tasks/t1/changes", (_req, res) => sendJson(res, 200, changesView()));
    mock.on("GET", "/api/v1/tasks/t1/changes/benchfs/diff", (_req, res) =>
      sendProblem(res, { status: 403, code: "path_forbidden", detail: "path escapes the repo: ../../etc/passwd" }),
    );

    const result = await loadTaskChanges(client, "t1", { repo: "benchfs", file: "../../etc/passwd" });

    expect(result.changes.repos).toHaveLength(1);
    expect(result.diff).toBeNull();
    expect(result.diffPath).toBe("../../etc/passwd");
    expect(result.diffError?.status).toBe(403);
    expect(result.diffError?.code).toBe("path_forbidden");
    expect(result.diffError?.detail).toBe("path escapes the repo: ../../etc/passwd");
  });

  it("一覧の 404（作業ツリーが無い・知らないタスク）は例外になる（画面は ErrorBoundary）", async () => {
    mock.on("GET", "/api/v1/tasks/t1/changes", (_req, res) =>
      sendProblem(res, { status: 404, code: "file_not_found", detail: "task t1 has no work tree" }),
    );

    await expect(loadTaskChanges(client, "t1", {})).rejects.toThrow("task t1 has no work tree");
  });

  it("`missing` / `ahead: 0` のリポジトリもそのまま通す（GUI では判定しない）", async () => {
    mock.on("GET", "/api/v1/tasks/t1/changes", (_req, res) =>
      sendJson(
        res,
        200,
        changesView({
          repos: [
            repoChangesView({ missing: true, ahead: 0, files: [], stat: { files: 0, additions: 0, deletions: 0 } }),
            repoChangesView({ repo: "paper", ahead: 0, files: [], stat: { files: 0, additions: 0, deletions: 0 } }),
          ],
        }),
      ),
    );

    const result = await loadTaskChanges(client, "t1", {});

    expect(result.changes.repos[0].missing).toBe(true);
    expect(result.changes.repos[1].ahead).toBe(0);
    expect(result.changes.repos[1].files).toEqual([]);
  });
});

describe("readIntegrateBody（空欄はキーごと送らない）", () => {
  const body = (entries: Record<string, string>) => {
    const form = new FormData();
    for (const [k, v] of Object.entries(entries)) form.set(k, v);
    return readIntegrateBody(form);
  };

  it("method だけ", () => {
    expect(body({ method: "merge" })).toEqual({ method: "merge" });
    expect(body({ method: "pr", note: "" })).toEqual({ method: "pr" });
  });

  it("ひとことは書いたときだけ", () => {
    expect(body({ method: "merge", note: "レビュー済み" })).toEqual({ method: "merge", note: "レビュー済み" });
  });

  it("confirm は確認欄が出ているときだけ true", () => {
    expect(body({ method: "discard", confirm: "true" })).toEqual({ method: "discard", confirm: true });
    expect(body({ method: "discard" })).toEqual({ method: "discard" });
  });
});

describe("integrateChange (POST /tasks/{id}/changes/{repo}/integrate)", () => {
  const onIntegrate = (handler: Parameters<MockCeleris["on"]>[2]) =>
    mock.on("POST", "/api/v1/tasks/t1/changes/benchfs/integrate", handler);

  it("merge: 本文をそのまま送り、200 の記録を返す", async () => {
    onIntegrate((_req, res) => sendJson(res, 200, integrateResult()));

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "merge", note: "ok" });

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) throw new Error("unreachable");
    expect(outcome.op).toBe("integrate");
    expect(outcome.repo).toBe("benchfs");
    expect(outcome.result.integration.state).toBe("done");
    expect(JSON.parse(mock.requests[0].body)).toEqual({ method: "merge", note: "ok" });
  });

  it("409 default_branch_busy の「main が編集中」はそのまま画面に出す", async () => {
    onIntegrate((_req, res) => sendProblem(res, { status: 409, code: "default_branch_busy", detail: "main が編集中" }));

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "merge" });

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.status).toBe(409);
    expect(outcome.error.code).toBe("default_branch_busy");
    expect(outcome.error.detail).toBe("main が編集中");
    // `toActionError` は 409 を一律 `conflict: true` にする（`ErrorFlash` の既存の扱い）。
    // 文言そのものは `error.detail` として画面に出る（部品側に専用の Alert もある）。
    expect(outcome.error.conflict).toBe(true);
  });

  it("409 pr_unavailable（origin / gh が無い）もそのまま", async () => {
    onIntegrate((_req, res) =>
      sendProblem(res, { status: 409, code: "pr_unavailable", detail: "no origin remote for benchfs" }),
    );

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "pr" });

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.code).toBe("pr_unavailable");
    expect(outcome.error.detail).toBe("no origin remote for benchfs");
  });

  it("discard に confirm が無いと 422 `validation`（`field: confirm`）", async () => {
    onIntegrate((_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "confirm is required for discard",
        extra: { errors: [{ field: "confirm", message: "確認が必要です" }] },
      }),
    );

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "discard" });

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.status).toBe(422);
    expect(outcome.error.fields.confirm).toEqual(["確認が必要です"]);
  });

  it("discard は confirm: true を送る", async () => {
    onIntegrate((_req, res) =>
      sendJson(res, 200, integrateResult({ integration: taskIntegration({ method: "discard", state: "done" }) })),
    );

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "discard", confirm: true });

    expect(outcome.ok).toBe(true);
    expect(JSON.parse(mock.requests[0].body)).toEqual({ method: "discard", confirm: true });
  });

  it("衝突は 200（state: conflict + child_task_id）でエラーにしない", async () => {
    onIntegrate((_req, res) =>
      sendJson(
        res,
        200,
        integrateResult({
          integration: taskIntegration({
            state: "conflict",
            detail: "衝突: src/lib.rs（解消タスク 01CHILD を作りました）",
          }),
          child_task_id: "01CHILD",
        }),
      ),
    );

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "merge" });

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) throw new Error("unreachable");
    expect(outcome.result.integration.state).toBe("conflict");
    expect(outcome.result.child_task_id).toBe("01CHILD");
    expect(outcome.result.integration.detail).toContain("01CHILD");
  });

  it("git の失敗も 200（state: failed + detail）", async () => {
    onIntegrate((_req, res) =>
      sendJson(
        res,
        200,
        integrateResult({ integration: taskIntegration({ state: "failed", detail: "git push failed: permission" }) }),
      ),
    );

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "pr" });

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) throw new Error("unreachable");
    expect(outcome.result.integration.state).toBe("failed");
    expect(outcome.result.integration.detail).toBe("git push failed: permission");
  });

  it("401 unauthorized（管理系。トークン未設定）もそのまま", async () => {
    onIntegrate((_req, res) => sendProblem(res, { status: 401, code: "unauthorized", detail: "missing token" }));

    const outcome = await integrateChange(client, "t1", "benchfs", { method: "merge" });

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.code).toBe("unauthorized");
  });
});

describe("mergePullRequest (POST /tasks/{id}/changes/{repo}/pr/merge)", () => {
  it("空の本文を送り、merged の記録を返す", async () => {
    mock.on("POST", "/api/v1/tasks/t1/changes/benchfs/pr/merge", (_req, res) =>
      sendJson(
        res,
        200,
        integrateResult({
          integration: taskIntegration({
            method: "pr",
            state: "merged",
            pr_number: 42,
            pr_url: "https://github.test/example/benchfs/pull/42",
            merged_at: "2026-09-19T12:00:00Z",
          }),
        }),
      ),
    );

    const outcome = await mergePullRequest(client, "t1", "benchfs");

    expect(outcome.ok).toBe(true);
    if (!outcome.ok) throw new Error("unreachable");
    expect(outcome.op).toBe("pr_merge");
    expect(outcome.result.integration.state).toBe("merged");
    expect(outcome.result.integration.pr_number).toBe(42);
    expect(JSON.parse(mock.requests[0].body)).toEqual({});
  });

  it("開いている PR が無ければ 409 pr_unavailable", async () => {
    mock.on("POST", "/api/v1/tasks/t1/changes/benchfs/pr/merge", (_req, res) =>
      sendProblem(res, { status: 409, code: "pr_unavailable", detail: "no open pull request for benchfs" }),
    );

    const outcome = await mergePullRequest(client, "t1", "benchfs");

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.status).toBe(409);
    expect(outcome.error.detail).toBe("no open pull request for benchfs");
  });
});

describe("案件の「PR と取り込み」（GET /projects/{id}/integrations）", () => {
  const detail: ProjectDetail = {
    project: {
      id: "p1",
      title: "benchfs",
      request: "…",
      status: "active",
      created_at: "2026-09-19T00:00:00Z",
      updated_at: "2026-09-19T00:00:00Z",
    },
    milestones: [],
    tasks: [],
  };

  it("celeris の並び（新しい順）をそのまま通す", async () => {
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/projects/p1/integrations", (_req, res) => sendJson(res, 200, defaultProjectIntegrations));

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.integrations).toEqual(defaultProjectIntegrations.items);
    expect(result.integrations[0].integration.pr_number).toBe(42);
    expect(result.integrations[0].task_title).toBe("ベンチマークの並列化");
    expect(result.integrations[1].integration.method).toBe("merge");
  });

  it("404 project_not_found でも案件の詳細自体は出る（節は空になる）", async () => {
    mock.on("GET", "/api/v1/projects/p1", (_req, res) => sendJson(res, 200, detail));
    mock.on("GET", "/api/v1/projects/p1/integrations", (_req, res) =>
      sendProblem(res, { status: 404, code: "project_not_found", detail: "no project p1" }),
    );

    const result = await loadProjectDetail(client, "p1", new Request("http://gui.invalid/projects/p1"));

    expect(result.integrations).toEqual([]);
    expect(result.detail.project.title).toBe("benchfs");
  });
});

describe("画面の作り（ソースの確認。G10-U1 の制約）", () => {
  const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
  const component = read("../../app/components/task-changes.tsx");
  const route = read("../../app/routes/tasks.$id.changes.tsx");
  const routes = read("../../app/routes.ts");
  const taskDetail = read("../../app/routes/tasks.$id.tsx");
  const projectDetail = read("../../app/routes/projects.$id.tsx");
  const projectIntegrations = read("../../app/components/ProjectIntegrations.tsx");

  it("部品は自己完結（ルートは TaskChanges を 1 つ載せるだけ）", () => {
    expect(route).toContain("<TaskChanges");
    expect(component).toContain("export function TaskChanges");
  });

  it("クライアントから celeris を呼ばない（fetch も CELERIS_API_URL も無い）", () => {
    expect(component).not.toContain("fetch(");
    expect(component).not.toContain("CELERIS_API_URL");
    expect(projectIntegrations).not.toContain("fetch(");
  });

  it("差分は dangerouslySetInnerHTML を使わずに描く", () => {
    expect(component).not.toContain("dangerouslySetInnerHTML");
    expect(component).toContain("parseDiff");
  });

  it("409 `default_branch_busy` の文言を専用の Alert でも出す", () => {
    expect(component).toContain('data-testid="task-changes-busy"');
    expect(component).toContain('=== "default_branch_busy"');
  });

  it("取り込みの 2 つの intent がルートの action にある", () => {
    expect(route).toMatch(/export async function action/);
    expect(route).toContain('case "integrate":');
    expect(route).toContain('case "pr_merge":');
  });

  // マージ（Phase 54）: ADR-0044 B1 のタブの殻ができたので、部品は `/tasks/:id?tab=changes` に載り、
  // 兄弟のルート `/tasks/:id/changes` は差分のリンクと取り込みの送り先として残っている。
  it("兄弟のルートとして登録され、タスク詳細の「変更」タブに載っている", () => {
    expect(routes).toContain('route("tasks/:id/changes", "routes/tasks.$id.changes.tsx")');
    expect(taskDetail).toContain("?tab=changes`}");
    expect(taskDetail).toContain('data-testid="task-changes-link"');
    // タブの中身は同じ部品で、loader は `?tab=changes` のときだけ `GET /tasks/{id}/changes` を引く。
    expect(taskDetail).toContain('import { TaskChanges } from "~/components/task-changes"');
    expect(taskDetail).toContain('parseTaskTab(url.searchParams.get("tab")) === "changes"');
    expect(taskDetail).toContain("loadTaskChanges(client, taskId, readTaskChangesQuery(request)");
    expect(taskDetail).toContain('data-testid="task-changes-unavailable"');
    // 取り込みの `fetcher` と差分の `<Link>` の送り先は兄弟のルートのまま。
    expect(component).toContain("}/changes`}");
  });

  it("PR のリンクは別タブで開き、opener を渡さない", () => {
    expect(component).toContain('rel="noreferrer noopener"');
    expect(projectIntegrations).toContain('rel="noreferrer noopener"');
  });

  it("案件画面に「PR と取り込み」節がある（GET /projects/{id}/integrations）", () => {
    expect(projectDetail).toContain("/integrations`");
    expect(projectDetail).toContain("<ProjectIntegrations items={integrations} />");
    expect(projectDetail).toContain("PR と取り込み");
  });
});
