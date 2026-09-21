import { data } from "react-router";
import { getCelerisClient } from "~/celeris/client.server";
import { buildInstructBodyFromForm, loadConsole, sendInstruct } from "~/celeris/console.server";
import { celerisErrorResponse, isCelerisUnavailable } from "~/celeris/errors";
import { Console } from "~/components/Console";
import { type ConsoleData, normalizeScope } from "~/lib/console";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import type { Route } from "./+types/home";

/**
 * `/`（Console。ADR-0048 D4、GUI Phase G22）。既定は `scope=all`（全案件の流れ）。`?scope=project:<id>` /
 * `?scope=node:<id>` で深リンクできる（途中目標の「議論」からの遷移など。`~/routes/projects.$id.tsx` の
 * `MilestoneReviewPanel` 参照。ノードは通常 `/org/:id` を使うが、`?scope=` でも受け付ける）。
 *
 * 旧仕様（Phase G13f-1）は `/` を秘書（`/org/secretary`）へ 302 するだけだったが、G22 でその場所自体が
 * Console になった（P-59-a）。「celeris 停止中も 200 で開く」契約（docs/DESIGN.md §10 Phase G0 受け入れ条件 4）
 * はここが引き継ぐ（`/org/secretary` 時代からの継続。`~/routes/org.$id.tsx` の同じ扱いと合わせている）。
 */
export async function loader({ request }: Route.LoaderArgs): Promise<ConsoleData> {
  const url = new URL(request.url);
  const scope = normalizeScope(url.searchParams.get("scope"));
  try {
    return await loadConsole(getCelerisClient(), scope, request);
  } catch (e) {
    if (isCelerisUnavailable(e)) {
      return {
        scope,
        page: { items: [], next_cursor: null },
        org: [],
        projects: [],
        mcpClients: [],
        fetchedAt: new Date().toISOString(),
      };
    }
    throw celerisErrorResponse(e);
  }
}

export const shouldRevalidate = revalidateAfterActionErrors;

export function meta(_: Route.MetaArgs) {
  return [{ title: "Console - Celeris" }];
}

/** `POST /console/instruct`（**管理系**、202 をそのまま返す）。 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const outcome = await sendInstruct(getCelerisClient(), buildInstructBodyFromForm(form), request.signal);
  return data(outcome, { status: outcome.ok ? 202 : outcome.error.status });
}

export default function HomePage({ loaderData }: Route.ComponentProps) {
  return <Console data={loaderData} />;
}
