import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse } from "~/celeris/errors";
import type { McpCallsView } from "~/celeris/types";
import type { Route } from "./+types/mcp.clients.$id.calls";

/**
 * `/mcp/clients/:id/calls`（resource route。コンポーネントは持たない）。`GET /mcp/calls?client=<id>`
 * （ADR-0056 D4、docs/gui/api.md §3.110〜3.111）をそのまま返す。`/accounts` の「MCP クライアント」節
 * （ADR-0056 D4、GUI Phase 80）が、客のカードを開いたときだけ `useFetcher().load()` から呼ぶ
 * （`~/routes/tasks.$id.runs.$runId.events.ts` の「すべて見る」と同じ、開いたときだけ取りに行く作り）。
 */
export async function loadMcpCalls(client: CelerisClient, clientId: string, request: Request): Promise<McpCallsView> {
  return client.get<McpCallsView>("/mcp/calls", { query: { client: clientId }, signal: request.signal });
}

export async function loader({ params, request }: Route.LoaderArgs): Promise<McpCallsView> {
  try {
    return await loadMcpCalls(getCelerisClient(), params.id, request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}
