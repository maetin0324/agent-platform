import { useEffect, useState } from "react";
import { Form, isRouteErrorResponse, Link, useNavigation, useSearchParams } from "react-router";
import { CodeViewer } from "~/components/CodeViewer";
import { TransitionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { ImageViewer } from "~/components/ImageViewer";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Sha256Badge } from "~/components/Sha256Badge";
import { artifactStatusMessage, isJson, pickViewer } from "~/lib/artifact-view";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import { transitionData } from "~/taskd/actions.server";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import { runTaskAction } from "~/taskd/route-actions.server";
import type { Action, ArtifactList, ArtifactView, Event, EventsPage, TaskDetail, TaskRef } from "~/taskd/types";
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
  artifacts: ArtifactList;
}

/**
 * `/tasks/:id`（タスク詳細、docs/DESIGN.md §4.3）の loader 本体。`GET /tasks/{id}`・`GET /tasks/{id}/events`・
 * `GET /tasks/{id}/artifacts` を並列に呼び、応答をそのまま返す（派生値は taskd 側で計算済み。GUI は再計算しない）。
 * taskd 停止中・タスクが無い（404 `task_not_found`）等は呼び出し側（`loader`）が `Response` に変換して投げる
 * （docs/adr/0004-g1-decisions.md D6。本番ビルドは素の Error を ErrorBoundary に渡す前に汎用 500 へ
 * サニタイズするため、`Response` として投げないと taskd 停止中でもバナーではなく 500 になってしまう）。
 * 生ログ本体は `/tasks/:id/runs/:runId`（別ルート）、DAG は `/graph`（docs/adr/0006-g3-decisions.md D4）。
 */
export async function loadTaskDetail(client: TaskdClient, taskId: string, request: Request): Promise<TaskDetailData> {
  const url = new URL(request.url);
  // フォームは `types` チェックボックスごとに 1 つずつ付ける（`?types=a&types=b`）。
  // taskd 側はカンマ区切りの単一パラメータを期待する（docs/taskd-api-v1.md §3.6）ので、ここで結合する。
  const types = url.searchParams.getAll("types");
  const [detail, events, artifacts] = await Promise.all([
    client.get<TaskDetail>(`/tasks/${taskId}`, { signal: request.signal }),
    client.get<EventsPage>(`/tasks/${taskId}/events`, {
      query: { types: types.length > 0 ? types.join(",") : undefined },
      signal: request.signal,
    }),
    client.get<ArtifactList>(`/tasks/${taskId}/artifacts`, { signal: request.signal }),
  ]);
  return { detail, events, artifacts };
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
  const { detail, events, artifacts } = loaderData;
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
          <HelpLink anchor="screens" label="画面ごとの説明" />
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
          <DlItem label="role" value={detail.role ?? "-"} testId="task-role" />
          <DlItem
            label="budget"
            value={`max_turns=${task.budget.max_turns}, max_wall_secs=${task.budget.max_wall_secs}, max_retries=${task.budget.max_retries}`}
          />
        </dl>
        {detail.cluster && (
          <p className="mt-2 text-sm" data-testid="task-cluster">
            cluster: <Link to="/clusters">{detail.cluster}</Link>
            <span className="ml-2 text-xs text-gray-500" data-testid="task-workspace-note">
              workspace_dir はクラスタ側ではなく手元の写しです（クラスタ側の元のパスは表示されません）。
            </span>
          </p>
        )}
        {task.parent_id && (
          <p className="mt-2 text-sm" data-testid="task-parent">
            親: <Link to={`/tasks/${task.parent_id}`}>{task.parent_id}</Link>
          </p>
        )}
        <p className="mt-2 text-sm">
          <Link to={`/graph?root=${task.id}`} data-testid="task-graph-link">
            DAG で見る
          </Link>
        </p>
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
                  <th className="p-1">ログ</th>
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
                    <td className="p-1">
                      <Link to={`/tasks/${task.id}/runs/${run.run_id}`} data-testid="run-log-link">
                        ログ
                      </Link>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section aria-labelledby="delegated-heading" data-testid="delegated-section">
        <h2 id="delegated-heading" className="text-lg font-semibold">
          委譲
        </h2>
        {detail.delegated.length === 0 ? (
          <p className="mt-2 text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-2">
            {detail.delegated.map((group) => (
              <li
                key={group.run_id}
                data-testid="delegated-group"
                data-run-id={group.run_id}
                className="rounded border p-2 text-sm"
              >
                <p className="text-xs text-gray-500">
                  run <Link to={`/tasks/${task.id}/runs/${group.run_id}`}>{group.run_id}</Link> · {group.ts}
                </p>
                <ul className="mt-1 space-y-1">
                  {group.tasks.map((child) => (
                    <li key={child.id}>
                      <Link to={`/tasks/${child.id}`} data-testid="delegated-child-link" className="hover:underline">
                        {child.title}
                      </Link>
                      （{child.status}）
                    </li>
                  ))}
                </ul>
              </li>
            ))}
          </ul>
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

      <section aria-labelledby="artifacts-heading" data-testid="artifacts-section">
        <h2 id="artifacts-heading" className="text-lg font-semibold">
          成果物
        </h2>
        {artifacts.items.length === 0 ? (
          <p className="mt-2 text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-2">
            {artifacts.items.map((artifact) => (
              <ArtifactRow key={artifact.idx} taskId={task.id} artifact={artifact} />
            ))}
          </ul>
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
 * 成果物 1 件の行（docs/adr/0006-g3-decisions.md D3/D4）。本体は「開く」を押したときだけ
 * `/files/tasks/:id/artifacts/:idx` を fetch し、taskd が返した実際の `Content-Type` でビューアを選ぶ
 * （拡張子からの推測はしない。taskd の値をそのまま使う）。403（`forbidden`）は一覧の `ArtifactView.forbidden`
 * だけで判定し、本体を取りに行かない。
 */
function ArtifactRow({ taskId, artifact }: { taskId: string; artifact: ArtifactView }) {
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState<{ contentType: string; content?: string } | null>(null);
  const href = `/files/tasks/${taskId}/artifacts/${artifact.idx}`;
  const canOpen = artifact.exists && !artifact.forbidden;
  const statusMessage = artifactStatusMessage(artifact);

  useEffect(() => {
    if (!open || body || !canOpen) return;
    let cancelled = false;
    (async () => {
      const res = await fetch(href);
      const contentType = res.headers.get("content-type") ?? "application/octet-stream";
      if (pickViewer(contentType, artifact.artifact.name) === "image") {
        if (!cancelled) setBody({ contentType });
        return;
      }
      const content = await res.text();
      if (!cancelled) setBody({ contentType, content });
    })();
    return () => {
      cancelled = true;
    };
  }, [open, body, canOpen, href, artifact.artifact.name]);

  return (
    <li data-testid="artifact-item" className="rounded border p-2 text-sm">
      <div className="flex items-center justify-between gap-2">
        <div>
          <p className="font-mono" data-testid="artifact-name">
            {artifact.artifact.name}
          </p>
          <p className="text-xs text-gray-500">
            {artifact.artifact.kind} · run {artifact.run_id}
          </p>
        </div>
        {canOpen && (
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => setOpen((v) => !v)}
              data-testid="artifact-toggle"
              className="rounded border px-2 py-0.5"
            >
              {open ? "閉じる" : "開く"}
            </button>
            <a
              href={`${href}?download=1`}
              download
              data-testid="artifact-download"
              className="rounded border px-2 py-0.5"
            >
              保存
            </a>
          </div>
        )}
      </div>
      {statusMessage && (
        <p data-testid={artifact.forbidden ? "artifact-forbidden" : "artifact-missing"} className="mt-1 text-red-700">
          {statusMessage}
        </p>
      )}
      <Sha256Badge
        recorded={artifact.artifact.sha256}
        current={artifact.sha256_current}
        matches={artifact.sha256_matches}
      />
      {open &&
        body &&
        (pickViewer(body.contentType, artifact.artifact.name) === "image" ? (
          <ImageViewer src={href} alt={artifact.artifact.name} />
        ) : pickViewer(body.contentType, artifact.artifact.name) === "markdown" ? (
          <MarkdownViewer content={body.content ?? ""} />
        ) : (
          <CodeViewer content={body.content ?? ""} json={isJson(body.contentType)} />
        ))}
    </li>
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
