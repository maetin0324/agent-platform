import { useState } from "react";
import type { ActionError } from "~/celeris/action-types";
import type { ClusterView } from "~/celeris/types";
import { FieldErrors } from "~/components/Flash";
import { hintClass, inputClass, labelClass, selectClass } from "~/components/ui/form";
import { REPO_KIND_AUTO_LABEL, repoKindLabel, repoRunLabel } from "~/lib/labels";
import type { RepoPlace } from "~/lib/repo-form";

/**
 * 案件のリポジトリ（ADR-0043 D1、docs/celeris-api-v1.md §3.69〜3.70。Phase 52 / G16）の入力欄。
 * `~/components/WorkspaceFields.tsx`（案件 1 つぶんの作業場所）の兄弟で、そちらは触らない
 * （`/projects` の従来の `workspace` フォームは今までどおり動く）。
 *
 * 読み手は `~/celeris/repos-admin.server.ts`（`repoCreateBodyFrom` / `repoPatchBodyFrom`）。
 * **入力欄は常に全部描く**（クラスタの選択は `hidden` で隠すだけ）。`/projects` の「追加のリポジトリ」は
 * この組を繰り返して `form.getAll()` で列ごとに読むので、行ごとに欄が欠けると並びがずれるため。
 *
 * GUI 側では検証しない: 空のパス・知らないクラスタ・不正な slug もそのまま送り、celeris の 422 の文言を
 * `FieldErrors` / `ErrorFlash` でそのまま出す。
 */
export interface RepoFieldsDefaults {
  name?: string;
  /** `""`（＝自動） / `git` / `dir`。 */
  kind?: string;
  place?: RepoPlace;
  path?: string;
  cluster?: string;
  defaultBranch?: string;
  /** `auto` / `host` / `container`。 */
  run?: string;
}

export interface RepoFieldsProps {
  /** id・htmlFor の接頭辞（同じページに複数あるので必ず別の値にする）。 */
  idPrefix: string;
  /** フォームの `name` の接頭辞（`repo` → `repo_path` …、`extra_repo` → `extra_repo_path` …）。 */
  namePrefix?: string;
  clusters: readonly ClusterView[];
  /** 種類（git / ディレクトリ）を選べるか。追加は `true`、行の編集は `false`（種類は変えない）。 */
  showKind?: boolean;
  defaults?: RepoFieldsDefaults;
  error?: ActionError | undefined | null;
}

export function RepoFields({
  idPrefix,
  namePrefix = "repo",
  clusters,
  showKind = true,
  defaults = {},
  error,
}: RepoFieldsProps) {
  const [place, setPlace] = useState<RepoPlace>(defaults.place ?? "local");

  return (
    <div className="grid gap-3 sm:grid-cols-2" data-testid="repo-fields">
      <div>
        <label htmlFor={`${idPrefix}-name`} className={labelClass}>
          名前
        </label>
        <input
          id={`${idPrefix}-name`}
          name={`${namePrefix}_name`}
          type="text"
          data-testid="repo-name"
          defaultValue={defaults.name ?? ""}
          placeholder="benchfs（空ならパスの末尾から付きます）"
          className={`${inputClass} mt-1.5 w-full`}
        />
        <FieldErrors error={error} field="name" />
      </div>
      {showKind && (
        <div>
          <label htmlFor={`${idPrefix}-kind`} className={labelClass}>
            種類
          </label>
          <select
            id={`${idPrefix}-kind`}
            name={`${namePrefix}_kind`}
            data-testid="repo-kind"
            defaultValue={defaults.kind ?? ""}
            className={`${selectClass} mt-1.5 w-full`}
          >
            <option value="">{REPO_KIND_AUTO_LABEL}</option>
            <option value="git">{repoKindLabel("git")}</option>
            <option value="dir">{repoKindLabel("dir")}</option>
          </select>
          <FieldErrors error={error} field="kind" />
        </div>
      )}
      <div>
        <label htmlFor={`${idPrefix}-place`} className={labelClass}>
          置き場所
        </label>
        <select
          id={`${idPrefix}-place`}
          name={`${namePrefix}_place`}
          data-testid="repo-place"
          value={place}
          onChange={(e) => setPlace(e.target.value as RepoPlace)}
          className={`${selectClass} mt-1.5 w-full`}
        >
          <option value="local">手元</option>
          <option value="remote">クラスタ</option>
        </select>
      </div>
      {/* クラスタは `hidden` で隠すだけ（値は常に submit される。繰り返し行の並びをずらさないため）。 */}
      <div className={place === "remote" ? undefined : "hidden"}>
        <label htmlFor={`${idPrefix}-cluster`} className={labelClass}>
          クラスタ
        </label>
        <select
          id={`${idPrefix}-cluster`}
          name={`${namePrefix}_cluster`}
          data-testid="repo-cluster"
          defaultValue={defaults.cluster ?? ""}
          className={`${selectClass} mt-1.5 w-full`}
        >
          <option value="">選んでください</option>
          {clusters.map((c) => (
            <option key={c.id} value={c.id}>
              {c.id}
            </option>
          ))}
        </select>
        <FieldErrors error={error} field="location.cluster" />
      </div>
      <div className="sm:col-span-2">
        <label htmlFor={`${idPrefix}-path`} className={labelClass}>
          パス
        </label>
        <input
          id={`${idPrefix}-path`}
          name={`${namePrefix}_path`}
          type="text"
          data-testid="repo-path"
          defaultValue={defaults.path ?? ""}
          placeholder={place === "local" ? "~/workspace/rust/benchfs" : "/work/NBB/rmaeda/workspace/rust/benchfs"}
          className={`${inputClass} mt-1.5 w-full`}
        />
        <p className={hintClass}>
          {place === "local"
            ? "普段のパス（SPEC §2.1）。~ から始めれば celeris の $HOME で展開して保存されます。"
            : "クラスタ側の作業ディレクトリ。"}
        </p>
        <FieldErrors error={error} field="location.path" />
      </div>
      <div>
        <label htmlFor={`${idPrefix}-default-branch`} className={labelClass}>
          既定のブランチ
        </label>
        <input
          id={`${idPrefix}-default-branch`}
          name={`${namePrefix}_default_branch`}
          type="text"
          data-testid="repo-default-branch"
          defaultValue={defaults.defaultBranch ?? ""}
          placeholder="空なら celeris が検出（origin/HEAD → main → master）"
          className={`${inputClass} mt-1.5 w-full`}
        />
        <FieldErrors error={error} field="default_branch" />
      </div>
      <div>
        <label htmlFor={`${idPrefix}-run`} className={labelClass}>
          実行環境
        </label>
        <select
          id={`${idPrefix}-run`}
          name={`${namePrefix}_run`}
          data-testid="repo-run"
          defaultValue={defaults.run ?? "auto"}
          className={`${selectClass} mt-1.5 w-full`}
        >
          <option value="auto">{repoRunLabel("auto")}</option>
          <option value="host">{repoRunLabel("host")}</option>
          <option value="container">{repoRunLabel("container")}</option>
        </select>
        <p className={hintClass}>コンテナは ADR-0043 A3 の工事中で、いまは読まれるだけです。</p>
        <FieldErrors error={error} field="run" />
      </div>
    </div>
  );
}
