import type { ProjectRepo, WorkspaceSpec } from "~/taskd/types";

/**
 * 案件のリポジトリ（ADR-0043 D1、docs/taskd-api-v1.md §3.68〜3.71。Phase 52 / G16）のフォーム入出力。
 * `~/lib/workspace-form.ts`（案件 1 つぶんの作業場所）の兄弟で、そちらは触らない。
 * `~/components/RepoFields.tsx`（入力欄）と `~/taskd/repos-admin.server.ts`（`RepoCreateBody` /
 * `RepoPatchBody` の組み立て）が共有する純粋関数をここに置く。
 *
 * GUI 側では検証しない（`~/taskd/projects-admin.server.ts` と同じ規律）: 空のパス・知らないクラスタ・
 * 不正な slug もそのまま taskd に送り、422 `validation` / 409 `repo_in_use` の文言をそのまま出す。
 */

/** 置き場所の 2 択（「手元」＝ `WorkspaceSpec::Local` ／「クラスタ」＝ `Remote`）。 */
export type RepoPlace = "local" | "remote";

/** 入力欄 1 組ぶんの生の値（`~/components/RepoFields.tsx` の `name` と 1:1）。空欄は `null`。 */
export interface RepoFormValues {
  name: string | null;
  /** `""` / `"auto"` は「taskd に決めさせる」（`kind` を送らない）。 */
  kind: string | null;
  place: string | null;
  path: string | null;
  cluster: string | null;
  defaultBranch: string | null;
  run: string | null;
}

/** 既存の行の置き場所（編集フォームの初期値）。 */
export function repoPlaceOf(location: WorkspaceSpec | null | undefined): RepoPlace {
  return location?.kind === "remote" ? "remote" : "local";
}

/**
 * `place` / `path` / `cluster` から `WorkspaceSpec` を組む（`RepoCreateBody.location` は必須なので
 * 常に 1 つ返す）。`remote` 以外（未選択を含む）は `local` 扱い。値の妥当性は taskd が決める。
 */
export function repoLocationFrom(values: Pick<RepoFormValues, "place" | "path" | "cluster">): WorkspaceSpec {
  const path = values.path ?? "";
  if (values.place === "remote") {
    return { kind: "remote", cluster: values.cluster ?? "", path };
  }
  return { kind: "local", path };
}

/** 一覧の 1 行に出す置き場所（手元はパスそのまま、クラスタは `cluster:path`）。 */
export function repoLocationText(location: WorkspaceSpec): string {
  return location.kind === "remote" ? `${location.cluster}:${location.path}` : location.path;
}

/**
 * 並びは taskd が決めたもの（primary が先頭、あとは作った順。§3.68）をそのまま使う。
 * GUI では並べ替えない。primary の 1 件だけを取り出したいときのための小さな補助。
 */
export function primaryRepo(repos: readonly ProjectRepo[] | undefined): ProjectRepo | null {
  return repos?.find((r) => r.is_primary) ?? null;
}
