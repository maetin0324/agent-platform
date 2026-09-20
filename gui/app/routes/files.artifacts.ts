import type { LoaderFunctionArgs } from "react-router";
import type { CelerisClient } from "~/celeris/client.server";
import { getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse } from "~/celeris/errors";

/**
 * `/files/tasks/:id/artifacts/:idx`（docs/DESIGN.md §6.2 resource `files`、docs/celeris-api-v1.md §3.16）。
 * resource route（コンポーネントを持たない）。celeris の `GET /tasks/{id}/artifacts/{idx}` をそのまま中継する。
 * バイト列は一切加工しない（docs/DESIGN.md §8.3）。
 */

/** celeris から中継してよいヘッダのみ（許可リスト。DESIGN §8.3「本体は resource route が…そのまま中継する」に沿う）。 */
const RELAYED_HEADERS = [
  "content-type",
  "content-disposition",
  "content-length",
  "content-range",
  "accept-ranges",
  "x-celeris-sha256",
  "x-celeris-sha256-current",
  "x-celeris-size",
] as const;

function relayHeaders(upstream: Response): Headers {
  const headers = new Headers();
  for (const name of RELAYED_HEADERS) {
    const value = upstream.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  // celeris も付けているはずだが、GUI 側でも能動的な型を決して返さない防御を保つ（docs/DESIGN.md §8.3）。
  headers.set("x-content-type-options", "nosniff");
  return headers;
}

export async function relayArtifactFile(
  client: CelerisClient,
  taskId: string,
  idx: string,
  request: Request,
): Promise<Response> {
  const searchParams = new URL(request.url).searchParams;
  const offsetParam = searchParams.get("offset");
  const lengthParam = searchParams.get("length");

  let upstream: Response;
  try {
    upstream = await client.file(`/tasks/${taskId}/artifacts/${idx}`, {
      range: request.headers.get("Range"),
      offset: offsetParam !== null ? Number(offsetParam) : undefined,
      length: lengthParam !== null ? Number(lengthParam) : undefined,
      download: searchParams.get("download") === "1",
      signal: request.signal,
    });
  } catch (e) {
    return celerisErrorResponse(e);
  }

  return new Response(upstream.body, { status: upstream.status, headers: relayHeaders(upstream) });
}

export async function loader({ params, request }: LoaderFunctionArgs): Promise<Response> {
  const { id, idx } = params as { id: string; idx: string };
  return relayArtifactFile(getCelerisClient(), id, idx, request);
}
