import { data, Form, redirect } from "react-router";
import {
  FAILED_LOGIN_DELAY_MS,
  getAuthConfig,
  issueSessionCookie,
  safeNextPath,
  sessionContext,
  sleep,
  verifyPassword,
} from "~/auth.server";
import type { Route } from "./+types/login";

/** 非 loopback（またはパスワード明示）のときのログイン画面（docs/DESIGN.md §8.2、docs/adr/0008 D1〜D4）。 */

export function meta(_: Route.MetaArgs) {
  return [{ title: "ログイン · taskd-gui" }];
}

export async function loader({ request, context }: Route.LoaderArgs) {
  const session = context.get(sessionContext);
  if (!session.enabled || session.authenticated) throw redirect("/");
  const next = safeNextPath(new URL(request.url).searchParams.get("next"));
  return { next };
}

export async function action({ request, context }: Route.ActionArgs) {
  const session = context.get(sessionContext);
  if (!session.enabled) throw redirect("/");
  const form = await request.formData();
  const password = form.get("password");
  const next = safeNextPath(typeof form.get("next") === "string" ? (form.get("next") as string) : null);
  const config = getAuthConfig();
  if (typeof password !== "string" || !verifyPassword(config, password)) {
    await sleep(FAILED_LOGIN_DELAY_MS);
    return data({ error: "パスワードが違います。", next }, { status: 401 });
  }
  throw redirect(next, { headers: { "Set-Cookie": await issueSessionCookie(config, request) } });
}

export default function LoginPage({ loaderData, actionData }: Route.ComponentProps) {
  const next = actionData?.next ?? loaderData.next;
  const error = actionData?.error;
  return (
    <main className="mx-auto max-w-sm py-16">
      <h1 className="mb-4 text-xl font-semibold">taskd-gui にログイン</h1>
      <Form method="post" className="flex flex-col gap-3">
        <input type="hidden" name="next" value={next} />
        <label htmlFor="password" className="text-sm font-medium">
          パスワード
        </label>
        <input
          id="password"
          name="password"
          type="password"
          autoComplete="current-password"
          required
          className="rounded border px-2 py-1"
          aria-describedby={error ? "login-error" : undefined}
          aria-invalid={error ? true : undefined}
        />
        {error && (
          <p id="login-error" role="alert" data-testid="login-error" className="text-sm text-red-700">
            {error}
          </p>
        )}
        <button
          type="submit"
          data-testid="login-submit"
          className="rounded bg-gray-900 px-3 py-1.5 text-sm font-semibold text-white hover:bg-gray-700"
        >
          ログイン
        </button>
      </Form>
    </main>
  );
}
