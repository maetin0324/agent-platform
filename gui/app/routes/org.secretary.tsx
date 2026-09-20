import { redirect } from "react-router";
import type { Route } from "./+types/org.secretary";

/**
 * `/org/secretary`（旧 URL）。ADR-0046 D6（Phase 59）で根ノードの id は `secretary` → `cos` に改名され、
 * GUI 側は P-59-a（GUI Phase G22）で画面の言葉も「秘書」→「Chief of Staff（CoS）」に揃えた。
 * このルートは古いリンク・ブックマークのために残し、`/org/cos`（`~/routes/org.$id.tsx` が
 * `scope=node:cos` の Console として描く）へ 302 するだけにする。celeris には問い合わせない。
 */
export function loader(_: Route.LoaderArgs) {
  return redirect("/org/cos");
}

export default function OrgSecretaryRedirectPage() {
  return null;
}
