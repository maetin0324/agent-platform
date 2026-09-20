import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse } from "~/celeris/errors";
import type { EventsPage } from "~/celeris/types";
import type { Route } from "./+types/tasks.$id.runs.$runId.events";

/**
 * `/tasks/:id/runs/:runId/events`（resource route。コンポーネントは持たない）。
 * `GET /tasks/{id}/runs/{run_id}/events`（docs/gui/api.md §3.100）をそのまま返す。Console の `progress`
 * ブロック（`~/components/ConsoleBlockItem.tsx`）の「すべて見る」が `useFetcher().load()` から呼ぶ
 * （`~/routes/reports.$id.tsx` と同じ、開いたときだけ取りに行く作り。ADR-0048 D1「詳細は必要なときだけ」）。
 */
export async function loadRunEvents(
  client: CelerisClient,
  taskId: string,
  runId: string,
  request: Request,
): Promise<EventsPage> {
  const url = new URL(request.url);
  return client.get<EventsPage>(`/tasks/${encodeURIComponent(taskId)}/runs/${encodeURIComponent(runId)}/events`, {
    query: {
      after_seq: url.searchParams.get("after_seq") ?? undefined,
      limit: url.searchParams.get("limit") ?? undefined,
    },
    signal: request.signal,
  });
}

export async function loader({ params, request }: Route.LoaderArgs): Promise<EventsPage> {
  try {
    return await loadRunEvents(getCelerisClient(), params.id, params.runId, request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}
