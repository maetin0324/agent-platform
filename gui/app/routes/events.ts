import type { CelerisClient } from "~/celeris/client.server";
import { getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse } from "~/celeris/errors";
import type { Route } from "./+types/events";

/**
 * `/events`（SSE 中継、docs/DESIGN.md §6.4、docs/celeris-api-v1.md §4）。resource route（コンポーネントを持たない）。
 * celeris の `GET /stream` をそのまま中継する。バイト列は一切加工しない。
 * celeris 側の 503 `too_many_streams`（`CelerisClient` は非 2xx を `CelerisError` にする）や接続不可
 * （`CelerisUnavailable`）は、同じ status の `Response` を返す（同じ経路の document route と違い resource route
 * には ErrorBoundary が無いので、`Response` を投げるのではなくそのまま返す。docs/adr/0004 D6）。
 */
export async function relayEvents(client: CelerisClient, request: Request): Promise<Response> {
  const url = new URL(request.url);
  const taskId = url.searchParams.get("task_id");
  const lastEventId = request.headers.get("Last-Event-ID");

  let upstream: Response;
  try {
    upstream = await client.stream({
      taskId: taskId ?? undefined,
      lastEventId: lastEventId ?? undefined,
      signal: request.signal,
    });
  } catch (e) {
    return celerisErrorResponse(e);
  }

  return new Response(upstream.body, {
    status: upstream.status,
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-store",
      "X-Accel-Buffering": "no",
    },
  });
}

export async function loader({ request }: Route.LoaderArgs): Promise<Response> {
  return relayEvents(getCelerisClient(), request);
}
