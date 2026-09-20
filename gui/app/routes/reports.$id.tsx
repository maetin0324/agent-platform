import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse } from "~/celeris/errors";
import type { ReportDetail } from "~/celeris/types";
import type { Route } from "./+types/reports.$id";

/**
 * `/reports/:id`（resource route。コンポーネントは持たない）。`GET /reports/{id}` をそのまま返す
 * （docs/gui/api.md §3.51: `{report, sources_expanded[]}`）。`/reports` の行を展開したときと、
 * `sources_expanded` をさらに辿るとき（下の段の報告へ潜る）の両方で `useFetcher().load()` から呼ぶ
 * （ADR-0033 D3「圧縮の元を見に行ける」）。GUI 側では加工しない。
 */
export async function loadReportDetail(client: CelerisClient, id: string, request: Request): Promise<ReportDetail> {
  return client.get<ReportDetail>(`/reports/${encodeURIComponent(id)}`, { signal: request.signal });
}

export async function loader({ params, request }: Route.LoaderArgs): Promise<ReportDetail> {
  try {
    return await loadReportDetail(getCelerisClient(), params.id, request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}
