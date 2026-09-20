import type { CelerisClient } from "~/celeris/client.server";
import { getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse } from "~/celeris/errors";
import type { Route } from "./+types/console.stream";

/**
 * `/console/stream`（SSE 中継、ADR-0048 D1、docs/gui/api.md §3.99）。resource route（コンポーネントを持たない）。
 * celeris の `GET /console/stream` をそのまま中継する（`~/routes/events.ts` の `relayEvents` と同じ作り: バイト列は
 * 一切加工しない、非 2xx はそのまま同じ status で返す。docs/adr/0004 D6）。
 */
export async function relayConsoleStream(client: CelerisClient, request: Request): Promise<Response> {
  const url = new URL(request.url);
  const scope = url.searchParams.get("scope");
  const since = url.searchParams.get("since");

  let upstream: Response;
  try {
    upstream = await client.consoleStream({ scope, since, signal: request.signal });
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
  return relayConsoleStream(getCelerisClient(), request);
}
