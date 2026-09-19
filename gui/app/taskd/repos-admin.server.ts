import { type RepoFormValues, repoLocationFrom } from "~/lib/repo-form";
import type { ProjectOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type { ProjectRepo, RepoCreateBody, RepoKind, RepoList, RepoPatchBody, RepoRun } from "./types";

/**
 * 案件のリポジトリ（ADR-0043 D1、docs/taskd-api-v1.md §3.68〜3.71。Phase 52 / G16）の中継。
 * `GET /projects/{id}/repos` は読み取り、`POST /projects/{id}/repos` / `PATCH /repos/{id}` /
 * `DELETE /repos/{id}` は**管理系**（`token_file` 未設定でも 401）。管理系かどうかで中継コードは変わらない
 * （`TaskdClient` が `Authorization` を付けるだけ。401 の案内文は `Flash.tsx` に集約されている）。
 *
 * GUI 側では検証しない（`./projects-admin.server.ts` と同じ規律）: 名前の形（slug）・重複・知らない
 * クラスタ・使用中の削除はすべて taskd が決め、422 `validation` / 409 `repo_in_use` の文言を
 * そのまま画面に出す。`is_primary` の付け替え（1 案件 1 つ）も taskd 側の規則で、GUI は再計算しない。
 */

/** `GET /projects/{id}/repos`（読み取り。primary が先頭、あとは作った順で taskd が並べる）。 */
export function listRepos(client: TaskdClient, projectId: string, signal?: AbortSignal): Promise<RepoList> {
  return client.get<RepoList>(`/projects/${encodeURIComponent(projectId)}/repos`, { signal });
}

/** `POST /projects/{id}/repos`（管理系、201 `ProjectRepo`）。 */
export async function createRepo(
  client: TaskdClient,
  projectId: string,
  body: RepoCreateBody,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const repo = await client.post<ProjectRepo>(`/projects/${encodeURIComponent(projectId)}/repos`, body, { signal });
    return { ok: true, op: "repo_create", repo };
  } catch (e) {
    return { ok: false, op: "repo_create", error: toActionError(e) };
  }
}

/** `PATCH /repos/{id}`（管理系、200 `ProjectRepo`）。書いたものだけ変える（1 つも書かなければ taskd が 422）。 */
export async function patchRepo(
  client: TaskdClient,
  repoId: string,
  body: RepoPatchBody,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const repo = await client.patch<ProjectRepo>(`/repos/${encodeURIComponent(repoId)}`, body, { signal });
    return { ok: true, op: "repo_patch", repo };
  } catch (e) {
    return { ok: false, op: "repo_patch", error: toActionError(e) };
  }
}

/**
 * 「主にする」（`PATCH /repos/{id}` の `is_primary: true`）。他の行の `is_primary` を落とすのは taskd 側
 * （§3.70）。`false` は何もしない仕様なので、GUI からは「主にする」だけを送る。
 */
export async function setPrimaryRepo(
  client: TaskdClient,
  repoId: string,
  signal?: AbortSignal,
): Promise<ProjectOpOutcome> {
  try {
    const repo = await client.patch<ProjectRepo>(
      `/repos/${encodeURIComponent(repoId)}`,
      { is_primary: true } satisfies RepoPatchBody,
      { signal },
    );
    return { ok: true, op: "repo_primary", repo };
  } catch (e) {
    return { ok: false, op: "repo_primary", error: toActionError(e) };
  }
}

/**
 * `DELETE /repos/{id}`（管理系、204）。未終端のタスクが使っていると 409 `repo_in_use`。
 * primary を消したときに残りのどれが primary になるかは taskd が決める（§3.71）。
 */
export async function deleteRepo(client: TaskdClient, repoId: string, signal?: AbortSignal): Promise<ProjectOpOutcome> {
  try {
    await client.delete<unknown>(`/repos/${encodeURIComponent(repoId)}`, { signal });
    return { ok: true, op: "repo_delete", repoId };
  } catch (e) {
    return { ok: false, op: "repo_delete", error: toActionError(e) };
  }
}

/** 入力欄 1 組（`<prefix>_name` / `_kind` / `_place` / `_path` / `_cluster` / `_default_branch` / `_run`）を読む。 */
function readRepoFormValues(form: FormData, prefix: string): RepoFormValues {
  return {
    name: formString(form, `${prefix}_name`),
    kind: formString(form, `${prefix}_kind`),
    place: formString(form, `${prefix}_place`),
    path: formString(form, `${prefix}_path`),
    cluster: formString(form, `${prefix}_cluster`),
    defaultBranch: formString(form, `${prefix}_default_branch`),
    run: formString(form, `${prefix}_run`),
  };
}

/**
 * `RepoCreateBody` を組む（§3.69）。**空欄はキーごと送らない**（taskd の既定に委ねる）:
 * `name` を省略するとパスの末尾から slug、`kind` を省略すると `.git` の有無で決まり、`run` を省略すると
 * `auto`。`kind` の選択肢の `auto` は「taskd に決めさせる」なので同じくキーを送らない
 * （`RepoKind` に `auto` は無い）。`location` だけは必須なので常に送る。
 */
export function repoCreateBodyFrom(values: RepoFormValues): RepoCreateBody {
  const body: RepoCreateBody = { location: repoLocationFrom(values) };
  if (values.name) body.name = values.name;
  if (values.kind && values.kind !== "auto") body.kind = values.kind as RepoKind;
  if (values.defaultBranch) body.default_branch = values.defaultBranch;
  if (values.run) body.run = values.run as RepoRun;
  return body;
}

/** 案件詳細の「リポジトリを追加」フォーム（`repo_*`）から `RepoCreateBody` を組む。 */
export function readRepoCreateBody(form: FormData, prefix = "repo"): RepoCreateBody {
  return repoCreateBodyFrom(readRepoFormValues(form, prefix));
}

/**
 * `RepoPatchBody` を組む（§3.70）。行の編集フォームは現在値で埋まっているので `name` / `location` /
 * `run` は常に送り、`default_branch` は**空欄なら `null`（消す）**を明示する（`git` 以外では taskd が
 * どのみち落とす）。`kind` はこのフォームでは変えない（種類を変えるのは作り直しに近いため）。
 */
export function repoPatchBodyFrom(values: RepoFormValues): RepoPatchBody {
  const body: RepoPatchBody = {
    name: values.name,
    location: repoLocationFrom(values),
    default_branch: values.defaultBranch,
  };
  if (values.run) body.run = values.run as RepoRun;
  return body;
}

/** 案件詳細の行の編集フォーム（`repo_*`）から `RepoPatchBody` を組む。 */
export function readRepoPatchBody(form: FormData, prefix = "repo"): RepoPatchBody {
  return repoPatchBodyFrom(readRepoFormValues(form, prefix));
}

/**
 * `/projects` の新規フォームの「追加のリポジトリ」（繰り返し行、`extra_repo_*`）を読む。
 * `POST /projects` は 1 つの `workspace`（＝ primary になるリポジトリ）しか受けないので、
 * 案件を作ったあとに 1 行ずつ `POST /projects/{id}/repos` する（§3.69）。
 * 入力欄は行ごとに必ず全部が submit されるので、`getAll()` の並びは行の並びと一致する。
 * **パスが空の行は送らない**（行を足しただけで書かなかったもの。空の `location` で 422 を出さないため）。
 */
export function readExtraRepoCreateBodies(form: FormData, prefix = "extra_repo"): RepoCreateBody[] {
  const column = (field: string): string[] =>
    form.getAll(`${prefix}_${field}`).map((v) => (typeof v === "string" ? v : ""));
  const paths = column("path");
  const names = column("name");
  const kinds = column("kind");
  const places = column("place");
  const clusters = column("cluster");
  const branches = column("default_branch");
  const runs = column("run");
  const bodies: RepoCreateBody[] = [];
  for (const [i, path] of paths.entries()) {
    if (path.trim() === "") continue;
    bodies.push(
      repoCreateBodyFrom({
        name: names[i] || null,
        kind: kinds[i] || null,
        place: places[i] || null,
        path,
        cluster: clusters[i] || null,
        defaultBranch: branches[i] || null,
        run: runs[i] || null,
      }),
    );
  }
  return bodies;
}
