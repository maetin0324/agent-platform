import type { NotifyTestOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import type { NotifyTestResult } from "./types";

/**
 * 「報告」画面（`/reports`）の Discord 区画からのテスト送信（ADR-0037 D4、docs/celeris-api-v1.md §3.65。
 * **管理系**、`token_file` 未設定でも 401）。要求本文は無し。celeris のエラーは例外にせず `NotifyTestOutcome`
 * として返す（`reports-admin.server.ts` と同じ作り）。台帳（`notifications`）には残らない。
 */
export async function sendNotifyTest(client: CelerisClient, signal?: AbortSignal): Promise<NotifyTestOutcome> {
  try {
    const result = await client.post<NotifyTestResult>("/notify/test", {}, { signal });
    return { ok: true, op: "notify_test", result };
  } catch (e) {
    return { ok: false, op: "notify_test", error: toActionError(e) };
  }
}
