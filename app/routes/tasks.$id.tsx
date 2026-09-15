import { Form, isRouteErrorResponse, Link, useNavigation, useSearchParams } from "react-router";
import { TransitionFlash } from "~/components/Flash";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import { transitionData } from "~/taskd/actions.server";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { runTaskAction } from "~/taskd/route-actions.server";
import type { Action, Event, EventsPage, TaskDetail, TaskRef } from "~/taskd/types";
import type { Route } from "./+types/tasks.$id";

/**
 * `docs/taskd-api-v1.md` §3.6 の `types` フィルタの選択肢。`Event` の `type` タグと同じ。
 */
const EVENT_TYPES: Event["type"][] = [
  "created",
  "transitioned",
  "worker_started",
  "worker_progress",
  "artifact_produced",
  "worker_finished",
  "review_verdict",
  "approval_requested",
  "approval_decided",
  "answered",
  "provider_throttled",
];

const ACTION_LABELS: Record<Action, string> = {
  approve: "承認",
  reject: "却下",
  answer: "回答",
  cancel: "取り消し",
};

export interface TaskDetailData {
  detail: TaskDetail;
  events: EventsPage;
}

/**
 * `/tasks/:id`（タスク詳細、docs/DESIGN.md §4.3）の loader 本体。`GET /tasks/{id}` と
 * `GET /tasks/{id}/events` を並列に呼び、応答をそのまま返す（派生値は taskd 側で計算済み。GUI は再計算しない）。
 * taskd 停止中・タスクが無い（404 `task_not_found`）等は呼び出し側（`loader`）が `Response` に変換して投げる
 * （docs/adr/0004-g1-decisions.md D6。本番ビルドは素の Error を ErrorBoundary に渡す前に汎用 500 へ
 * サニタイズするため、`Response` として投げないと taskd 停止中でもバナーではなく 500 になってしまう）。
 * G1 の範囲: 生ログ・成果物本体・DAG は出さない（`docs/adr/0004-g1-decisions.md` D4）。`GET /tasks/{id}/artifacts` は呼ばない。
 */
export async function loadTaskDetail(client: TaskdClient, taskId: string, request: Request): Promise<TaskDetailData> {
  const url = new URL(request.url);
  // フォームは `types` チェックボックスごとに 1 つずつ付ける（`?types=a&types=b`）。
  // taskd 側はカンマ区切りの単一パラメータを期待する（docs/taskd-api-v1.md §3.6）ので、ここで結合する。
  const types = url.searchParams.getAll("types");
  const [detail, events] = await Promise.all([
    client.get<TaskDetail>(`/tasks/${taskId}`, { signal: request.signal }),
    client.get<EventsPage>(`/tasks/${taskId}/events`, {
      query: { types: types.length > 0 ? types.join(",") : undefined },
      signal: request.signal,
    }),
  ]);
  return { detail, events };
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "タスク詳細 - taskd-gui" }];
}

// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ params, request }: Route.LoaderArgs): Promise<TaskDetailData> {
  try {
    return await loadTaskDetail(getTaskdClient(), params.id, request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export async function action({ request, params }: Route.ActionArgs) {
  const form = await request.formData();
  const outcome = await runTaskAction(getTaskdClient(), params.id, form, request.signal);
  return transitionData(outcome);
}

export default function TaskDetailPage({ loaderData, actionData }: Route.ComponentProps) {
  const { detail, events } = loaderData;
  const { task } = detail;
  const [searchParams] = useSearchParams();
  const selectedTypes = new Set(searchParams.getAll("types"));
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";

  return (
    <div className="space-y-8">
      <section aria-labelledby="header-heading" data-testid="header-section">
        <h1 id="header-heading" className="text-xl font-semibold">
          <span data-testid="task-id">{task.id}</span>
        </h1>
        <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm sm:grid-cols-4">
          <DlItem label="kind" value={task.kind} testId="task-kind" />
          <DlItem label="status" value={task.status} testId="task-status" />
          <DlItem label="title" value={task.title} testId="task-title" />
          <DlItem label="priority" value={String(task.priority)} />
          <DlItem
            label="worker_hint"
            value={`tier=${task.worker_hint.tier}${task.worker_hint.adapter ? `, adapter=${task.worker_hint.adapter}` : ""}`}
          />
          <DlItem label="attempts / max_retries" value={`${task.attempts} / ${task.budget.max_retries}`} />
          <DlItem label="workspace_dir" value={detail.workspace_dir ?? "(remote)"} />
          <DlItem
            label="budget"
            value={`max_turns=${task.budget.max_turns}, max_wall_secs=${task.budget.max_wall_secs}, max_retries=${task.budget.max_retries}`}
          />
        </dl>
        {task.parent_id && (
          <p className="mt-2 text-sm" data-testid="task-parent">
            親: <Link to={`/tasks/${task.parent_id}`}>{task.parent_id}</Link>
          </p>
        )}
        <TaskRefList label="dependencies" testId="dependencies" refs={detail.dependencies} />
        <TaskRefList label="dependents" testId="dependents" refs={detail.dependents} />
        <TaskRefList label="children" testId="children" refs={detail.children} />
      </section>

      <section aria-labelledby="timers-heading" data-testid="timers-section">
        <h2 id="timers-heading" className="text-lg font-semibold">
          タイマー
        </h2>
        <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm sm:grid-cols-4">
          <DlItem label="lease_expires_at" value={detail.timers.lease_expires_at ?? "-"} />
          <DlItem label="backoff_until" value={detail.timers.backoff_until ?? "-"} />
          <DlItem
            label="consecutive_requeues / max_requeues"
            value={`${detail.timers.consecutive_requeues} / ${detail.timers.max_requeues}`}
          />
          <DlItem label="consecutive_reviewer_requeues" value={String(detail.timers.consecutive_reviewer_requeues)} />
          <DlItem label="now" value={detail.timers.now} />
        </dl>
      </section>

      <section aria-labelledby="criteria-heading" data-testid="criteria-section">
        <h2 id="criteria-heading" className="text-lg font-semibold">
          受け入れ条件と判定
        </h2>
        {detail.criteria.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-2">
            {detail.criteria.map((criterion) => (
              <li key={criterion.idx} data-testid="criterion-item" className="rounded border p-2 text-sm">
                <p>
                  #{criterion.idx} [{criterion.check.type}] {criterion.text}
                </p>
                {criterion.latest_verdict && (
                  <p className="text-gray-600" data-testid="criterion-verdict">
                    直近判定: {criterion.latest_verdict.pass ? "pass" : "fail"} — {criterion.latest_verdict.reason}
                  </p>
                )}
                {criterion.check.type === "human" && criterion.approval && (
                  <p data-testid="criterion-approval">
                    Approval:{" "}
                    <Link to={`/tasks/${criterion.approval.approval.id}`}>{criterion.approval.approval.id}</Link>（
                    {criterion.approval.approval.status}）
                  </p>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section aria-labelledby="runs-heading" data-testid="runs-section">
        <h2 id="runs-heading" className="text-lg font-semibold">
          run 一覧
        </h2>
        {detail.runs.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <div className="mt-2 overflow-x-auto">
            <table className="w-full text-left text-sm">
              <thead>
                <tr className="border-b">
                  <th className="p-1">run_id</th>
                  <th className="p-1">role</th>
                  <th className="p-1">adapter</th>
                  <th className="p-1">provider</th>
                  <th className="p-1">model</th>
                  <th className="p-1">started_at</th>
                  <th className="p-1">finished_at</th>
                  <th className="p-1">outcome</th>
                  <th className="p-1">usage</th>
                  <th className="p-1">progress</th>
                  <th className="p-1">artifacts</th>
                  <th className="p-1">verdicts</th>
                  <th className="p-1">files</th>
                </tr>
              </thead>
              <tbody>
                {detail.runs.map((run) => (
                  <tr key={run.run_id} data-testid="run-row" className="border-b align-top">
                    <td className="p-1 font-mono text-xs">{run.run_id}</td>
                    <td className="p-1">{run.role}</td>
                    <td className="p-1">{run.adapter}</td>
                    <td className="p-1">{run.provider ?? "-"}</td>
                    <td className="p-1">{run.model}</td>
                    <td className="p-1">{run.started_at}</td>
                    <td className="p-1">{run.finished_at ?? "-"}</td>
                    <td className="p-1">
                      {run.outcome ?? "-"}
                      {run.outcome_text ? ` (${run.outcome_text})` : ""}
                    </td>
                    <td className="p-1">
                      {run.usage ? `in=${run.usage.input_tokens ?? "-"} out=${run.usage.output_tokens ?? "-"}` : "-"}
                    </td>
                    <td className="p-1">{run.progress}</td>
                    <td className="p-1">{run.artifacts}</td>
                    <td className="p-1">{run.verdicts}</td>
                    <td className="p-1" data-testid="run-files">
                      {run.files
                        ? ["stdout", "stderr", "result"]
                            .filter((k) => run.files?.[k as keyof typeof run.files])
                            .join(", ") || "-"
                        : "-"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section aria-labelledby="timeline-heading" data-testid="timeline-section">
        <h2 id="timeline-heading" className="text-lg font-semibold">
          タイムライン
        </h2>
        <Form method="get" className="mt-2 flex flex-wrap gap-3 text-sm" data-testid="timeline-filter-form">
          {EVENT_TYPES.map((type) => (
            <label key={type} className="flex items-center gap-1">
              <input type="checkbox" name="types" value={type} defaultChecked={selectedTypes.has(type)} />
              {type}
            </label>
          ))}
          <button type="submit" className="rounded border px-2 py-0.5">
            絞り込み
          </button>
        </Form>
        {events.items.length === 0 ? (
          <p className="mt-2 text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-1 text-sm">
            {events.items.map((row) => (
              <li key={row.id} data-testid="event-item" data-event-type={row.event.type} className="rounded border p-1">
                {row.event.type === "worker_progress" ? (
                  <details>
                    <summary>
                      #{row.seq} {row.ts} {row.event.type}
                    </summary>
                    <p>{row.event.msg}</p>
                  </details>
                ) : (
                  <p>
                    #{row.seq} {row.ts} {row.event.type}
                  </p>
                )}
              </li>
            ))}
          </ul>
        )}
        {events.has_more && <p className="mt-2 text-xs text-gray-500">続きがあります（has_more）。</p>}
      </section>

      <section aria-labelledby="prior-review-heading" data-testid="prior-review-section">
        <h2 id="prior-review-heading" className="text-lg font-semibold">
          prior_review
        </h2>
        {detail.prior_review.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-1 text-sm">
            {detail.prior_review.map((note) => (
              <li key={`${note.criterion}-${note.pass}-${note.reason}`} data-testid="prior-review-item">
                #{note.criterion} {note.pass ? "pass" : "fail"} — {note.reason}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section aria-labelledby="answers-heading" data-testid="answers-section">
        <h2 id="answers-heading" className="text-lg font-semibold">
          answers
        </h2>
        {detail.answers.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-1 text-sm">
            {detail.answers.map((note) => (
              <li key={`${note.question}-${note.answer}`} data-testid="answer-item">
                Q: {note.question} / A: {note.answer}
              </li>
            ))}
          </ul>
        )}
        {detail.latest_question && (
          <p className="mt-2 text-sm" data-testid="latest-question">
            最新の質問: {detail.latest_question}
          </p>
        )}
      </section>

      <section aria-labelledby="actions-heading" data-testid="actions-section">
        <h2 id="actions-heading" className="text-lg font-semibold">
          操作
        </h2>
        <TransitionFlash outcome={actionData} />
        {detail.actions.length === 0 ? (
          <p className="text-sm text-gray-500">できる操作はありません。</p>
        ) : (
          <div className="mt-2 flex flex-wrap gap-4">
            {detail.actions.includes("approve") && (
              <Form method="post" className="flex flex-col gap-1">
                <input type="hidden" name="intent" value="approve" />
                <input type="hidden" name="expected_status" value={task.status} />
                <textarea
                  name="note"
                  data-testid="action-note-approve"
                  rows={2}
                  className="rounded border px-2 py-1 text-sm"
                />
                <button
                  type="submit"
                  disabled={submitting}
                  data-testid="action-approve"
                  className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
                >
                  {ACTION_LABELS.approve}
                </button>
              </Form>
            )}
            {detail.actions.includes("reject") && (
              <Form method="post" className="flex flex-col gap-1">
                <input type="hidden" name="intent" value="reject" />
                <input type="hidden" name="expected_status" value={task.status} />
                <textarea
                  name="note"
                  data-testid="action-note-reject"
                  rows={2}
                  className="rounded border px-2 py-1 text-sm"
                />
                <button
                  type="submit"
                  disabled={submitting}
                  data-testid="action-reject"
                  className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
                >
                  {ACTION_LABELS.reject}
                </button>
              </Form>
            )}
            {detail.actions.includes("answer") && (
              <Form method="post" className="flex flex-col gap-1">
                {detail.latest_question && (
                  <p className="text-sm" data-testid="action-question">
                    {detail.latest_question}
                  </p>
                )}
                <input type="hidden" name="intent" value="answer" />
                <input type="hidden" name="expected_status" value={task.status} />
                <textarea
                  name="answer"
                  data-testid="action-answer"
                  rows={3}
                  className="rounded border px-2 py-1 text-sm"
                />
                <button
                  type="submit"
                  disabled={submitting}
                  data-testid="action-answer-submit"
                  className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
                >
                  回答する
                </button>
              </Form>
            )}
            {detail.actions.includes("cancel") && (
              <Form method="post" className="flex flex-col gap-1">
                <input type="hidden" name="intent" value="cancel" />
                <input type="hidden" name="expected_status" value={task.status} />
                <button
                  type="submit"
                  disabled={submitting}
                  data-testid="action-cancel"
                  className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
                >
                  {ACTION_LABELS.cancel}
                </button>
              </Form>
            )}
          </div>
        )}
      </section>

      {detail.worker_run_hint && (
        <section aria-labelledby="worker-run-hint-heading" data-testid="worker-run-hint-section">
          <h2 id="worker-run-hint-heading" className="text-lg font-semibold">
            worker_run_hint
          </h2>
          <code className="mt-2 block rounded bg-gray-100 p-2 text-sm" data-testid="worker-run-hint">
            {detail.worker_run_hint}
          </code>
        </section>
      )}
    </div>
  );
}

function DlItem({ label, value, testId }: { label: string; value: string; testId?: string }) {
  return (
    <div>
      <dt className="text-xs text-gray-500">{label}</dt>
      <dd data-testid={testId}>{value}</dd>
    </div>
  );
}

function TaskRefList({ label, testId, refs }: { label: string; testId: string; refs: TaskRef[] }) {
  return (
    <div className="mt-2" data-testid={testId}>
      <p className="text-xs text-gray-500">{label}</p>
      {refs.length === 0 ? (
        <p className="text-sm text-gray-500">ありません。</p>
      ) : (
        <ul className="text-sm">
          {refs.map((ref) => (
            <li key={ref.id}>
              <Link to={`/tasks/${ref.id}`}>{ref.title}</Link>（{ref.status}）
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * loader が `taskdErrorResponse` で投げた `Response` を `isRouteErrorResponse` で判別する
 * （docs/adr/0004-g1-decisions.md D6）。taskd 停止中はこのルート自身が root と同じバナーを出し
 * （200 にはならないが 500 でもない。§6.5「500 にしない」）、404 は「タスクが見つかりません」にする。
 */
export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as TaskdRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="p-4">
          <TaskdBanner taskdApiUrl={data.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="p-4">
        <h1 className="text-xl font-semibold">
          {data.status === 404 ? "タスクが見つかりません" : `エラー ${data.status}`}
        </h1>
        <p className="mt-2 text-sm text-gray-600">{data.detail}</p>
      </main>
    );
  }

  return (
    <main className="p-4">
      <h1 className="text-xl font-semibold">エラー</h1>
      <p className="mt-2 text-sm text-gray-600">予期しないエラーが起きました。</p>
    </main>
  );
}
