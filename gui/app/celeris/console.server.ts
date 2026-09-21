import type { ConsoleData } from "~/lib/console";
import type { ConsoleInstructOutcome, ConsoleNewConversationOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import type { ConsoleInstructAccepted, ConsolePage, InstructBody, OrgList, ProjectList } from "./types";

/**
 * Console（ADR-0048 D1/D3/D4、celeris Phase 60a/60b、GUI Phase G22）の中継。
 * - `GET /console?scope=&limit=` は読み取り（管理系ではない）。`POST /console/instruct` は**管理系**。
 * - `GET /org` / `GET /projects` は範囲の選択肢（左の帯の「案件」「ノード」の候補）と `@node` 補完に使うだけ
 *   （`~/celeris/conversation.server.ts` の `loadConversation` と同じく、落ちても Console 自体は出す）。
 * - 履歴の初期表示だけをここで読む。以後の更新は `GET /console/stream`（`~/routes/console.stream.ts` の中継 +
 *   `~/hooks/useConsoleStream.ts`）が受け持つ（画面側は再取得しない。D1「Console は…」）。
 */

/** 1 回の `GET /console` で読む件数（celeris の上限は 200。画面の初期表示はこれで十分）。 */
export const CONSOLE_PAGE_LIMIT = 100;

export async function loadConsole(client: CelerisClient, scope: string, request: Request): Promise<ConsoleData> {
  const [page, org, projects] = await Promise.all([
    client.get<ConsolePage>("/console", { query: { scope, limit: CONSOLE_PAGE_LIMIT }, signal: request.signal }),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
    client.get<ProjectList>("/projects", { signal: request.signal }).catch(() => ({ items: [] }) as ProjectList),
  ]);
  return { scope, page, org: org.items, projects: projects.items };
}

/** `POST /console/instruct`（**管理系**、202 `ConsoleInstructAccepted`）。応答はそのまま画面へ渡す。 */
export async function sendInstruct(
  client: CelerisClient,
  body: InstructBody,
  signal?: AbortSignal,
): Promise<ConsoleInstructOutcome> {
  try {
    const accepted = await client.post<ConsoleInstructAccepted>("/console/instruct", body, { signal });
    return { ok: true, op: "instruct", accepted };
  } catch (e) {
    return { ok: false, op: "instruct", error: toActionError(e) };
  }
}

/** フォーム（`text` / `scope`）から `InstructBody` を読む。空の `scope` は送らない（celeris の既定 = CoS）。 */
export function buildInstructBodyFromForm(form: FormData): InstructBody {
  const text = (form.get("text") as string | null) ?? "";
  const scope = (form.get("scope") as string | null) || undefined;
  return scope ? { text, scope } : { text };
}

/**
 * `POST /console/new-conversation`（ADR-0054 D1、Phase 67。GUI の配線は Phase 68、ADR-0054 D3）。
 * CoS の継続セッションを捨てる（**管理系**、204・本文なし）。GUI の「新しい会話」ボタンの入口。
 */
export async function sendNewConversation(
  client: CelerisClient,
  signal?: AbortSignal,
): Promise<ConsoleNewConversationOutcome> {
  try {
    await client.post<void>("/console/new-conversation", {}, { signal });
    return { ok: true, op: "new_conversation" };
  } catch (e) {
    return { ok: false, op: "new_conversation", error: toActionError(e) };
  }
}
