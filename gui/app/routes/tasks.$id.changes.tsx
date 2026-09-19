import { data, isRouteErrorResponse, Link } from "react-router";
import { TaskChanges } from "~/components/task-changes";
import { Icon } from "~/components/ui/Icon";
import { Alert, PageHeader } from "~/components/ui/misc";
import { TaskdBanner } from "~/root";
import type { IntegrateOutcome } from "~/taskd/action-types";
import { getTaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { formString } from "~/taskd/forms";
import {
  integrateChange,
  loadTaskChanges,
  mergePullRequest,
  readIntegrateBody,
  readTaskChangesQuery,
  type TaskChangesData,
} from "~/taskd/task-changes";
import type { Route } from "./+types/tasks.$id.changes";

/**
 * `/tasks/:id/changes`（変更の取り込み。ADR-0043 D5、taskd Phase 54 / G18）。
 * `/tasks/:id/files` と同じ**兄弟のルート**で、中身は全部 `~/components/task-changes.tsx`
 * （自己完結の部品）に入れてある。同じ部品は `/tasks/:id?tab=changes`（ADR-0044 D5 の「変更」タブ）
 * にも載っているが、**このルートは残す**: 差分の `<Link>`（`?repo=&file=`）と取り込みの `fetcher` の
 * 送り先がここで、`ErrorBoundary` の経路も持つ（「ファイル」タブと `/tasks/:id/files` と同じ作り）。
 *
 * 読み取り（一覧・差分）は `?repo=&file=` のリンクで loader を走らせ、取り込み（**管理系。人だけ**）は
 * この `action` の 2 つの intent に流す。判断はすべて taskd 側なので、ここは form → 要求の写しだけ。
 */
export async function loader({ params, request }: Route.LoaderArgs): Promise<TaskChangesData> {
  try {
    return await loadTaskChanges(getTaskdClient(), params.id, readTaskChangesQuery(request), request.signal);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export async function action({ params, request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const repo = formString(form, "repo") ?? "";
  const client = getTaskdClient();

  let outcome: IntegrateOutcome;
  switch (intent) {
    // `merge` / `pr` / `discard`（`method` はボタンが送る）。`discard` の `confirm` は確認欄が出ているときだけ。
    case "integrate":
      outcome = await integrateChange(client, params.id, repo, readIntegrateBody(form), request.signal);
      break;
    // 開いている PR を Celeris から merge する（方法は taskd の `[github] merge_method`）。
    case "pr_merge":
      outcome = await mergePullRequest(client, params.id, repo, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "変更の取り込み - taskd-gui" }];
}

export default function TaskChangesPage({ loaderData }: Route.ComponentProps) {
  const { taskId, changes, diff, diffError, diffRepo, diffPath } = loaderData;
  return (
    <div className="space-y-6">
      <Link
        to={`/tasks/${taskId}`}
        className="inline-flex items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg"
      >
        <Icon name="arrowLeft" />← タスク詳細
      </Link>
      <PageHeader
        icon="gitBranch"
        title="変更の取り込み"
        description="このタスクがブランチに作った変更です。差分を見てから、既定のブランチに取り込む・PR を作る・捨てる のどれかを選びます（人だけができます）。"
      />
      <TaskChanges
        taskId={taskId}
        changes={changes}
        diff={diff}
        diffError={diffError}
        diffRepo={diffRepo}
        diffPath={diffPath}
      />
    </div>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const problem = error.data as TaskdRouteErrorData;
    if (problem.kind === "unavailable") {
      return (
        <main className="p-4">
          <TaskdBanner taskdApiUrl={problem.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-2xl space-y-3 p-6">
        <h1 className="text-xl font-semibold text-fg">
          {problem.status === 404 ? "取り込める変更がありません" : `エラー ${problem.status}`}
        </h1>
        {/* taskd の文言をそのまま出す（404 `file_not_found` / `task_not_found`）。 */}
        <Alert tone="danger">{problem.detail}</Alert>
      </main>
    );
  }
  return (
    <main className="mx-auto max-w-2xl space-y-3 p-6">
      <h1 className="text-xl font-semibold text-fg">エラー</h1>
      <Alert tone="danger">予期しないエラーが起きました。</Alert>
    </main>
  );
}
