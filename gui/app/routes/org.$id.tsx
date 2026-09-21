import { data } from "react-router";
import { getCelerisClient } from "~/celeris/client.server";
import { buildInstructBodyFromForm, loadConsole, sendInstruct } from "~/celeris/console.server";
import { celerisErrorResponse, isCelerisUnavailable } from "~/celeris/errors";
import { Console } from "~/components/Console";
import { type ConsoleData, scopeForNode } from "~/lib/console";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import type { Route } from "./+types/org.$id";

/**
 * `/org/:id`（組織の木から選んだ「人」の Console。ADR-0048 D4「ノードの画面は同じ部品で
 * `scope = node:<id>`」、GUI Phase G22）。CoS（`id: "cos"`）もこのルートで受ける（`/org/cos` は
 * 専用のルートを持たない。`~/routes/org.secretary.tsx` が旧 URL からの 302 だけを担う。P-59-a）。
 *
 * G13b 時代の `~/components/Conversation.tsx`（202 → ポーリングで返事を待つ部品）は Phase G22 で削除した:
 * Console は `GET /console/stream` で block ごとに届くので、ポーリングは不要になった。旧「新しい案件として」
 * チェックボックスの代わりは、CoS に「案件として」と伝えると `propose_project` action を宣言する経路
 * （ADR-0048 D3）に統合された（`/projects` の新規フォームは従来どおり別に残る）。
 */
export async function loader({ params, request }: Route.LoaderArgs): Promise<ConsoleData> {
  const scope = scopeForNode(params.id);
  try {
    return await loadConsole(getCelerisClient(), scope, request);
  } catch (e) {
    if (isCelerisUnavailable(e)) {
      return {
        scope,
        page: { items: [], next_cursor: null },
        org: [],
        projects: [],
        fetchedAt: new Date().toISOString(),
      };
    }
    throw celerisErrorResponse(e);
  }
}

export const shouldRevalidate = revalidateAfterActionErrors;

export function meta({ params }: Route.MetaArgs) {
  return [{ title: `${params.id} - Celeris` }];
}

/** `POST /console/instruct`（**管理系**、202 をそのまま返す）。 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const outcome = await sendInstruct(getCelerisClient(), buildInstructBodyFromForm(form), request.signal);
  return data(outcome, { status: outcome.ok ? 202 : outcome.error.status });
}

export default function OrgConsolePage({ loaderData }: Route.ComponentProps) {
  return <Console data={loaderData} />;
}
