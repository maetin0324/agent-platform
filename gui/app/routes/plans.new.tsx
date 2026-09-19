import { data, redirect, useFetcher } from "react-router";
import { ErrorFlash, FieldErrors } from "~/components/Flash";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, selectClass, textareaClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, PageHeader } from "~/components/ui/misc";
import { cn } from "~/lib/utils";
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
  return [{ title: "Plan 作成 - Celeris" }];
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const spec = buildNewPlanSpec(form);
  const outcome = await createPlan(getTaskdClient(), spec, request.signal);
  if (outcome.ok) return redirect(`/tasks/${outcome.task.id}`);
  return data({ ok: false, error: outcome.error } satisfies CreateFailure, { status: outcome.error.status });
}

export default function NewPlanPage({ loaderData }: Route.ComponentProps) {
  const config = loaderData;
  // 失敗が SSE の再検証で消えないよう fetcher に載せる（監査 H1）。成功は action の redirect で移る。
  const fetcher = useFetcher<CreateFailure>();
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : null;

  return (
    <div className="space-y-6">
      <PageHeader
        icon="sparkles"
        title="Plan 作成"
        description="NewPlanSpec を taskd に送信します。Plan は taskd が子タスクに分解します。"
      />

      <Alert tone={config.plan_auto_accept ? "success" : "info"} data-testid="plan-auto-accept">
        {config.plan_auto_accept
          ? "plan.auto_accept = true: Plan が作った子タスクは自動で ready になります"
          : "plan.auto_accept = false: Plan が作った子タスクは draft のまま受信箱に来ます（人が承認すると ready になります）"}
      </Alert>

      <ErrorFlash error={error} />

      <fetcher.Form method="post" data-testid="new-plan-form" className="space-y-6">
        <Card>
          <CardHeader icon="file" title="基本" description="Plan の目的と作業ディレクトリ" />
          <CardBody className="space-y-4">
            <div>
              <label htmlFor="goal" className={labelClass}>
                goal
              </label>
              <textarea id="goal" name="goal" rows={4} className={cn(textareaClass, "mt-1.5 w-full")} />
              <p className={cn(hintClass, "mt-1")}>taskd が分解する Plan 全体の目的。</p>
              <FieldErrors error={error} field="goal" />
            </div>

            <div>
              <label htmlFor="workspace" className={labelClass}>
                workspace
              </label>
              <input id="workspace" name="workspace" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
              <FieldErrors error={error} field="workspace" />
            </div>
          </CardBody>
        </Card>

        <Card>
          <CardHeader icon="clock" title="予算" description="子タスクに引き継ぐ既定の優先度・実行上限" />
          <CardBody>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
              <div>
                <label htmlFor="tier" className={labelClass}>
                  tier
                </label>
                <select id="tier" name="tier" defaultValue="frontier" className={cn(selectClass, "mt-1.5 w-full")}>
                  {TIERS.map((tier) => (
                    <option key={tier} value={tier}>
                      {tier}
                    </option>
                  ))}
                </select>
              </div>

              <div>
                <label htmlFor="priority" className={labelClass}>
                  priority
                </label>
                <input id="priority" name="priority" type="number" className={cn(inputClass, "mt-1.5 w-full")} />
              </div>

              <div>
                <label htmlFor="max_turns" className={labelClass}>
                  max_turns
                </label>
                <input
                  id="max_turns"
                  name="max_turns"
                  type="number"
                  defaultValue={30}
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>

              <div>
                <label htmlFor="max_wall_secs" className={labelClass}>
                  max_wall_secs
                </label>
                <input
                  id="max_wall_secs"
                  name="max_wall_secs"
                  type="number"
                  defaultValue={900}
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>

              <div>
                <label htmlFor="max_retries" className={labelClass}>
                  max_retries
                </label>
                <input
                  id="max_retries"
                  name="max_retries"
                  type="number"
                  defaultValue={1}
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>
            </div>
          </CardBody>
        </Card>

        <div className="flex justify-end">
          <Button type="submit" variant="primary" size="md" data-testid="submit" disabled={submitting}>
            <Icon name="send" />
            作成
          </Button>
        </div>
      </fetcher.Form>
    </div>
  );
}
