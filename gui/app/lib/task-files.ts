import type { TreeFileView } from "~/celeris/types";
import { binaryFileLabel, tooLargeFileLabel } from "~/lib/labels";

/**
 * タスクの作業ツリーの閲覧（ADR-0043 D6、docs/celeris-api-v1.md §3.72〜3.73。Phase 52 / G16）で
 * 画面が使う純粋関数。DOM を描画する unit テストがこのリポジトリに無い（G10-U1）ので、
 * **表示の判断と文言はここに集めて** `test/unit/task-files.test.ts` で検証する
 * （`~/lib/artifact-view.ts` の `artifactStatusMessage` と同じ考え方）。
 *
 * 境界（`..` や作業ツリーの外に出るパス）を弾くのは celeris（403 `path_forbidden`）で、ここでは判定しない。
 * 一覧の並び（ディレクトリが先、あとは名前順）も celeris が決めたものをそのまま使う（§3.72）。
 */

/** パンくずの 1 つ（`path` はそのリポジトリの作業ツリーからの相対パス）。 */
export interface TreeCrumb {
  name: string;
  path: string;
}

/**
 * `TreeView.path` をパンくずに割る。根（`""`）は空配列（呼び出し側がリポジトリ名で根のリンクを出す）。
 * 空の区切り（`a//b`）や前後の `/` は落とす。
 */
export function treeBreadcrumbs(path: string): TreeCrumb[] {
  const crumbs: TreeCrumb[] = [];
  let acc = "";
  for (const segment of path.split("/")) {
    if (segment === "") continue;
    acc = acc === "" ? segment : `${acc}/${segment}`;
    crumbs.push({ name: segment, path: acc });
  }
  return crumbs;
}

/** 1 つ上のディレクトリ。根（`""`）には親が無いので `null`。 */
export function parentPath(path: string): string | null {
  const crumbs = treeBreadcrumbs(path);
  if (crumbs.length === 0) return null;
  return crumbs.length === 1 ? "" : (crumbs[crumbs.length - 2]?.path ?? "");
}

/** 本文を出すか、出さずに大きさだけ出すか（§3.73）。 */
export type FileBodyKind = "text" | "binary" | "too_large";

export interface FileBody {
  kind: FileBodyKind;
  /** 本文（`kind === "text"` のときだけ。celeris が `text` を返さなかったら空文字）。 */
  text: string | null;
  /** 本文を出さない理由（`kind === "text"` なら `null`）。 */
  message: string | null;
}

/**
 * `TreeFileView` から表示の形を決める。`binary` / `too_large` はどちらも **`text` が返らない**ので、
 * 本文の代わりに大きさだけを出す（GUI では中身を推測しない）。
 */
export function fileBody(file: TreeFileView): FileBody {
  if (file.binary) return { kind: "binary", text: null, message: binaryFileLabel(file.size) };
  if (file.too_large) return { kind: "too_large", text: null, message: tooLargeFileLabel(file.size) };
  return { kind: "text", text: file.text ?? "", message: null };
}

/** 拡張子でビューアを選ぶ（`~/lib/artifact-view.ts` と同じ「名前だけで決める」方針）。 */
export function pickTreeFileViewer(path: string): "markdown" | "code" {
  return /\.(md|markdown)$/i.test(path) ? "markdown" : "code";
}

/** `CodeViewer` に JSON の構文強調を使わせるか。 */
export function isJsonPath(path: string): boolean {
  return /\.json$/i.test(path);
}

/** `/tasks/:id/files` のリンク（ディレクトリを開く・ファイルを選ぶ）。空の値はクエリに出さない。 */
export function taskFilesHref(
  taskId: string,
  params: { repo?: string | null; path?: string | null; file?: string | null },
): string {
  const query = new URLSearchParams();
  if (params.repo) query.set("repo", params.repo);
  if (params.path) query.set("path", params.path);
  if (params.file) query.set("file", params.file);
  const suffix = query.toString();
  return suffix === "" ? `/tasks/${taskId}/files` : `/tasks/${taskId}/files?${suffix}`;
}
