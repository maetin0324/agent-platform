import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { binaryFileLabel, fileSizeLabel, tooLargeFileLabel, treeEntryKindLabel } from "~/lib/labels";
import { fileBody, isJsonPath, parentPath, pickTreeFileViewer, taskFilesHref, treeBreadcrumbs } from "~/lib/task-files";
import { TaskdClient } from "~/taskd/client.server";
import { loadTaskFiles, readTaskFilesQuery } from "~/taskd/task-files";
import { treeFileView, treeView } from "../mock-taskd/fixtures";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * タスクの作業ツリーの閲覧（ADR-0043 D6、docs/taskd-api-v1.md §3.72〜3.73。Phase 52 / G16）。
 * DOM を描画する unit テストが無い（G10-U1）ので、表示の判断は `~/lib/task-files.ts` の純粋関数、
 * 取得は `~/taskd/task-files.ts` の loader で見る。
 */

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("treeBreadcrumbs / parentPath", () => {
  it("根は空（呼び出し側がリポジトリ名で根のリンクを出す）", () => {
    expect(treeBreadcrumbs("")).toEqual([]);
    expect(parentPath("")).toBeNull();
  });

  it("1 段", () => {
    expect(treeBreadcrumbs("src")).toEqual([{ name: "src", path: "src" }]);
    expect(parentPath("src")).toBe("");
  });

  it("2 段以上は累積のパスを持つ", () => {
    expect(treeBreadcrumbs("src/bin/x")).toEqual([
      { name: "src", path: "src" },
      { name: "bin", path: "src/bin" },
      { name: "x", path: "src/bin/x" },
    ]);
    expect(parentPath("src/bin/x")).toBe("src/bin");
  });

  it("余分な区切りは落とす", () => {
    expect(treeBreadcrumbs("/src//bin/")).toEqual([
      { name: "src", path: "src" },
      { name: "bin", path: "src/bin" },
    ]);
  });
});

describe("taskFilesHref", () => {
  it("空の値はクエリに出さない", () => {
    expect(taskFilesHref("t1", {})).toBe("/tasks/t1/files");
    expect(taskFilesHref("t1", { repo: "benchfs" })).toBe("/tasks/t1/files?repo=benchfs");
  });

  it("ディレクトリを開く・ファイルを選ぶ", () => {
    expect(taskFilesHref("t1", { repo: "benchfs", path: "src" })).toBe("/tasks/t1/files?repo=benchfs&path=src");
    expect(taskFilesHref("t1", { repo: "benchfs", path: "src", file: "src/lib.rs" })).toBe(
      "/tasks/t1/files?repo=benchfs&path=src&file=src%2Flib.rs",
    );
  });
});

describe("fileBody（§3.73: text は 512 KiB 以下のテキストのときだけ）", () => {
  it("テキストは本文を出す", () => {
    const body = fileBody(treeFileView({ text: "hello\n", size: 6 }));
    expect(body).toEqual({ kind: "text", text: "hello\n", message: null });
  });

  it("binary は本文を出さず大きさだけ", () => {
    const body = fileBody(treeFileView({ path: "a.png", binary: true, size: 2048, text: undefined }));
    expect(body.kind).toBe("binary");
    expect(body.text).toBeNull();
    expect(body.message).toBe("バイナリのため表示しません（2.0 KiB）");
    expect(body.message).toBe(binaryFileLabel(2048));
  });

  it("too_large は本文を出さず大きさだけ", () => {
    const body = fileBody(treeFileView({ path: "big.log", too_large: true, size: 1_048_576, text: undefined }));
    expect(body.kind).toBe("too_large");
    expect(body.text).toBeNull();
    expect(body.message).toBe("512 KiB を超えるため表示しません（1.0 MiB）");
    expect(body.message).toBe(tooLargeFileLabel(1_048_576));
  });

  it("text が無いテキスト（空ファイル）は空文字として出す", () => {
    expect(fileBody(treeFileView({ size: 0, text: undefined }))).toEqual({ kind: "text", text: "", message: null });
  });
});

describe("ビューアの選択と大きさの表示", () => {
  it("Markdown は MarkdownViewer、ほかは CodeViewer", () => {
    expect(pickTreeFileViewer("README.md")).toBe("markdown");
    expect(pickTreeFileViewer("docs/a.MARKDOWN")).toBe("markdown");
    expect(pickTreeFileViewer("src/lib.rs")).toBe("code");
  });

  it("JSON は構文強調を使う", () => {
    expect(isJsonPath("Cargo.json")).toBe(true);
    expect(isJsonPath("Cargo.toml")).toBe(false);
  });

  it("バイト数は 1024 進", () => {
    expect(fileSizeLabel(0)).toBe("0 B");
    expect(fileSizeLabel(512)).toBe("512 B");
    expect(fileSizeLabel(1024)).toBe("1.0 KiB");
    expect(fileSizeLabel(1536)).toBe("1.5 KiB");
    expect(fileSizeLabel(1_048_576)).toBe("1.0 MiB");
  });

  it("entry の kind の日本語（知らない値は素のまま）", () => {
    expect(treeEntryKindLabel("dir")).toBe("ディレクトリ");
    expect(treeEntryKindLabel("file")).toBe("ファイル");
    expect(treeEntryKindLabel("other")).toBe("その他");
    expect(treeEntryKindLabel("socket")).toBe("socket");
  });
});

describe("readTaskFilesQuery", () => {
  it("?repo=&path=&file= をそのまま読む（無ければ null）", () => {
    expect(readTaskFilesQuery(new Request("http://gui.invalid/tasks/t1/files"))).toEqual({
      repo: null,
      path: null,
      file: null,
    });
    expect(
      readTaskFilesQuery(new Request("http://gui.invalid/tasks/t1/files?repo=benchfs&path=src&file=src%2Flib.rs")),
    ).toEqual({ repo: "benchfs", path: "src", file: "src/lib.rs" });
  });
});

describe("loadTaskFiles (GET /tasks/{id}/tree, §3.72)", () => {
  it("一覧: taskd の並び（ディレクトリが先）とリポジトリの一覧をそのまま返す", async () => {
    const tree = treeView();
    mock.on("GET", "/api/v1/tasks/t1/tree", (_req, res) => sendJson(res, 200, tree));

    const result = await loadTaskFiles(client, "t1", {});

    expect(result.tree).toEqual(tree);
    expect(result.tree.entries.map((e) => e.name)).toEqual(["src", "Cargo.toml", "README.md"]);
    expect(result.tree.repos.map((r) => r.name)).toEqual(["benchfs", "benchfs-paper"]);
    expect(result.file).toBeNull();
    expect(result.fileError).toBeNull();
    expect(result.filePath).toBeNull();
    // repo / path を省略したら taskd に渡さない（先頭のリポジトリ・根は taskd が決める）。
    const url = new URL(mock.requests[0].url, "http://mock-taskd.invalid");
    expect(url.searchParams.has("repo")).toBe(false);
    expect(url.searchParams.has("path")).toBe(false);
  });

  it("repo / path を指定したらクエリに載せる", async () => {
    mock.on("GET", "/api/v1/tasks/t1/tree", (_req, res) => sendJson(res, 200, treeView({ repo: "data", path: "src" })));

    await loadTaskFiles(client, "t1", { repo: "data", path: "src" });

    const url = new URL(mock.requests[0].url, "http://mock-taskd.invalid");
    expect(url.searchParams.get("repo")).toBe("data");
    expect(url.searchParams.get("path")).toBe("src");
  });

  it("ファイルを選ぶと GET /tasks/{id}/tree/file を一覧と同じ repo で引く", async () => {
    mock.on("GET", "/api/v1/tasks/t1/tree", (_req, res) => sendJson(res, 200, treeView()));
    mock.on("GET", "/api/v1/tasks/t1/tree/file", (_req, res) => sendJson(res, 200, treeFileView()));

    const result = await loadTaskFiles(client, "t1", { file: "README.md" });

    expect(result.filePath).toBe("README.md");
    expect(result.file?.text).toContain("# benchfs");
    expect(result.fileError).toBeNull();
    const fileReq = mock.requests.find((r) => r.url.startsWith("/api/v1/tasks/t1/tree/file"));
    const url = new URL(fileReq?.url ?? "", "http://mock-taskd.invalid");
    expect(url.searchParams.get("repo")).toBe("benchfs");
    expect(url.searchParams.get("path")).toBe("README.md");
  });

  it("binary / too_large はそのまま通す（本文は付かない）", async () => {
    mock.on("GET", "/api/v1/tasks/t1/tree", (_req, res) => sendJson(res, 200, treeView()));
    mock.on("GET", "/api/v1/tasks/t1/tree/file", (_req, res) =>
      sendJson(res, 200, treeFileView({ path: "a.png", binary: true, size: 2048, text: undefined })),
    );

    const result = await loadTaskFiles(client, "t1", { file: "a.png" });

    expect(result.file?.binary).toBe(true);
    expect(result.file?.text).toBeUndefined();
    expect(fileBody(result.file as NonNullable<typeof result.file>).message).toBe(binaryFileLabel(2048));
  });

  it("403 path_forbidden は一覧を出したまま taskd の文言を fileError に載せる", async () => {
    mock.on("GET", "/api/v1/tasks/t1/tree", (_req, res) => sendJson(res, 200, treeView()));
    mock.on("GET", "/api/v1/tasks/t1/tree/file", (_req, res) =>
      sendProblem(res, {
        status: 403,
        code: "path_forbidden",
        detail: "path escapes the work tree: ../../etc/passwd",
      }),
    );

    const result = await loadTaskFiles(client, "t1", { file: "../../etc/passwd" });

    expect(result.tree.entries).toHaveLength(3);
    expect(result.file).toBeNull();
    expect(result.filePath).toBe("../../etc/passwd");
    expect(result.fileError?.status).toBe(403);
    expect(result.fileError?.code).toBe("path_forbidden");
    expect(result.fileError?.detail).toBe("path escapes the work tree: ../../etc/passwd");
  });

  it("404 file_not_found（作業ツリーが無いタスク）は一覧の時点で例外になる（画面は ErrorBoundary）", async () => {
    mock.on("GET", "/api/v1/tasks/t1/tree", (_req, res) =>
      sendProblem(res, { status: 404, code: "file_not_found", detail: "task t1 has no work tree" }),
    );

    await expect(loadTaskFiles(client, "t1", {})).rejects.toThrow("task t1 has no work tree");
  });
});

describe("画面の作り（ソースの確認。G10-U1 の制約）", () => {
  const component = readFileSync(
    fileURLToPath(new URL("../../app/components/task-files.tsx", import.meta.url)),
    "utf8",
  );
  const route = readFileSync(fileURLToPath(new URL("../../app/routes/tasks.$id.files.tsx", import.meta.url)), "utf8");
  const routes = readFileSync(fileURLToPath(new URL("../../app/routes.ts", import.meta.url)), "utf8");

  it("部品は自己完結（ルートは TaskFiles を 1 行で載せるだけ）", () => {
    expect(route).toContain("<TaskFiles");
    expect(component).toContain("export function TaskFiles");
  });

  it("クライアントから taskd を呼ばない（fetch も TASKD_API_URL も無い）", () => {
    expect(component).not.toContain("fetch(");
    expect(component).not.toContain("TASKD_API_URL");
  });

  it("読み取りだけ（ルートに action が無い）", () => {
    expect(route).not.toMatch(/export async function action/);
  });

  it("兄弟のルートとして登録されている", () => {
    expect(routes).toContain('route("tasks/:id/files", "routes/tasks.$id.files.tsx")');
  });
});
