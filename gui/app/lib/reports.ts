import type { Query } from "~/celeris/client.server";
import type { OrgNode, Project, Report, ReportKind, ReportsLive } from "~/celeris/types";
import { secondsBetween, splitDuration } from "~/lib/time-delta";

/**
 * 「報告の流れ」（SPEC §3.5・§4 の 4、ADR-0033 D3、ADR-0034）の純粋関数。
 * celeris への問い合わせ（loader/action）やコンポーネントから使う。DOM を描画する unit テストは
 * このリポジトリに無い（G10-U1）ため、判断・計算はここに集めて純粋関数としてテストする。
 */

export type ReportsUnreadFilter = "unread" | "all";

/**
 * `GET /reports` のクエリを組む（docs/gui/api.md §3.50）。**`kind` はここに含めない**:
 * celeris の `GET /reports` は `project` / `node` / `level` / `unread` / `limit` しか受け付けず、
 * 「知らないクエリキーは 400」（§3.50）なので、kind の絞り込みは GUI 側で（`filterReportsByKind`）行う。
 * `level` を省略すると「秘書レベルの未読」（既定）にならないため、URL に `level` が無いときは `0` を送る
 * （§3.50「level=0&unread=true が秘書レベルの未読（GUI の『報告の流れ』の既定）」）。
 */
export function buildReportsQuery(searchParams: URLSearchParams): Query {
  const filter: ReportsUnreadFilter = searchParams.get("filter") === "all" ? "all" : "unread";
  const levelParam = searchParams.has("level") ? searchParams.get("level") : "0";
  const project = searchParams.get("project");
  const limit = searchParams.get("limit");

  const query: Query = {};
  if (filter === "unread") query.unread = true;
  if (levelParam !== null && levelParam !== "") {
    const n = Number(levelParam);
    if (!Number.isNaN(n)) query.level = n;
  }
  if (project) query.project = project;
  if (limit) query.limit = limit;
  return query;
}

/** `kind` の絞り込み（GUI 側のみ。上記コメント参照）。空配列は「絞り込まない」。 */
export function filterReportsByKind(items: Report[], kinds: ReportKind[]): Report[] {
  if (kinds.length === 0) return items;
  const set = new Set(kinds);
  return items.filter((r) => set.has(r.kind));
}

/** 案件名の解決（`project_id` → `GET /projects` の `title`）。`null`/空は「案件なし」（ADR-0034 D1）。 */
export function reportProjectName(report: Pick<Report, "project_id">, projects: Project[]): string {
  if (!report.project_id) return "案件なし";
  return projects.find((p) => p.id === report.project_id)?.title ?? report.project_id;
}

/** 担当ノード名の解決（`node_id` → `GET /org` の `name`）。見つからなければ id をそのまま出す。 */
export function reportNodeName(report: Pick<Report, "node_id">, org: OrgNode[]): string {
  return org.find((n) => n.id === report.node_id)?.name ?? report.node_id;
}

/** 7 日（ADR-0055 D2 ラウンド 7）を超えたら「n 前」ではなく絶対日付にする。 */
const RELATIVE_ABSOLUTE_THRESHOLD_SECONDS = 7 * 86400;

/**
 * 7 日を超えた `relativeTimeLabel` のフォールバック絶対日付。UTC の年月日で比較・整形する
 * （celeris が返す ISO は UTC、GUI はタイムゾーン変換をしない。他の絶対時刻表示と同じ方針）。
 * 同じ年なら `"M/D"`、違えば `"YYYY/M/D"`。
 */
function absoluteDateLabel(iso: string, fetchedAtIso: string): string {
  const d = new Date(iso);
  const now = new Date(fetchedAtIso);
  const y = d.getUTCFullYear();
  const m = d.getUTCMonth() + 1;
  const day = d.getUTCDate();
  return y === now.getUTCFullYear() ? `${m}/${day}` : `${y}/${m}/${day}`;
}

/**
 * 相対時刻（「n 前」）。Console・タイムライン・`/approvals`・`/accounts` 等で使う（ADR-0055 D2
 * ラウンド 7、P-G30-1）。1 単位に丸める（`formatDuration` の期間表示より粗い。コンパクトにするため）:
 * 10 秒未満は「たった今」、それ以降は秒・分・時間・日のうち最大の単位を 1 つだけ出し、7 日を超えたら
 * 絶対日付にする。絶対時刻は返さない（呼び出し側が `title` 属性に生の ISO 文字列を残す）。
 */
export function relativeTimeLabel(iso: string, fetchedAtIso: string): string {
  const totalSeconds = secondsBetween(iso, fetchedAtIso);
  if (totalSeconds >= RELATIVE_ABSOLUTE_THRESHOLD_SECONDS) return absoluteDateLabel(iso, fetchedAtIso);
  const { days, hours, minutes, seconds } = splitDuration(totalSeconds);
  if (days > 0) return `${days}日前`;
  if (hours > 0) return `${hours}時間前`;
  if (minutes > 0) return `${minutes}分前`;
  if (seconds < 10) return "たった今";
  return `${seconds}秒前`;
}

/** ナビの「報告」バッジの色（SPEC §4「良い知らせも悪い知らせも」。bad_news があれば赤）。 */
export function reportsBadgeTone(reportsLive: ReportsLive | null | undefined): "danger" | "neutral" {
  return reportsLive && reportsLive.unread_bad_news > 0 ? "danger" : "neutral";
}

/** 通知の文面（ADR-0033 D3「通知は数時間単位」）。`unread_bad_news > 0` なら先頭に出す。 */
export function notificationMessage(reportsLive: Pick<ReportsLive, "unread_secretary" | "unread_bad_news">): string {
  const parts: string[] = [];
  if (reportsLive.unread_bad_news > 0) parts.push(`悪い知らせ ${reportsLive.unread_bad_news} 件`);
  parts.push(`未読の報告 ${reportsLive.unread_secretary} 件`);
  return parts.join(" / ");
}

/** 通知の重複排除キー（未読の件数の組。変わらない限り同じ通知を鳴らさない）。 */
export function reportsNotificationKey(reportsLive: Pick<ReportsLive, "unread_secretary" | "unread_bad_news">): string {
  return `${reportsLive.unread_secretary}:${reportsLive.unread_bad_news}`;
}

/**
 * ブラウザ通知を今出すべきか（GUI 側の判断）。`notify_now` は celeris が決定的に決める（ADR-0034 D6）ので、
 * GUI はそれに従うだけ（SPEC §3.5「GUI は notify_now に従うだけ」）。ただし celeris 側は
 * 「bad_news の未読があれば notify_now は常に true」（2 時間の間隔を待たない）ため、同じ状態のまま
 * SSE の daemon イベントで再検証されるたびに鳴らし続けないよう、**GUI 側だけ**で
 * 「未読の件数の組が前回と同じなら鳴らさない」という重複排除を足す（判断が必要だった点。celeris には無い規約）。
 */
export function shouldFireNotification(
  reportsLive: ReportsLive | null | undefined,
  permission: "default" | "denied" | "granted",
  lastFiredKey: string | null,
): { fire: boolean; key: string | null } {
  if (!reportsLive || permission !== "granted" || !reportsLive.notify_now) {
    return { fire: false, key: lastFiredKey };
  }
  const key = reportsNotificationKey(reportsLive);
  if (key === lastFiredKey) return { fire: false, key };
  return { fire: true, key };
}
