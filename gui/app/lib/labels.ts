import type { BoardColumnId } from "~/lib/board";
import type {
  CommentAuthorKind,
  CommentEffect,
  Decision,
  InstanceRole,
  MilestoneStatus,
  OrgKind,
  ProjectStatus,
  RepoKind,
  RepoRun,
  RepoSync,
  Status,
  TaskCategory,
  Tier,
} from "~/taskd/types";

/**
 * 業務の 6 画面（SPEC §4: 秘書・組織・案件・報告・認可・成果物）で使う日本語の言葉（Phase G13f-1、監査 5）。
 * 画面には API のフィールド名（`request` / `status` / `node_id` …）や英語の状態値を出さず、ここの言葉だけを出す。
 * 裏方の画面（`/tasks` 等）は taskd の値をそのまま出す従来どおりの扱いなので、ここは使わなくてよい。
 *
 * 呼び方の統一（監査 5）: 組織のノード = **担当**（「ノード」「人」とは呼ばない。help の説明文だけ
 * 「人（担当）」と一度言い換える）。案件は「案件」のまま。
 */

/** 組織のノードの呼び方（画面に出す唯一の言い方）。 */
export const ASSIGNEE_WORD = "担当";

/** 案件の状態（ADR-0033 D2）。 */
const PROJECT_STATUS_LABEL: Record<ProjectStatus, string> = {
  proposed: "提案中",
  active: "進行中",
  paused: "一時停止",
  done: "完了",
};

export function projectStatusLabel(status: ProjectStatus | string): string {
  return PROJECT_STATUS_LABEL[status as ProjectStatus] ?? status;
}

/** 途中目標の状態（SPEC §7 のアジャイル）。 */
const MILESTONE_STATUS_LABEL: Record<MilestoneStatus, string> = {
  proposed: "提案",
  approved: "承認済み",
  in_progress: "進行中",
  reached: "達成",
  redesigned: "再設計",
};

export function milestoneStatusLabel(status: MilestoneStatus | string): string {
  return MILESTONE_STATUS_LABEL[status as MilestoneStatus] ?? status;
}

/**
 * タスクの状態（`Status`）の日本語。裏方の言葉（draft / ready …）をそのまま業務の画面に出さないため
 * （taskd の値は変えない。表示だけ）。
 */
const TASK_STATUS_LABEL: Record<Status, string> = {
  draft: "下書き",
  ready: "待機中",
  running: "作業中",
  blocked: "質問待ち",
  reviewing: "確認中",
  done: "完了",
  failed: "失敗",
  cancelled: "取り消し",
};

export function taskStatusLabel(status: Status | string): string {
  return TASK_STATUS_LABEL[status as Status] ?? status;
}

/**
 * 組織の木の形で分かるので英語のバッジ（section / department / secretary）は出さない（監査 4）。
 * どうしても添えるときの 1 文字だけをここに持つ（秘書は木の根なので印を出さない）。
 */
const ORG_KIND_MARK: Record<OrgKind, string> = { secretary: "", department: "部", section: "課" };

export function orgKindMark(kind: OrgKind | string): string {
  return ORG_KIND_MARK[kind as OrgKind] ?? "";
}

/** 認可の決定（SPEC §3.6）。 */
const DECISION_LABEL: Record<Decision, string> = {
  once: "今回だけ",
  standing: "今後ずっと",
  denied: "認めない",
};

export function decisionLabel(decision: Decision | string): string {
  return DECISION_LABEL[decision as Decision] ?? decision;
}

/**
 * taskd のインスタンスの役割（ADR-0040 D4）。「リリース」画面（`/releases`、Phase G14）は裏方だが、
 * 引き継ぎの進行は人が読むところなので日本語にする（`active` / `standby` / … のままは出さない）。
 */
const INSTANCE_ROLE_LABEL: Record<InstanceRole, string> = {
  active: "稼働中",
  standby: "待機（切り替え中）",
  draining: "引き継ぎ中（残りの仕事を完了待ち）",
  verify: "検証",
};

export function instanceRoleLabel(role: InstanceRole | string): string {
  return INSTANCE_ROLE_LABEL[role as InstanceRole] ?? role;
}

/**
 * 昇格の前に見せる差分（ADR-0041 D4。Phase G15）。**どのパスが「安全に関わる」かは
 * taskd 側（`scripts/selfdeploy/lib.sh` の `SD_SENSITIVE_PATTERNS`）が決める**ので、
 * ここにあるのは言葉だけ。
 */
export function sensitiveChangesLabel(count: number): string {
  return `安全に関わる変更 ${count} 件`;
}

/** `changes.base` がいまの `current` と違うときの断り書き。 */
export function staleChangesLabel(base: string | null): string {
  return base
    ? `この差分は ${base} を起点に作られたもので、いまの現行とは違います（もう一度 release.sh を通すと新しくなります）`
    : "この差分は現行が無いときに作られたもので、いまの現行との差ではありません";
}

/**
 * 案件のリポジトリ（ADR-0043 D1、docs/taskd-api-v1.md §3.68〜3.71。Phase 52 / G16）。
 * 値（`git` / `dir` / `auto` / `host` / `container` / `worktree` / `rsync` / `none`）は taskd のものを
 * そのまま送り返すだけで、画面に出す言葉だけをここに集める。知らない値は素のまま出す
 * （taskd が値を増やしても壊れない）。
 */
const REPO_KIND_LABEL: Record<RepoKind, string> = {
  git: "git",
  dir: "ディレクトリ",
};

export function repoKindLabel(kind: RepoKind | string): string {
  return REPO_KIND_LABEL[kind as RepoKind] ?? kind;
}

/** 実行環境（ADR-0043 D1 / D3）。`container` はこの Phase では読むだけ（ADR-0043 A3）。 */
const REPO_RUN_LABEL: Record<RepoRun, string> = {
  auto: "自動",
  host: "ホスト",
  container: "コンテナ",
};

export function repoRunLabel(run: RepoRun | string): string {
  return REPO_RUN_LABEL[run as RepoRun] ?? run;
}

/** リモートのリポジトリの持ち込み方（ADR-0043 D7。`none` は taskd が 422 にする）。 */
const REPO_SYNC_LABEL: Record<RepoSync, string> = {
  worktree: "worktree",
  rsync: "rsync",
  none: "同期しない",
};

export function repoSyncLabel(sync: RepoSync | string): string {
  return REPO_SYNC_LABEL[sync as RepoSync] ?? sync;
}

/** 案件の「主なリポジトリ」（`is_primary`）に添える印。`Project.workspace` はこの行の写し。 */
export const PRIMARY_REPO_MARK = "主";

/** 「主にする」ボタンの文言（1 案件に 1 つ。primary を空にはできない）。 */
export const SET_PRIMARY_REPO_LABEL = "主にする";

/** リポジトリの `kind` を「自動で決める」（`kind` を送らない）ときの選択肢の文言。 */
export const REPO_KIND_AUTO_LABEL = "自動（.git があれば git）";

/**
 * タスクの作業ツリー（ADR-0043 D6、docs/taskd-api-v1.md §3.72）の一覧の種類。
 * `kind` は taskd が決めた `dir` / `file` / `other` をそのまま受ける（GUI で再判定しない）。
 */
const TREE_ENTRY_KIND_LABEL: Record<string, string> = {
  dir: "ディレクトリ",
  file: "ファイル",
  other: "その他",
};

export function treeEntryKindLabel(kind: string): string {
  return TREE_ENTRY_KIND_LABEL[kind] ?? kind;
}

/** バイト数の表示（`size` は taskd が返した値そのまま。1024 進で小数 1 桁まで）。 */
export function fileSizeLabel(size: number): string {
  const units = ["B", "KiB", "MiB", "GiB"];
  let value = size;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? String(value) : value.toFixed(1)} ${units[unit]}`;
}

/** テキストとして読めなかったファイル（`binary: true`）。本文は出さず大きさだけ出す（§3.73）。 */
export function binaryFileLabel(size: number): string {
  return `バイナリのため表示しません（${fileSizeLabel(size)}）`;
}

/** 512 KiB を超えたファイル（`too_large: true`）。本文は出さず大きさだけ出す（§3.73）。 */
export function tooLargeFileLabel(size: number): string {
  return `512 KiB を超えるため表示しません（${fileSizeLabel(size)}）`;
}

/**
 * ADR-0044（Phase 53）のタスク管理で足した言葉。ボードの列・種類・優先度・コメント・タイムラインは
 * 人が読むところなので、英語の値（`feature` / `interrupted` / `delegation` …）をそのまま出さない。
 */

/** ボードの列（ADR-0044 D4）。列の並び・状態の束ね方は `~/lib/board.ts` が持つ。 */
const BOARD_COLUMN_LABEL: Record<BoardColumnId, string> = {
  waiting: "待ち",
  in_progress: "進行中",
  blocked: "止まっている",
  done: "完了",
  failed: "失敗",
  cancelled: "中止",
};

export function boardColumnLabel(column: BoardColumnId | string): string {
  return BOARD_COLUMN_LABEL[column as BoardColumnId] ?? column;
}

/** タスクの種類（ADR-0044 D3）。既定は `other`。 */
const TASK_CATEGORY_LABEL: Record<TaskCategory, string> = {
  feature: "機能",
  bug: "不具合",
  research: "調査",
  ops: "運用",
  docs: "文書",
  other: "その他",
};

export const TASK_CATEGORIES: readonly TaskCategory[] = ["feature", "bug", "research", "ops", "docs", "other"];

export function taskCategoryLabel(category: TaskCategory | string): string {
  return TASK_CATEGORY_LABEL[category as TaskCategory] ?? category;
}

/**
 * 優先度（ADR-0044 D3）。P0 が最優先。記号だけでは分からないので短い言葉を添える
 * （画面には `P1（高い）` のように出す）。
 */
const PRIORITY_LABEL_TEXT: Record<string, string> = {
  P0: "今すぐ",
  P1: "高い",
  P2: "ふつう",
  P3: "低い",
};

export function priorityText(label: string): string {
  return PRIORITY_LABEL_TEXT[label] ?? label;
}

/** `P1（高い）` の形。プルダウンの選択肢とカードのバッジで使う。 */
export function priorityFullLabel(label: string): string {
  const text = PRIORITY_LABEL_TEXT[label];
  return text ? `${label}（${text}）` : label;
}

/** 担当エージェントのレベル（ADR-0033 D2 の tier。ADR-0044 D1 でタスクから指定できるようになった）。 */
const TIER_LABEL: Record<Tier, string> = {
  frontier: "最上位（frontier）",
  standard: "標準（standard）",
  cheap: "軽い（cheap）",
};

export const TIERS: readonly Tier[] = ["frontier", "standard", "cheap"];

export function tierLabel(tier: Tier | string): string {
  return TIER_LABEL[tier as Tier] ?? tier;
}

/** コメントを誰が書いたか（ADR-0044 D2）。 */
const COMMENT_AUTHOR_LABEL: Record<CommentAuthorKind, string> = {
  human: "あなた",
  node: ASSIGNEE_WORD,
  system: "taskd",
};

export function commentAuthorLabel(kind: CommentAuthorKind | string): string {
  return COMMENT_AUTHOR_LABEL[kind as CommentAuthorKind] ?? kind;
}

/**
 * 人のコメントが何を起こしたか（ADR-0044 D2 の表）。コメントを送った直後に、
 * 「記録しただけ」なのか「走っていた run を止めた」のかを必ず言う。
 */
const COMMENT_EFFECT_MESSAGE: Record<CommentEffect, string> = {
  stored: "コメントを記録しました。次の run の前置きに載ります。",
  interrupted: "走っていた run を止めて ready に戻しました。次の run はこのコメントから始まります。",
  answered: "質問への回答として渡しました。",
  terminal: "終わったタスクなので、コメントを記録しただけです。",
};

export function commentEffectMessage(effect: CommentEffect | string): string {
  return COMMENT_EFFECT_MESSAGE[effect as CommentEffect] ?? String(effect);
}

/** タイムラインの 1 件の種類（ADR-0044 D5）。 */
const TIMELINE_KIND_LABEL: Record<string, string> = {
  event: "できごと",
  comment: "コメント",
  approval: "認可",
  report: "報告",
  delegation: "委譲",
  release: "リリース",
  integration: "取り込み",
};

export function timelineKindLabel(kind: string): string {
  return TIMELINE_KIND_LABEL[kind] ?? kind;
}

/** 編集で実際に変わった項目（`EditResult.fields`）を日本語にする（ADR-0044 D1）。 */
const TASK_FIELD_LABEL: Record<string, string> = {
  title: "題名",
  objective: "目的",
  acceptance: "受け入れ条件",
  priority: "優先度",
  labels: "ラベル",
  category: "種類",
  assignee: ASSIGNEE_WORD,
  role: "役割",
  tier: "レベル",
  adapter: "アダプタ",
  milestone_id: "途中目標",
  depends_on: "依存",
  max_turns: "max_turns",
  max_wall_secs: "max_wall_secs",
  max_retries: "max_retries",
};

export function taskFieldLabel(field: string): string {
  return TASK_FIELD_LABEL[field] ?? field;
}

/** タスク画面のタブ（ADR-0044 D5）。URL の `?tab=` の値 → 見出し。 */
export const TASK_TABS = ["overview", "timeline", "changes", "files", "artifacts"] as const;
export type TaskTab = (typeof TASK_TABS)[number];

const TASK_TAB_LABEL: Record<TaskTab, string> = {
  overview: "概要",
  timeline: "タイムライン",
  changes: "変更",
  files: "ファイル",
  artifacts: "成果物",
};

export function taskTabLabel(tab: TaskTab | string): string {
  return TASK_TAB_LABEL[tab as TaskTab] ?? tab;
}

/** `?tab=` を読む。知らない値・未指定は概要（ADR-0044 D5 の既定）。 */
export function parseTaskTab(value: string | null | undefined): TaskTab {
  return value && (TASK_TABS as readonly string[]).includes(value) ? (value as TaskTab) : "overview";
}
