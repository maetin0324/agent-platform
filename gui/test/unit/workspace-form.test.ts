import { describe, expect, it } from "vitest";
import type { WorkspaceSpec } from "~/celeris/types";
import { readWorkspaceFromForm, workspaceKindOf, workspaceSummaryText } from "~/lib/workspace-form";

/**
 * 案件の作業場所（ADR-0039 D1、Phase G13k）のフォーム入出力の純粋関数。
 * `~/components/WorkspaceFields.tsx` / `~/celeris/projects-admin.server.ts` / `~/celeris/conversation.server.ts`
 * が共有する読み手・表示のロジックをここでまとめて検証する。
 */

function form(entries: Array<[string, string]>): FormData {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
}

describe("readWorkspaceFromForm", () => {
  it("workspace_kind が無ければ null（まだ決めない）", () => {
    expect(readWorkspaceFromForm(new FormData())).toBeNull();
  });

  it("undecided は null", () => {
    expect(readWorkspaceFromForm(form([["workspace_kind", "undecided"]]))).toBeNull();
  });

  it("local は {kind:'local', path}", () => {
    expect(
      readWorkspaceFromForm(
        form([
          ["workspace_kind", "local"],
          ["workspace_path", "~/workspace/rust/pluvio-poc"],
        ]),
      ),
    ).toEqual({ kind: "local", path: "~/workspace/rust/pluvio-poc" });
  });

  it("remote は {kind:'remote', cluster, path}", () => {
    expect(
      readWorkspaceFromForm(
        form([
          ["workspace_kind", "remote"],
          ["workspace_cluster", "pegasus"],
          ["workspace_path", "/work/NBB/rmaeda/workspace/rust/benchfs"],
        ]),
      ),
    ).toEqual({ kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/workspace/rust/benchfs" });
  });

  it("空のパス・知らない cluster も検証せずそのまま組む（celeris の 422 に委ねる）", () => {
    expect(readWorkspaceFromForm(form([["workspace_kind", "remote"]]))).toEqual({
      kind: "remote",
      cluster: "",
      path: "",
    });
  });
});

describe("workspaceKindOf", () => {
  it("無ければ undecided", () => {
    expect(workspaceKindOf(null)).toBe("undecided");
    expect(workspaceKindOf(undefined)).toBe("undecided");
  });

  it("あれば kind をそのまま", () => {
    expect(workspaceKindOf({ kind: "local", path: "x" })).toBe("local");
    expect(workspaceKindOf({ kind: "remote", cluster: "c", path: "x" })).toBe("remote");
  });
});

describe("workspaceSummaryText", () => {
  it("無ければ null（呼び出し側が未設定の注意文を出す）", () => {
    expect(workspaceSummaryText(null)).toBeNull();
  });

  it("local はパスそのまま", () => {
    const workspace: WorkspaceSpec = { kind: "local", path: "/home/user/workspace/rust/pluvio-poc" };
    expect(workspaceSummaryText(workspace)).toBe("/home/user/workspace/rust/pluvio-poc");
  });

  it("remote は cluster:path", () => {
    const workspace: WorkspaceSpec = { kind: "remote", cluster: "pegasus", path: "/work/NBB/rmaeda/benchfs" };
    expect(workspaceSummaryText(workspace)).toBe("pegasus:/work/NBB/rmaeda/benchfs");
  });
});
