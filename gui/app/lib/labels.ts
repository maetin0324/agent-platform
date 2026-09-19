import type { Decision, InstanceRole, MilestoneStatus, OrgKind, ProjectStatus, Status } from "~/taskd/types";

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
