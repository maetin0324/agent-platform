import { data, Form, redirect, useNavigation } from "react-router";
import { ErrorFlash, FieldErrors } from "~/components/Flash";
import type { CreateFailure } from "~/taskd/action-types";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { taskdErrorResponse } from "~/taskd/errors";
import { createPlan } from "~/taskd/route-actions.server";
import type { ConfigView, NewPlanSpec, Tier } from "~/taskd/types";
import type { Route } from "./+types/plans.new";

/**
 * `/plans/new`（Plan 作成、docs/DESIGN.md §4.4）。`NewPlanSpec` と 1:1 のフォームを
 * `POST /plans` にそのまま送る。検証は taskd（task-ops）が行い、422 の文言をそのまま表示する
 * （docs/taskd-api-v1.md §3.14）。`GET /config` の `plan_auto_accept` を説明として出す。
 */

const TIERS: Tier[] = ["frontier", "standard", "cheap"];

/** `GET /config`（`ConfigView`）を取る。エラーは呼び出し側で `Response` に変換する。 */
export async function loadNewPlan(client: TaskdClient, request: Request): Promise<ConfigView> {
  return client.get<ConfigView>("/config", { signal: request.signal });
}

/**
 * フォームから `NewPlanSpec` を組み立てる（pure）。空欄は本文から省き taskd の既定を使う
 * （`deny_unknown_fields` なので `NewPlanSpec` に無いキーは入れない）。`goal` は必須フィールドなので
 * 空でも `""` として送り、taskd の 422 文言をそのまま出す。
 */
export function buildNewPlanSpec(form: FormData): NewPlanSpec {
  const goal = form.get("goal");
  const spec: NewPlanSpec = { goal: typeof goal === "string" ? goal : "" };

  const workspace = form.get("workspace");
  if (typeof workspace === "string" && workspace !== "") spec.workspace = workspace;

  const tier = form.get("tier");
  if (typeof tier === "string" && (TIERS as readonly string[]).includes(tier)) spec.tier = tier as Tier;

  const priority = form.get("priority");
  if (typeof priority === "string" && priority !== "") {
    const n = Number(priority);
    if (!Number.isNaN(n)) spec.priority = n;
  }

  const maxTurns = form.get("max_turns");
  if (typeof maxTurns === "string" && maxTurns !== "") {
    const n = Number(maxTurns);
    if (!Number.isNaN(n)) spec.max_turns = n;
  }

  const maxWallSecs = form.get("max_wall_secs");
  if (typeof maxWallSecs === "string" && maxWallSecs !== "") {
    const n = Number(maxWallSecs);
    if (!Number.isNaN(n)) spec.max_wall_secs = n;
  }

  const maxRetries = form.get("max_retries");
  if (typeof maxRetries === "string" && maxRetries !== "") {
    const n = Number(maxRetries);
    if (!Number.isNaN(n)) spec.max_retries = n;
  }

  return spec;
}

export async function loader({ request }: Route.LoaderArgs): Promise<ConfigView> {
  try {
    return await loadNewPlan(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "Plan 作成 - taskd-gui" }];
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const spec = buildNewPlanSpec(form);
  const outcome = await createPlan(getTaskdClient(), spec, request.signal);
  if (outcome.ok) return redirect(`/tasks/${outcome.task.id}`);
  return data({ ok: false, error: outcome.error } satisfies CreateFailure, { status: outcome.error.status });
}

export default function NewPlanPage({ loaderData, actionData }: Route.ComponentProps) {
  const config = loaderData;
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";
  const error = actionData && !actionData.ok ? actionData.error : null;

  return (
    <div className="space-y-4">
      <h1 className="text-xl font-semibold">Plan 作成</h1>

      <p data-testid="plan-auto-accept" className="rounded border border-gray-200 bg-gray-50 px-3 py-2 text-sm">
        {config.plan_auto_accept
          ? "plan.auto_accept = true: Plan が作った子タスクは自動で ready になります"
          : "plan.auto_accept = false: Plan が作った子タスクは draft のまま受信箱に来ます（人が承認すると ready になります）"}
      </p>

      <ErrorFlash error={error} />

      <Form method="post" data-testid="new-plan-form" className="space-y-4">
        <div>
          <label htmlFor="goal" className="block text-sm font-medium">
            goal
          </label>
          <textarea id="goal" name="goal" rows={4} className="mt-1 w-full rounded border px-2 py-1 text-sm" />
          <FieldErrors error={error} field="goal" />
        </div>

        <div>
          <label htmlFor="workspace" className="block text-sm font-medium">
            workspace
          </label>
          <input id="workspace" name="workspace" type="text" className="mt-1 w-full rounded border px-2 py-1 text-sm" />
          <FieldErrors error={error} field="workspace" />
        </div>

        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <div>
            <label htmlFor="tier" className="block text-sm font-medium">
              tier
            </label>
            <select
              id="tier"
              name="tier"
              defaultValue="frontier"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            >
              {TIERS.map((tier) => (
                <option key={tier} value={tier}>
                  {tier}
                </option>
              ))}
            </select>
          </div>

          <div>
            <label htmlFor="priority" className="block text-sm font-medium">
              priority
            </label>
            <input
              id="priority"
              name="priority"
              type="number"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>

          <div>
            <label htmlFor="max_turns" className="block text-sm font-medium">
              max_turns
            </label>
            <input
              id="max_turns"
              name="max_turns"
              type="number"
              defaultValue={30}
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>

          <div>
            <label htmlFor="max_wall_secs" className="block text-sm font-medium">
              max_wall_secs
            </label>
            <input
              id="max_wall_secs"
              name="max_wall_secs"
              type="number"
              defaultValue={900}
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>

          <div>
            <label htmlFor="max_retries" className="block text-sm font-medium">
              max_retries
            </label>
            <input
              id="max_retries"
              name="max_retries"
              type="number"
              defaultValue={1}
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>
        </div>

        <button
          type="submit"
          data-testid="submit"
          disabled={submitting}
          className="rounded border px-4 py-1.5 text-sm font-medium disabled:opacity-50"
        >
          作成
        </button>
      </Form>
    </div>
  );
}
