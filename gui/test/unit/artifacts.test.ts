import { describe, expect, it } from "vitest";
import type { ArtifactView, OrgNode, ProjectTaskView, WorkspaceSpec } from "~/celeris/types";
import {
  buildProjectArtifactRows,
  isSourcesArtifact,
  parseSourcesJson,
  resolveAssigneeName,
  type TaskArtifactBundle,
  workspacePlace,
} from "~/lib/artifacts";

const artifact = (over: Partial<ArtifactView> = {}): ArtifactView => ({
  idx: 0,
  run_id: "run1",
  ts: "2026-09-17T00:00:00Z",
  artifact: { name: "report.md", path: "artifacts/report.md", sha256: "abc", kind: "markdown" },
  exists: true,
  forbidden: false,
  size: 10,
  sha256_current: "abc",
  sha256_matches: true,
  ...over,
});

describe("workspacePlace", () => {
  it("local: workspace_dir をそのまま出し、vscode リンクを添える", () => {
    const workspace: WorkspaceSpec = { kind: "local", path: "rust/brainfuck" };
    expect(workspacePlace(workspace, "/home/user/workspace/rust/brainfuck")).toEqual({
      text: "/home/user/workspace/rust/brainfuck",
      vscodeHref: "vscode://file/home/user/workspace/rust/brainfuck",
      localCopyNote: null,
    });
  });

  it("local: workspace_dir が無ければ生の path にフォールバックする", () => {
    const workspace: WorkspaceSpec = { kind: "local", path: "rust/brainfuck" };
    expect(workspacePlace(workspace, null)).toEqual({
      text: "rust/brainfuck",
      vscodeHref: "vscode://file/rust/brainfuck",
      localCopyNote: null,
    });
  });

  it("remote: cluster:path で出し、リンクは付けない（クラスタ側のパスであってローカルではない）", () => {
    const workspace: WorkspaceSpec = { kind: "remote", cluster: "cl1", path: "code/proj" };
    expect(workspacePlace(workspace, null)).toEqual({
      text: "cl1:code/proj",
      vscodeHref: null,
      localCopyNote: null,
    });
  });

  it("remote: workspace_dir があれば「手元の写し」の案内文を添える（ADR-0039 D3、実機の事故 2026-09-18）", () => {
    const workspace: WorkspaceSpec = { kind: "remote", cluster: "cl1", path: "code/proj" };
    expect(workspacePlace(workspace, "/home/user/workspace/T1")).toEqual({
      text: "cl1:code/proj",
      vscodeHref: null,
      localCopyNote: "手元の写し: /home/user/workspace/T1",
    });
  });
});

describe("isSourcesArtifact", () => {
  it("sources.json という名前だけ true", () => {
    expect(isSourcesArtifact("sources.json")).toBe(true);
    expect(isSourcesArtifact("research.json")).toBe(false);
    expect(isSourcesArtifact("report.md")).toBe(false);
  });
});

describe("parseSourcesJson", () => {
  it("docs/adr/0031 の形（[{url, title, engine?, cited}]）をリンク集に変換する", () => {
    const text = JSON.stringify([
      { url: "https://a.example.com/1", title: "A", engine: "tavily", cited: true },
      { url: "https://b.example.com/2", title: "B", engine: null, cited: false },
    ]);
    expect(parseSourcesJson(text)).toEqual([
      { url: "https://a.example.com/1", title: "A", engine: "tavily", cited: true },
      { url: "https://b.example.com/2", title: "B", engine: null, cited: false },
    ]);
  });

  it("不正な JSON は null", () => {
    expect(parseSourcesJson("not json")).toBeNull();
  });

  it("配列でなければ null", () => {
    expect(parseSourcesJson(JSON.stringify({ url: "https://a.example.com" }))).toBeNull();
  });

  it("url/title が欠けている要素があれば null（通常の JSON 表示にフォールバックさせる）", () => {
    expect(parseSourcesJson(JSON.stringify([{ url: "https://a.example.com" }]))).toBeNull();
  });
});

describe("resolveAssigneeName", () => {
  const orgById = new Map<string, OrgNode>([
    [
      "research-survey",
      {
        id: "research-survey",
        parent_id: "research",
        name: "関連研究調査課",
        kind: "section",
        position: 0,
        created_at: "…",
        updated_at: "…",
      },
    ],
  ]);

  it("組織にあれば名前を返す", () => {
    expect(resolveAssigneeName("research-survey", orgById)).toBe("関連研究調査課");
  });

  it("組織に無ければ id をそのまま返す", () => {
    expect(resolveAssigneeName("unknown-id", orgById)).toBe("unknown-id");
  });

  it("assignee が無ければ null", () => {
    expect(resolveAssigneeName(null, orgById)).toBeNull();
    expect(resolveAssigneeName(undefined, orgById)).toBeNull();
  });
});

describe("buildProjectArtifactRows", () => {
  const tasks: Pick<ProjectTaskView, "id" | "title" | "status" | "assignee">[] = [
    { id: "t1", title: "survey", status: "done", assignee: "research-survey" },
    { id: "t2", title: "poc", status: "running", assignee: null },
  ];
  const orgById = new Map<string, OrgNode>([
    [
      "research-survey",
      {
        id: "research-survey",
        parent_id: "research",
        name: "関連研究調査課",
        kind: "section",
        position: 0,
        created_at: "…",
        updated_at: "…",
      },
    ],
  ]);

  it("タスクごとに束ねた成果物を、新しい順（ts 降順）に平らにする", () => {
    const workspace1 = { text: "rust/brainfuck", vscodeHref: "vscode://file/rust/brainfuck", localCopyNote: null };
    const workspace2 = { text: "cl1:code/proj", vscodeHref: null, localCopyNote: null };
    const bundles = new Map<string, TaskArtifactBundle>([
      [
        "t1",
        {
          workspace: workspace1,
          artifacts: [
            artifact({ idx: 0, ts: "2026-09-17T00:00:00Z", artifact: { ...artifact().artifact, name: "report.md" } }),
          ],
        },
      ],
      [
        "t2",
        {
          workspace: workspace2,
          artifacts: [
            artifact({
              idx: 0,
              ts: "2026-09-17T01:00:00Z",
              artifact: { ...artifact().artifact, name: "sources.json" },
            }),
          ],
        },
      ],
    ]);

    const rows = buildProjectArtifactRows(tasks, bundles, orgById);

    expect(rows).toHaveLength(2);
    expect(rows[0].artifact.artifact.name).toBe("sources.json");
    expect(rows[0].taskId).toBe("t2");
    expect(rows[0].assigneeName).toBeNull();
    expect(rows[0].workspace).toEqual(workspace2);
    expect(rows[1].artifact.artifact.name).toBe("report.md");
    expect(rows[1].taskId).toBe("t1");
    expect(rows[1].assigneeName).toBe("関連研究調査課");
    expect(rows[1].workspace).toEqual(workspace1);
  });

  it("bundle が無いタスク（取得失敗）は行を出さない", () => {
    const rows = buildProjectArtifactRows(tasks, new Map(), orgById);
    expect(rows).toEqual([]);
  });
});

// フェーズ 74（ADR-0055 D2 ラウンド 6）: `artifactRelativeTime` は `~/lib/reports.ts::relativeTimeLabel` と
// 実装が重複していたので削除し、`relativeTimeLabel` に一本化した（テストは test/unit/reports.test.ts）。
