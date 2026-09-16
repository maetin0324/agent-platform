import { redirect } from "react-router";
import { clearSessionCookie, getAuthConfig } from "~/auth.server";
import type { Route } from "./+types/logout";

/** `POST /logout`: セッションクッキーを消して `/login` へ（docs/adr/0008 D1）。GET は `/` へ返す。 */

export function loader() {
  throw redirect("/");
}

export async function action({ request }: Route.ActionArgs) {
  const config = getAuthConfig();
  if (!config.enabled) throw redirect("/");
  throw redirect("/login", { headers: { "Set-Cookie": await clearSessionCookie(config, request) } });
}
