import type { MilestoneStatus, ProjectStatus } from "~/celeris/types";

/**
 * 案件・途中目標の中止・一時停止・アーカイブ（ADR-0044 D6、docs/celeris-api-v1.md §3.84〜3.91。Phase 55 / G19）。
 *
 * **どの操作ができるかを決めるのは celeris**（できない操作は 409 `invalid_transition`）。ここにあるのは
 * 「押せないボタンを最初から出さない」ための表示の判定だけで、押されたら常に celeris に送り、409 の文言を
 * そのまま出す（GUI 側で状態機械を作り直さない。CLAUDE.md の「仕様外の挙動に頼らない」）。
 * 判定の根拠は docs/celeris-api-v1.md §3.84〜3.91 の表と「いまの状態でできない操作」の 4 行:
 * 中止済みの `cancel` / 終端・一時停止中の `pause` / `paused` でないものの `resume` / 非終端の案件の `archive`。
 */

/** 案件の終端（`archive` できるのはここに入ったものだけ。§3.84〜3.91 の表）。 */
export const TERMINAL_PROJECT_STATUSES: readonly ProjectStatus[] = ["done", "cancelled"];

/**
 * 途中目標の終端。§3.84〜3.91 が「非終端の途中目標（`proposed`/`approved`/`in_progress`/`paused`）」と
 * 書いているので、その補集合（`reached` / `redesigned` / `cancelled`）。
 */
export const TERMINAL_MILESTONE_STATUSES: readonly MilestoneStatus[] = ["reached", "redesigned", "cancelled"];

/** 案件が終端か（`done` / `cancelled`）。 */
export function projectIsTerminal(status: ProjectStatus | string): boolean {
  return (TERMINAL_PROJECT_STATUSES as readonly string[]).includes(status);
}

/** 途中目標が終端か（`reached` / `redesigned` / `cancelled`）。 */
export function milestoneIsTerminal(status: MilestoneStatus | string): boolean {
  return (TERMINAL_MILESTONE_STATUSES as readonly string[]).includes(status);
}

/** アーカイブ済みか（`archived_at` が入っていればアーカイブ済み。`GET /projects` は既定で隠す）。 */
export function projectIsArchived(project: { archived_at?: string | null }): boolean {
  return typeof project.archived_at === "string" && project.archived_at !== "";
}

/** 案件のヘッダに出すボタン（ADR-0044 D6 の「案件画面のヘッダに…」）。 */
export interface ProjectLifecycleButtons {
  /** 「一時停止」: 終端でもなく、まだ止まっていないとき。 */
  pause: boolean;
  /** 「再開」: `paused` のときだけ。 */
  resume: boolean;
  /** 「中止（確認付き）」: 中止済み以外。`done` の案件も celeris は受ける。 */
  cancel: boolean;
  /** 「アーカイブ（確認付き）」: まだアーカイブされていないとき（押せるかは `archiveEnabled`）。 */
  archive: boolean;
  /** 「アーカイブ解除」: アーカイブ済みのときだけ。 */
  unarchive: boolean;
  /**
   * 「アーカイブ」を押せるか。**終端（`done` / `cancelled`）の案件だけ**（非終端は 409 `invalid_transition`）。
   * 押せないときはボタンを消さずに `disabled` で出し、`ARCHIVE_ONLY_TERMINAL_HINT` を添える。
   */
  archiveEnabled: boolean;
}

export function projectLifecycleButtons(project: {
  status: ProjectStatus | string;
  archived_at?: string | null;
}): ProjectLifecycleButtons {
  const terminal = projectIsTerminal(project.status);
  const archived = projectIsArchived(project);
  return {
    pause: !terminal && project.status !== "paused",
    resume: project.status === "paused",
    cancel: project.status !== "cancelled",
    archive: !archived,
    unarchive: archived,
    archiveEnabled: terminal,
  };
}

/** 途中目標のカードに出すボタン（ADR-0044 D6 の「途中目標カードに「一時停止／中止」」）。 */
export interface MilestoneLifecycleButtons {
  /** 「一時停止」: 終端でもなく、まだ止まっていないとき。 */
  pause: boolean;
  /** 「再開」: `paused` のときだけ。 */
  resume: boolean;
  /**
   * 「中止（確認付き）」: **終端でないときだけ**。案件と違い、途中目標は達成（`reached`）・
   * 再設計（`redesigned`）・中止済みのどれも celeris が 409 で断る
   * （docs/celeris-api-v1.md §3.84〜3.91 の「終端は…途中目標が `reached` / `redesigned` / `cancelled`」）。
   */
  cancel: boolean;
}

export function milestoneLifecycleButtons(milestone: { status: MilestoneStatus | string }): MilestoneLifecycleButtons {
  const terminal = milestoneIsTerminal(milestone.status);
  return {
    pause: !terminal && milestone.status !== "paused",
    resume: milestone.status === "paused",
    cancel: !terminal,
  };
}

/** 案件が一時停止中か（バナーを出すかどうか）。 */
export function projectIsPaused(project: { status: ProjectStatus | string }): boolean {
  return project.status === "paused";
}

/** 途中目標が一時停止中か。 */
export function milestoneIsPaused(milestone: { status: MilestoneStatus | string }): boolean {
  return milestone.status === "paused";
}

/** `GET /projects` / `GET /tasks` の `archived` を URL の検索パラメータから読む（`?archived=1` だけを真とする）。 */
export function readArchivedParam(params: URLSearchParams): boolean {
  return params.get("archived") === "1";
}

/**
 * `GET /projects` / `GET /tasks` に渡す `archived`。隠すのが既定なので、**表示するときだけ** `1` を送る
 * （送らなければ celeris の既定＝隠す）。
 */
export function archivedQuery(showArchived: boolean): "1" | undefined {
  return showArchived ? "1" : undefined;
}
