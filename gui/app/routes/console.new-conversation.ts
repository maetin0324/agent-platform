import { data, redirect } from "react-router";
import { getCelerisClient } from "~/celeris/client.server";
import { sendNewConversation } from "~/celeris/console.server";
import type { Route } from "./+types/console.new-conversation";

/**
 * `POST /console/new-conversation`（ADR-0054 D1、Phase 67。GUI の配線は Phase 68、ADR-0054 D3）。
 * Console の「新しい会話」ボタンの入口（リソースルート、画面は持たない。`~/routes/logout.ts` と同じ作り）。
 * GET は `/` へ返す（直接開かれても何もしない）。
 */

export function loader() {
  throw redirect("/");
}

export async function action({ request }: Route.ActionArgs) {
  const outcome = await sendNewConversation(getCelerisClient(), request.signal);
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}
