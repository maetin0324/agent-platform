import { useState } from "react";
import type { ActionError } from "~/celeris/action-types";
import type { ClusterView } from "~/celeris/types";
import { FieldErrors } from "~/components/Flash";
import { hintClass, inputClass, labelClass, selectClass } from "~/components/ui/form";
import type { WorkspaceKind } from "~/lib/workspace-form";

/**
 * 案件の作業場所（ADR-0039 D1、Phase G13k）の入力欄。「手元」（パス）／「クラスタ」（`GET /clusters` から
 * 選ぶ + リモートのパス）／「まだ決めない」を切り替える。`/projects` の新規フォーム、秘書の
 * 「新しい案件として」（`~/components/Conversation.tsx`）、`/projects/:id` の編集カードが共有する
 * （読み手は `~/lib/workspace-form.ts::readWorkspaceFromForm`）。
 * GUI 側では検証しない: 空のパス・知らないクラスタもそのまま送り、celeris の 422（`errors[].field =
 * "workspace.cluster"`）を `FieldErrors` でそのまま出す。
 */
export interface WorkspaceFieldsProps {
  /** id・htmlFor の接頭辞（同じページに複数のインスタンスがある場合の衝突を避ける）。 */
  idPrefix: string;
  clusters: readonly ClusterView[];
  defaultKind?: WorkspaceKind;
  defaultPath?: string;
  defaultCluster?: string;
  /** 「まだ決めない」を選べるか。新規フォームは `true`（既定）、編集カードは `false`（消去は別ボタン）。 */
  allowUndecided?: boolean;
  error?: ActionError | undefined | null;
}

export function WorkspaceFields({
  idPrefix,
  clusters,
  defaultKind = "undecided",
  defaultPath = "",
  defaultCluster,
  allowUndecided = true,
  error,
}: WorkspaceFieldsProps) {
  const [kind, setKind] = useState<WorkspaceKind>(defaultKind);

  return (
    <div className="space-y-3" data-testid="workspace-fields">
      <div>
        <label htmlFor={`${idPrefix}-kind`} className={labelClass}>
          作業場所
        </label>
        <select
          id={`${idPrefix}-kind`}
          name="workspace_kind"
          data-testid="project-workspace-kind"
          value={kind}
          onChange={(e) => setKind(e.target.value as WorkspaceKind)}
          className={`${selectClass} mt-1.5 w-full max-w-xs`}
        >
          {allowUndecided && <option value="undecided">まだ決めない</option>}
          <option value="local">手元</option>
          <option value="remote">クラスタ</option>
        </select>
      </div>
      {kind === "remote" && (
        <div>
          <label htmlFor={`${idPrefix}-cluster`} className={labelClass}>
            クラスタ
          </label>
          <select
            id={`${idPrefix}-cluster`}
            name="workspace_cluster"
            data-testid="project-workspace-cluster"
            defaultValue={defaultCluster ?? ""}
            className={`${selectClass} mt-1.5 w-full max-w-xs`}
          >
            <option value="">選んでください</option>
            {clusters.map((c) => (
              <option key={c.id} value={c.id}>
                {c.id}
              </option>
            ))}
          </select>
          <FieldErrors error={error} field="workspace.cluster" />
        </div>
      )}
      {(kind === "local" || kind === "remote") && (
        <div>
          <label htmlFor={`${idPrefix}-path`} className={labelClass}>
            パス
          </label>
          <input
            id={`${idPrefix}-path`}
            name="workspace_path"
            type="text"
            data-testid="project-workspace-path"
            defaultValue={defaultPath}
            placeholder={kind === "local" ? "~/workspace/rust/pluvio-poc" : "/work/NBB/rmaeda/workspace/rust/benchfs"}
            className={`${inputClass} mt-1.5 w-full`}
          />
          <p className={hintClass}>
            {kind === "local"
              ? "普段のパス（SPEC §2.1）。~ から始めれば celeris の $HOME で展開して保存されます。"
              : "クラスタ側の作業ディレクトリ（既存のリポジトリでかまいません）。"}
          </p>
        </div>
      )}
    </div>
  );
}
