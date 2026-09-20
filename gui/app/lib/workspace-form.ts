import { formString } from "~/celeris/forms";
import type { WorkspaceSpec } from "~/celeris/types";

/**
 * 案件の作業場所（ADR-0039 D1、docs/celeris-api-v1.md §3.46〜3.48。Phase G13k）のフォーム入出力。
 * `/projects` の新規フォーム、秘書の「新しい案件として」（`~/components/Conversation.tsx`）、
 * `/projects/:id` の編集カードが共有する（`~/components/WorkspaceFields.tsx` と対になる純粋関数）。
 * GUI 側では検証しない: 空の `path` / 知らない `cluster` もそのまま celeris に送り、422 の文言
 * （`errors[].field = "workspace.cluster"`）をそのまま出す（`~/celeris/projects-admin.server.ts` と同じ規律）。
 */

/** 3 択（「まだ決めない」／「手元」／「クラスタ」）。`WorkspaceSpec` が無ければ `undecided`。 */
export type WorkspaceKind = "undecided" | "local" | "remote";

/** 案件の現在の作業場所から選択肢の初期値を作る（編集フォームの `defaultKind` に使う）。 */
export function workspaceKindOf(workspace: WorkspaceSpec | null | undefined): WorkspaceKind {
  return workspace ? workspace.kind : "undecided";
}

/**
 * フォーム（`workspace_kind` / `workspace_path` / `workspace_cluster`）から `WorkspaceSpec` を組む。
 * `undecided`（未選択を含む）は `null`（＝「作業場所なし」）。
 */
export function readWorkspaceFromForm(form: FormData): WorkspaceSpec | null {
  const kind = formString(form, "workspace_kind");
  if (kind === "local") {
    return { kind: "local", path: formString(form, "workspace_path") ?? "" };
  }
  if (kind === "remote") {
    return {
      kind: "remote",
      cluster: formString(form, "workspace_cluster") ?? "",
      path: formString(form, "workspace_path") ?? "",
    };
  }
  return null;
}

/** 案件画面のカードに出す現在値の 1 行（SPEC §2.1「普段のパス」）。未設定は `null`（呼び出し側が注意文を出す）。 */
export function workspaceSummaryText(workspace: WorkspaceSpec | null | undefined): string | null {
  if (!workspace) return null;
  return workspace.kind === "remote" ? `${workspace.cluster}:${workspace.path}` : workspace.path;
}
