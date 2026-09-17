import type { ReportOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import type { ReportsNotifiedResult, ReportsReadBody, ReportsReadResult } from "./types";

/**
 * 「報告」画面（`/reports`）からの既読・通知（ADR-0033 D3、docs/gui/api.md §3.52〜3.53。**管理系**、
 * `token_file` 未設定でも 401）。taskd のエラーは例外にせず `ReportOpOutcome` として返す
 * （`org-admin.server.ts` と同じ作り）。
 */

export async function markReportsRead(
  client: TaskdClient,
  ids: string[],
  signal?: AbortSignal,
): Promise<ReportOpOutcome> {
  try {
    const result = await client.post<ReportsReadResult>("/reports/read", { ids } satisfies ReportsReadBody, {
      signal,
    });
    return { ok: true, op: "reports_read", ids, result };
  } catch (e) {
    return { ok: false, op: "reports_read", error: toActionError(e) };
  }
}

/** GUI がブラウザ通知を出したときに呼ぶ（§3.53）。本文は無い。 */
export async function markReportsNotified(client: TaskdClient, signal?: AbortSignal): Promise<ReportOpOutcome> {
  try {
    const result = await client.post<ReportsNotifiedResult>("/reports/notified", {}, { signal });
    return { ok: true, op: "reports_notified", result };
  } catch (e) {
    return { ok: false, op: "reports_notified", error: toActionError(e) };
  }
}
