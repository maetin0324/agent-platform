import type { Tone } from "~/components/ui/tone";
import type { ChangedFile, DiffStat, IntegrationState } from "~/taskd/types";

/**
 * 変更の取り込み（ADR-0043 D5、taskd Phase 54 / G18）で画面が使う純粋関数。
 * DOM を描画する unit テストがこのリポジトリに無い（G10-U1）ので、**表示の判断と組み立てはここに集めて**
 * `test/unit/task-changes.test.ts` で検証する（`~/lib/task-files.ts` と同じ考え方）。
 *
 * 判断そのものは taskd 側にある: どのファイルが変わったか（`status`）、何コミット進んでいるか（`ahead`）、
 * PR を作れるか（`origin` / `gh`）、`main` が編集中か（409 `default_branch_busy`）は全部 taskd が決める。
 * ここでやるのは「返ってきた値を読みやすく並べる」ことだけで、再計算も再判定もしない。
 */

/** unified diff の 1 行の種類。色を付けるためだけの分類（`dangerouslySetInnerHTML` は使わない）。 */
export type DiffLineKind = "meta" | "hunk" | "add" | "del" | "ctx";

export interface DiffLine {
  kind: DiffLineKind;
  /** 行の中身（改行は含まない。先頭の `+` / `-` もそのまま残す）。 */
  text: string;
}

/** `diff --git` などの見出し行（`+++` / `---` は追加・削除ではなくファイル名なので meta）。 */
const META_PREFIXES = [
  "diff --git",
  "index ",
  "--- ",
  "+++ ",
  "new file mode",
  "deleted file mode",
  "old mode",
  "new mode",
  "similarity index",
  "dissimilarity index",
  "rename from",
  "rename to",
  "copy from",
  "copy to",
  "Binary files",
  "GIT binary patch",
  "\\ No newline at end of file",
];

/**
 * `ChangeDiffView.diff`（unified diff、200 KiB で切られていることがある）を 1 行ずつ分類する。
 * 空文字（差分なし）は空配列。末尾の改行で余分な空行を作らない。
 */
export function parseDiff(text: string): DiffLine[] {
  if (text === "") return [];
  const body = text.endsWith("\n") ? text.slice(0, -1) : text;
  return body.split("\n").map((line) => ({ kind: diffLineKind(line), text: line }));
}

function diffLineKind(line: string): DiffLineKind {
  if (line.startsWith("@@")) return "hunk";
  for (const prefix of META_PREFIXES) {
    if (line.startsWith(prefix)) return "meta";
  }
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "del";
  return "ctx";
}

/** 追加・削除・文脈それぞれの色（`~/components/ui/tone.ts` の役割に寄せる。色だけに頼らず記号も残す）。 */
export const DIFF_LINE_CLASS: Record<DiffLineKind, string> = {
  meta: "text-fg-subtle",
  hunk: "bg-info-soft text-info-soft-fg",
  add: "bg-success-soft text-success-soft-fg",
  del: "bg-danger-soft text-danger-soft-fg",
  ctx: "text-fg-muted",
};

/** sha を短く出す（既定 12 桁。`~/lib/releases.ts` の sha12 と同じ長さ）。空なら `-`。 */
export function shortSha(sha: string | null | undefined, length = 12): string {
  if (!sha) return "-";
  return sha.length <= length ? sha : sha.slice(0, length);
}

/** `DiffStat` の 1 行（`3 ファイル +12 −4`）。 */
export function statChip(stat: DiffStat): string {
  return `${stat.files} ファイル +${stat.additions} −${stat.deletions}`;
}

/** 1 ファイルの増減（バイナリは git が行数を出さないので行数を出さない）。 */
export function fileDeltaChip(file: ChangedFile): string {
  return file.binary ? "バイナリ" : `+${file.additions} −${file.deletions}`;
}

/**
 * ファイルの `status`（`A` / `M` / `D` / `?` / `T`）の色。知らない文字は中立
 * （taskd が文字を増やしても壊れない。言葉は `~/lib/labels.ts`）。
 */
const CHANGED_FILE_STATUS_TONE: Record<string, Tone> = {
  A: "success",
  M: "primary",
  D: "danger",
  "?": "neutral",
  T: "warning",
};

export function changedFileStatusTone(status: string): Tone {
  return CHANGED_FILE_STATUS_TONE[status] ?? "neutral";
}

/** 取り込みの行方（`TaskIntegration.state`）の色。知らない値は中立。 */
const INTEGRATION_STATE_TONE: Record<IntegrationState, Tone> = {
  done: "success",
  open: "info",
  merged: "success",
  closed: "neutral",
  conflict: "warning",
  failed: "danger",
};

export function integrationStateTone(state: IntegrationState | string): Tone {
  return INTEGRATION_STATE_TONE[state as IntegrationState] ?? "neutral";
}

/** `/tasks/:id/changes` のリンク（ファイルを選ぶと差分が出る）。空の値はクエリに出さない。 */
export function taskChangesHref(taskId: string, params: { repo?: string | null; file?: string | null } = {}): string {
  const query = new URLSearchParams();
  if (params.repo) query.set("repo", params.repo);
  if (params.file) query.set("file", params.file);
  const suffix = query.toString();
  return suffix === "" ? `/tasks/${taskId}/changes` : `/tasks/${taskId}/changes?${suffix}`;
}
