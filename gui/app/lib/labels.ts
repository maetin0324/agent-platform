import type {
  CommentAuthorKind,
  CommentEffect,
  Decision,
  InstanceRole,
  IntegrationMethod,
  IntegrationState,
  MilestoneStatus,
  MountKind,
  OrgKind,
  ProfileRun,
  ProjectStatus,
  RepoKind,
  RepoRun,
  RepoSync,
  Status,
  TaskCategory,
  TaskMode,
  Tier,
} from "~/celeris/types";
import type { BoardColumnId } from "~/lib/board";

/**
 * 業務の 6 画面（SPEC §4: 秘書・組織・案件・報告・認可・成果物）で使う日本語の言葉（Phase G13f-1、監査 5）。
 * 画面には API のフィールド名（`request` / `status` / `node_id` …）や英語の状態値を出さず、ここの言葉だけを出す。
 * 裏方の画面（`/tasks` 等）は celeris の値をそのまま出す従来どおりの扱いなので、ここは使わなくてよい。
 *
 * 呼び方の統一（監査 5）: 組織のノード = **担当**（「ノード」「人」とは呼ばない。help の説明文だけ
 * 「人（担当）」と一度言い換える）。案件は「案件」のまま。
 */

/** 組織のノードの呼び方（画面に出す唯一の言い方）。 */
export const ASSIGNEE_WORD = "担当";

/** 案件の状態（ADR-0033 D2、ADR-0044 D6 で `cancelled` が増えた）。 */
const PROJECT_STATUS_LABEL: Record<ProjectStatus, string> = {
  proposed: "提案中",
  active: "進行中",
  paused: "一時停止",
  done: "完了",
  cancelled: "中止",
};

export function projectStatusLabel(status: ProjectStatus | string): string {
  return PROJECT_STATUS_LABEL[status as ProjectStatus] ?? status;
}

/** 途中目標の状態（SPEC §7 のアジャイル。ADR-0044 D6 で `paused` / `cancelled` が増えた）。 */
const MILESTONE_STATUS_LABEL: Record<MilestoneStatus, string> = {
  proposed: "提案",
  approved: "承認済み",
  in_progress: "進行中",
  reached: "達成",
  redesigned: "再設計",
  paused: "一時停止",
  cancelled: "中止",
};

export function milestoneStatusLabel(status: MilestoneStatus | string): string {
  return MILESTONE_STATUS_LABEL[status as MilestoneStatus] ?? status;
}

/**
 * タスクの状態（`Status`）の日本語。裏方の言葉（draft / ready …）をそのまま業務の画面に出さないため
 * （celeris の値は変えない。表示だけ）。
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
 * celeris のインスタンスの役割（ADR-0040 D4）。「リリース」画面（`/releases`、Phase G14）は裏方だが、
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
 * celeris 側（`scripts/selfdeploy/lib.sh` の `SD_SENSITIVE_PATTERNS`）が決める**ので、
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
 * 案件のリポジトリ（ADR-0043 D1、docs/celeris-api-v1.md §3.68〜3.71。Phase 52 / G16）。
 * 値（`git` / `dir` / `auto` / `host` / `container` / `worktree` / `rsync` / `none`）は celeris のものを
 * そのまま送り返すだけで、画面に出す言葉だけをここに集める。知らない値は素のまま出す
 * （celeris が値を増やしても壊れない）。
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

/** リモートのリポジトリの持ち込み方（ADR-0043 D7。`none` は celeris が 422 にする）。 */
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
 * タスクの作業ツリー（ADR-0043 D6、docs/celeris-api-v1.md §3.72）の一覧の種類。
 * `kind` は celeris が決めた `dir` / `file` / `other` をそのまま受ける（GUI で再判定しない）。
 */
const TREE_ENTRY_KIND_LABEL: Record<string, string> = {
  dir: "ディレクトリ",
  file: "ファイル",
  other: "その他",
};

export function treeEntryKindLabel(kind: string): string {
  return TREE_ENTRY_KIND_LABEL[kind] ?? kind;
}

/** バイト数の表示（`size` は celeris が返した値そのまま。1024 進で小数 1 桁まで）。 */
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
  system: "celeris",
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
  doc: "文書",
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
  // ADR-0046 D2 / D3 / D4（Phase 59 / G21）。`EditResult.fields` にも入りうる。
  skills: "能力タグ",
  mode: "進め方",
  harness: "ハーネス",
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

/**
 * 変更の取り込み（ADR-0043 D5、celeris Phase 54 / G18）。`merge` / `pr` / `discard` と
 * `done` / `open` / `merged` / `closed` / `conflict` / `failed` は celeris の値をそのまま送り返すだけで、
 * 画面に出す言葉だけをここに集める。知らない値は素のまま出す（celeris が値を増やしても壊れない）。
 */
const INTEGRATION_METHOD_LABEL: Record<IntegrationMethod, string> = {
  merge: "取り込み",
  pr: "PR",
  discard: "破棄",
};

export function integrationMethodLabel(method: IntegrationMethod | string): string {
  return INTEGRATION_METHOD_LABEL[method as IntegrationMethod] ?? method;
}

const INTEGRATION_STATE_LABEL: Record<IntegrationState, string> = {
  done: "取り込み済み",
  open: "PR 公開中",
  merged: "merge 済み",
  closed: "閉じた",
  conflict: "衝突",
  failed: "失敗",
};

export function integrationStateLabel(state: IntegrationState | string): string {
  return INTEGRATION_STATE_LABEL[state as IntegrationState] ?? state;
}

/**
 * 変わったファイルの `status`（`ChangedFile.status`）。`?` は git の管理外（未追跡）。
 * 文字は celeris が決めたものをそのまま受ける（GUI で再判定しない）。
 */
const CHANGED_FILE_STATUS_LABEL: Record<string, string> = {
  A: "追加",
  M: "変更",
  D: "削除",
  "?": "未追跡",
  T: "種類が変わった",
};

export function changedFileStatusLabel(status: string): string {
  return CHANGED_FILE_STATUS_LABEL[status] ?? status;
}

/** 「main に取り込む」ボタン（取り込む先は celeris が返した `default_branch`。`main` とは限らない）。 */
export function integrateMergeLabel(defaultBranch: string): string {
  return `${defaultBranch} に取り込む`;
}

export const CREATE_PR_LABEL = "PR を作る";
export const DISCARD_CHANGES_LABEL = "捨てる（確認）";
export const DISCARD_CHANGES_CONFIRM_LABEL = "本当に捨てる";
export const MERGE_PR_LABEL = "Celeris で merge";

/** `ahead === 0 && files.length === 0`（調査などコードを伴わないタスク。ADR-0043 D5）。 */
export const NO_CHANGES_LABEL = "変更なし";

/** `missing: true`（worktree もブランチも無い）。 */
export const CHANGES_MISSING_LABEL = "取り込み済み・中止済み（作業ツリーもブランチもありません）";

/** `ChangeDiffView.truncated`（200 KiB で切った）。 */
export const DIFF_TRUNCATED_LABEL = "途中で切りました（200 KiB）";

/**
 * 文書（ADR-0044 D7、celeris Phase 57 / G20。**正本は git**）。ページの中身も履歴も celeris が返すものを
 * そのまま出し、ここには画面の言葉だけを置く。
 */
export const DOCS_TAB_LABEL = "文書";
export const DOCS_SECTION_DESCRIPTION =
  "案件の文書です。正本は主なリポジトリの Markdown（git）で、ここでの編集は既定のブランチに直接コミットされます。";
export const DOCS_INIT_LABEL = "文書を用意する";
export const DOCS_EDIT_LABEL = "編集";
export const DOCS_SAVE_LABEL = "保存";
export const DOCS_CANCEL_LABEL = "やめる";
export const DOCS_NEW_PAGE_LABEL = "ページを作る";
export const DOCS_DELETE_LABEL = "削除";
export const DOCS_DELETE_CONFIRM_LABEL = "本当に削除";
export const DOCS_RELOAD_LABEL = "再読み込み";
export const DOCS_SEARCH_LABEL = "本文を検索";
export const DOCS_HISTORY_LABEL = "履歴";
export const DOCS_EMPTY_LABEL = "まだページがありません";
export const DOCS_TRUNCATED_LABEL = "多すぎるので途中まで出しています（500 ページ）";
export const DOCS_TOO_LARGE_LABEL = "大きすぎるので本文を出していません（512 KiB）";
export const PROMOTE_TO_DOC_LABEL = "文書に昇格";
export const PROMOTE_TO_DOC_SUBMIT_LABEL = "昇格する";
export const PROMOTE_OVERWRITE_LABEL = "既にあるページを上書きする";

/**
 * 文書の変更が弾かれた理由（celeris の `code`）を人の言葉にする。`detail` は別に出すので、
 * ここは「次に何をすればよいか」だけ。知らない `code` は `null`（`detail` だけ出す）。
 */
export function docsErrorHint(code: string): string | null {
  switch (code) {
    case "etag_mismatch":
      return "読み込んだ後に誰かがこのページを直しました。再読み込みしてから、もう一度編集してください。";
    case "default_branch_busy":
      return "既定のブランチが編集中（未コミットの変更がある）です。手元で片付けてから、もう一度保存してください。";
    case "page_exists":
      return "その場所には既にページがあります。別の場所にするか、上書きを選んでください。";
    case "docs_unavailable":
      return "この案件にはまだ文書の置き場がありません。「文書を用意する」を押すと作れます。";
    case "path_forbidden":
      return "文書の根の外は触れません（`..` や絶対パスは使えません）。";
    default:
      return null;
  }
}

/** 「PR を作る」を押せない理由（`origin` が無い・`gh` が使えない）。押せるなら `null`。 */
export function prUnavailableReason(origin: boolean, gh: boolean): string | null {
  if (!origin && !gh) return "origin リモートが無く、gh も使えないので PR を作れません";
  if (!origin) return "origin リモートが無いので PR を作れません";
  if (!gh) return "gh が使えない（PATH に無い・未認証）ので PR を作れません";
  return null;
}

/**
 * 中止・一時停止・アーカイブ（ADR-0044 D6、docs/celeris-api-v1.md §3.84〜3.91。Phase 55 / G19）。
 * どれも**人が押す操作**なので、英語の操作名（`cancel` / `pause` / `archive`）は画面に出さない。
 * 「できるかどうか」は `~/lib/lifecycle.ts`（表示の判定だけ）と celeris（409 `invalid_transition`）が決める。
 */
export const PAUSE_LABEL = "一時停止";
export const RESUME_LABEL = "再開";
export const CANCEL_LABEL = "中止";
/** 中止は取り返しがつかない（属するタスクの worktree とブランチも消える）ので 2 段にする。 */
export const CANCEL_CONFIRM_LABEL = "本当に中止する";
export const ARCHIVE_LABEL = "アーカイブ";
export const ARCHIVE_CONFIRM_LABEL = "アーカイブする";
export const UNARCHIVE_LABEL = "アーカイブ解除";
export const CANCEL_STOP_LABEL = "やめる";

/** 一覧のバッジ（`Project.archived_at` が入っているとき）。 */
export const ARCHIVED_BADGE_LABEL = "アーカイブ済み";

/** 案件一覧の絞り込み（`GET /projects?archived=1`。既定は隠す）。 */
export const SHOW_ARCHIVED_LABEL = "アーカイブを表示";

/** 「アーカイブ」を押せない理由（終端＝完了・中止の案件だけアーカイブできる）。 */
export const ARCHIVE_ONLY_TERMINAL_HINT = "アーカイブできるのは完了・中止の案件だけです";

/** 中止の確認文（案件）。 */
export function projectCancelConfirmText(title: string): string {
  return `案件「${title}」を中止します。まだ終わっていない仕事と途中目標はすべて中止され、作業ツリーとブランチも消えます。取り返しがつきません。`;
}

/** 中止の確認文（途中目標）。 */
export function milestoneCancelConfirmText(title: string): string {
  return `途中目標「${title}」を中止します。この途中目標のまだ終わっていない仕事はすべて中止され、作業ツリーとブランチも消えます。取り返しがつきません。`;
}

/** アーカイブの確認文（アーカイブは冪等なので取り返しはつく。一覧から消えることだけ言う）。 */
export function projectArchiveConfirmText(title: string): string {
  return `案件「${title}」をアーカイブします。一覧とタスクの一覧から既定で消えます（「${SHOW_ARCHIVED_LABEL}」で見えます。いつでも解除できます）。`;
}

/** 一時停止中の案件のバナー（ボード・案件詳細・仕事の木）。 */
export const PROJECT_PAUSED_BANNER =
  "この案件は一時停止中です。新しい仕事は始まりません（走っている仕事は最後まで走ります）。";

/** 一時停止中の途中目標のバナー。 */
export const MILESTONE_PAUSED_BANNER =
  "この途中目標は一時停止中です。新しい仕事は始まりません（走っている仕事は最後まで走ります）。";

/** 中止済みの案件のバナー。 */
export const PROJECT_CANCELLED_BANNER = "この案件は中止されています。新しい仕事は始まりません。";

/** アーカイブ済みの案件のバナー。 */
export const PROJECT_ARCHIVED_BANNER = "この案件はアーカイブされています。一覧とタスクの一覧からは既定で消えています。";

/**
 * 中止で連鎖して止まったものの件数（`ProjectLifecycle.cancelled_tasks` /
 * `cancelled_milestones`、`MilestoneLifecycle.cancelled_tasks`）。**数えるのは celeris が返した配列**で、
 * GUI 側では連鎖を計算し直さない。
 */
export function cancelledCountLabel(tasks: number, milestones?: number): string {
  const parts = [`仕事 ${tasks} 件`];
  if (milestones !== undefined) parts.push(`途中目標 ${milestones} 件`);
  return `${parts.join("・")}を中止しました`;
}

/**
 * ADR-0046（celeris Phase 59 / G21）: 組織 = Agent Profile の継承木と、タスクの harness / skills / mode。
 * 値（`prototype` / `kb` / `host` / …）は celeris のものをそのまま送り返すだけで、画面に出す言葉だけを
 * ここに集める。知らない値は素のまま出す（celeris が語彙を増やしても画面は壊れない）。
 * **継承は計算しない**（実効 profile は `GET /org` の `effective_profiles` をそのまま出す）。
 */

/** タスクの進め方（ADR-0046 D4）。既定は `production`。 */
const TASK_MODE_LABEL: Record<TaskMode, string> = {
  prototype: "試作（小さく試す）",
  production: "本番（きちんと作る）",
  research: "研究（調べる）",
};

export const TASK_MODES: readonly TaskMode[] = ["prototype", "production", "research"];

export function taskModeLabel(mode: TaskMode | string): string {
  return TASK_MODE_LABEL[mode as TaskMode] ?? mode;
}

/** 知識のマウントの種類（ADR-0046 D1 / ADR-0047）。 */
const KNOWLEDGE_KIND_LABEL: Record<MountKind, string> = {
  kb: "知識ベース",
  repo: "リポジトリ",
  dir: "ディレクトリ",
  memory: "記憶",
};

export const KNOWLEDGE_KINDS: readonly MountKind[] = ["kb", "repo", "dir", "memory"];

export function knowledgeKindLabel(kind: MountKind | string): string {
  return KNOWLEDGE_KIND_LABEL[kind as MountKind] ?? kind;
}

/** profile の `run`（どこで動かすか。ADR-0046 D1。子が勝つ）。 */
const PROFILE_RUN_LABEL: Record<ProfileRun, string> = {
  host: "ホスト",
  container: "コンテナ",
};

export const PROFILE_RUNS: readonly ProfileRun[] = ["host", "container"];

export function profileRunLabel(run: ProfileRun | string): string {
  return PROFILE_RUN_LABEL[run as ProfileRun] ?? run;
}

/**
 * profile の項目名（組織の画面のフォームと「実効 profile」の見出し）。API のフィールド名
 * （`harnesses_allowed` / `deny_tools` …）を画面に出さないため。`EffectiveProfile` の項目名に合わせる。
 */
const PROFILE_FIELD_LABEL: Record<string, string> = {
  chain: "どこから継いだか",
  skills: "能力タグ",
  knowledge: "知識",
  harnesses_allowed: "使えるハーネス",
  harness_default: "既定のハーネス",
  tools: "使える道具",
  deny_tools: "禁じる道具",
  run: "動かす場所",
  tier: "モデルの段",
  allowed_tiers: "許すモデルの段",
  policy: "文化（前置きに足す箇条書き）",
  review_harness: "レビューのハーネス",
  review_tier: "レビューのモデルの段",
  approvals: "認可が要る操作",
};

export function profileFieldLabel(field: string): string {
  return PROFILE_FIELD_LABEL[field] ?? field;
}

/**
 * 道具の語彙（ADR-0046 D8、docs/celeris-api-v1.md §3.43）。`cluster:<id>` は設定ごとに違うので
 * 選択肢にはできず、フォームは自由記述の欄を別に置く（正は celeris の 422）。
 */
export const PROFILE_TOOLS: readonly string[] = ["gh", "tavily", "exa", "docker"];

/** `cluster:<id>` のように選択肢に無い道具を書く欄の案内。 */
export const PROFILE_TOOLS_EXTRA_HINT = "選択肢に無い道具（例: cluster:pegasus）を空白かカンマで区切って書きます。";

/**
 * 組み込みのハーネス（docs/celeris-api-v1.md §3.43）。設定の `[[genres]]` の id と合わせたものが
 * `harnesses.allowed` / `harnesses.default` / `review.harness` / タスクの `harness` の選択肢になる。
 */
export const BUILTIN_HARNESSES: readonly string[] = ["conversation", "plan", "reviewer", "smoke"];

/** 設定の `[[genres]]` + 組み込みのハーネス（重複は落とす。並びは設定 → 組み込みの順）。 */
export function harnessOptions(genres: readonly string[]): string[] {
  return [...new Set([...genres, ...BUILTIN_HARNESSES])];
}

/** 「なぜこの担当か」（`Event::Assigned`。ADR-0046 D5）。`score` は能力タグの重なりの数。 */
export const ASSIGNED_WHY_LABEL = "なぜこの担当か";

export function assignedScoreLabel(score: number): string {
  return `能力タグの重なり ${score} 件`;
}

/**
 * 知識ベース（ADR-0047 D5、celeris Phase 61 / G21。**正本は `[knowledge] root` の Markdown**）。
 * ページの中身も履歴も celeris が返すものをそのまま出し、ここには画面の言葉だけを置く。
 */
export const KNOWLEDGE_NAV_LABEL = "知識";
export const KNOWLEDGE_SECTION_DESCRIPTION =
  "組織が覚えていることです。正本は知識ベースの Markdown（git）で、ここでの編集はそのパスだけを 1 件ずつコミットします。";
export const KNOWLEDGE_SEARCH_LABEL = "タグ・題名・本文を検索";
export const KNOWLEDGE_SCOPE_LABEL = "置き場";
export const KNOWLEDGE_SCOPE_ALL_LABEL = "すべて";
export const KNOWLEDGE_EDIT_LABEL = "編集";
export const KNOWLEDGE_SAVE_LABEL = "保存";
export const KNOWLEDGE_CANCEL_LABEL = "やめる";
export const KNOWLEDGE_NEW_PAGE_LABEL = "ページを作る";
export const KNOWLEDGE_RELOAD_LABEL = "再読み込み";
export const KNOWLEDGE_HISTORY_LABEL = "履歴";
export const KNOWLEDGE_EMPTY_LABEL = "まだページがありません";
export const KNOWLEDGE_TRUNCATED_LABEL = "多すぎるので途中まで出しています（500 ページ）";
export const KNOWLEDGE_TOO_LARGE_LABEL = "大きすぎるので本文を出していません（512 KiB）";
export const KNOWLEDGE_SEARCH_RESULT_LABEL = "検索結果（タグ → 題名 → 本文 → 更新の新しい順）";
export const KNOWLEDGE_UNINITIALIZED_TITLE = "知識ベースがまだありません";
export const KNOWLEDGE_UNINITIALIZED_HINT = "`celerisctl knowledge init` で用意してください。";
export const KNOWLEDGE_INBOX_LABEL = "候補";
export const KNOWLEDGE_INBOX_DESCRIPTION =
  "組織が書き留めた知識の候補（`_inbox/`）です。取り込むか捨てるかは人が決めます（索引にも検索にも出ません）。";
export const KNOWLEDGE_INBOX_EMPTY_LABEL = "候補はありません";
export const KNOWLEDGE_ACCEPT_LABEL = "取り込む";
export const KNOWLEDGE_REJECT_LABEL = "捨てる";
export const KNOWLEDGE_REJECT_CONFIRM_LABEL = "本当に捨てる";
export const KNOWLEDGE_TARGET_LABEL = "取り込み先";
export const KNOWLEDGE_OVERWRITE_LABEL = "既にあるページを上書きする";
export const KNOWLEDGE_TARGET_EXISTS_LABEL = "取り込み先には既にページがあります（上書きを選ばないと 409 になります）";

/**
 * 知識ベースの変更が弾かれた理由（celeris の `code`）を人の言葉にする。`detail` は別に出すので、
 * ここは「次に何をすればよいか」だけ。知らない `code` は `null`（`detail` だけ出す）。
 */
export function knowledgeErrorHint(code: string): string | null {
  switch (code) {
    case "etag_mismatch":
      return "読み込んだ後に誰かがこのページを直しました。再読み込みしてから、もう一度編集してください。";
    case "page_exists":
      return "その場所には既にページがあります。別の場所にするか、上書きを選んでください。";
    case "knowledge_unavailable":
      return "知識ベースの置き場がありません。celeris の `[knowledge] root` を設定し、`celerisctl knowledge init` で用意してください。";
    case "path_forbidden":
      return "知識ベースの根の外と `_inbox/` は触れません（`..` や絶対パスは使えません）。";
    case "candidate_not_found":
      return "その候補はもうありません（他の画面で取り込んだか捨てたようです）。再読み込みしてください。";
    default:
      return null;
  }
}
