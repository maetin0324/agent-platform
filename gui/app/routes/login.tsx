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
import { Button } from "~/components/ui/button";
import { inputClass, labelClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import type { Route } from "./+types/login";

/** 非 loopback（またはパスワード明示）のときのログイン画面（docs/DESIGN.md §8.2、docs/adr/0008 D1〜D4）。 */

export function meta(_: Route.MetaArgs) {
  return [{ title: "ログイン · Celeris" }];
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
    <main className="flex min-h-screen items-center justify-center px-4 py-16">
      <div className="w-full max-w-sm">
        <div className="mb-6 flex flex-col items-center gap-3 text-center">
          <span className="grid size-12 place-items-center rounded-2xl bg-linear-to-br from-primary via-primary to-teal text-white shadow-md ring-1 ring-white/20 dark:text-bg">
            <Icon name="zap" className="size-6" strokeWidth={2.2} />
          </span>
          <p className="text-lg font-bold tracking-tight text-fg">Celeris</p>
        </div>
        <div className="rounded-2xl border border-border bg-surface p-6 shadow-md sm:p-8">
          <h1 className="text-xl font-semibold text-fg">Celeris にログイン</h1>
          <Form method="post" className="mt-6 flex flex-col gap-4">
            <input type="hidden" name="next" value={next} />
            <div className="flex flex-col gap-1.5">
              <label htmlFor="password" className={labelClass}>
                パスワード
              </label>
              <input
                id="password"
                name="password"
                type="password"
                autoComplete="current-password"
                required
                className={inputClass}
                aria-describedby={error ? "login-error" : undefined}
                aria-invalid={error ? true : undefined}
              />
            </div>
            {error && (
              <p
                id="login-error"
                role="alert"
                data-testid="login-error"
                className="flex items-start gap-1.5 text-sm text-danger"
              >
                <Icon name="alert" className="mt-0.5 size-4 shrink-0" />
                {error}
              </p>
            )}
            <Button type="submit" data-testid="login-submit" variant="primary" size="md">
              ログイン
            </Button>
          </Form>
        </div>
      </div>
    </main>
  );
}
