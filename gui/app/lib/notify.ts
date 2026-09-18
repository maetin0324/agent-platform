import type { Tone } from "~/components/ui/tone";
import type { NotificationKind, NotifyRecent } from "~/taskd/types";

/**
 * Discord への通知（ADR-0037、Phase 39、docs/taskd-api-v1.md §3.64〜3.65）の純粋関数。
 * `/reports`（報告の流れ）の画面から使う。DOM を描画する unit テストが無い（G10-U1）ため、
 * 判断・計算はここに集めて純粋関数としてテストする（`~/lib/reports.ts` と同じ方針）。
 */

/** taskd の `error` 文言（`crates/taskd/src/notify.rs` の `NOT_CONFIGURED` 定数）。GUI 側の文言に畳む。 */
export const NOTIFY_NOT_CONFIGURED_ERROR = "discord webhook is not configured";

/** 5 種の知らせ（ADR-0037 D1）を SPEC の言葉で。 */
export const NOTIFY_KIND_LABEL: Record<NotificationKind, string> = {
  milestone_ready: "途中目標の仕事が終わった",
  approval_pending: "認可の要求が来た",
  question_blocked: "質問で止まっている",
  bad_news: "悪い知らせが届いた",
  secretary_reply: "秘書から方針の提案が届いた",
};

export function notifyKindLabel(kind: NotificationKind): string {
  return NOTIFY_KIND_LABEL[kind] ?? kind;
}

/**
 * 直近の送信 1 件の結果（Phase 39 の判断 2: 秘密が未設定の間の出来事は `ok=false` /
 * `error="discord webhook is not configured"` として台帳に残る。GUI は専用の文言で「未設定のため
 * 送っていません」と見せる）。
 */
export function notifyResultLabel(recent: Pick<NotifyRecent, "ok" | "error">): string {
  if (recent.ok === true) return "送れた";
  if (recent.ok === false) {
    if (recent.error === NOTIFY_NOT_CONFIGURED_ERROR) return "未設定のため送っていません";
    return recent.error ? `失敗（${recent.error}）` : "失敗";
  }
  return "送信待ち（次の tick で再送）";
}

export function notifyResultTone(recent: Pick<NotifyRecent, "ok">): Tone {
  if (recent.ok === true) return "success";
  if (recent.ok === false) return "danger";
  return "neutral";
}

/**
 * 対象への画面内リンク（`key` = 途中目標 / 認可 / タスク / 報告 / 案件 id、ADR-0037 D1）。
 * `milestone_ready` の `key` は途中目標 id で、taskd の API には途中目標単体を引く経路
 * （`GET /milestones/{id}` 相当）が無く、どの案件の途中目標かを画面側だけでは解決できないため、
 * リンクは作らず `null` を返す（未解決事項として `docs/PROGRESS.md` の提案に書く）。
 */
export function notifyTargetHref(recent: Pick<NotifyRecent, "kind" | "key">): string | null {
  switch (recent.kind) {
    case "approval_pending":
      return `/approvals#approval-${encodeURIComponent(recent.key)}`;
    case "question_blocked":
      return `/tasks/${encodeURIComponent(recent.key)}`;
    case "bad_news":
      return `/reports#report-${encodeURIComponent(recent.key)}`;
    case "secretary_reply":
      return `/projects/${encodeURIComponent(recent.key)}`;
    case "milestone_ready":
      return null;
    default:
      return null;
  }
}
