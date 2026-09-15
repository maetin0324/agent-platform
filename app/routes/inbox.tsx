import { data, Form, Link, useNavigation } from "react-router";
import { TransitionFlash } from "~/components/Flash";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { isTaskdUnavailable, taskdErrorResponse } from "~/taskd/errors";
import { runInboxAction } from "~/taskd/route-actions.server";
import type { AttentionItem, Inbox } from "~/taskd/types";
import type { Route } from "./+types/inbox";

export function meta(_: Route.MetaArgs) {
  return [{ title: "受信箱 - taskd-gui" }];
}

/**
 * `GET /inbox` をそのまま返す（派生値は taskd 側で計算済み。GUI は再計算しない）。`/` は root と同じく
 * taskd 停止中も 200 で返す契約（docs/DESIGN.md §10 Phase G0 受け入れ条件 4、docs/adr/0003 D4）があるため、
 * `TaskdUnavailable` はここで catch して `null` にする（root のバナーが既に状況を伝えている）。
 * それ以外の `TaskdError` 等は `Response` に変換して投げる（G1 の他の子ルートと同じ、docs/adr/0004 D6）。
 * `TaskdClient` を引数に取ることでテスト可能にする（`app/taskd/health.server.ts` の `loadHealth` と同じ形）。
 */
export async function loadInbox(client: TaskdClient, request: Request): Promise<Inbox | null> {
  try {
    return await client.get<Inbox>("/inbox", { signal: request.signal });
  } catch (e) {
    if (isTaskdUnavailable(e)) return null;
    throw taskdErrorResponse(e);
  }
}

/** `/`（受信箱、docs/DESIGN.md §4.1）。 */
// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<Inbox | null> {
  return loadInbox(getTaskdClient(), request);
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const outcomes = await runInboxAction(getTaskdClient(), form, request.signal);
  const status = outcomes.every((o) => o.ok) ? 200 : (outcomes.find((o) => !o.ok)?.error.status ?? 500);
  return data(outcomes, { status });
}

export default function InboxPage({ loaderData, actionData }: Route.ComponentProps) {
  const inbox = loaderData;
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";
  if (!inbox) {
    return (
      <p className="text-sm text-gray-500" data-testid="inbox-unavailable">
        taskd に接続できないため受信箱を表示できません。
      </p>
    );
  }
  return (
    <div className="space-y-8">
      {actionData?.map((o) => (
        <TransitionFlash key={`${o.taskId}-${o.intent}`} outcome={o} />
      ))}

      <section aria-labelledby="approvals-heading" data-testid="approvals-section">
        <h2 id="approvals-heading" className="text-lg font-semibold">
          承認待ち（{inbox.counts.approvals}）
        </h2>
        {inbox.approvals.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-3">
            {inbox.approvals.map((item) => (
              <li
                key={item.approval.id}
                data-testid="approval-item"
                className="rounded border border-amber-300 bg-amber-50 p-3 text-sm"
              >
                <p className="font-semibold" data-testid="approval-title">
                  <Link to={`/tasks/${item.approval.id}`} className="hover:underline">
                    {item.approval.title}
                  </Link>
                </p>
                {item.parent && (
                  <p className="text-gray-600" data-testid="approval-parent-title">
                    親: <Link to={`/tasks/${item.parent.id}`}>{item.parent.title}</Link>（{item.parent.status}）
                  </p>
                )}
                <p data-testid="approval-criterion-text">条件: {item.criterion_text}</p>
                {item.last_run?.outcome_text && (
                  <p className="text-gray-600" data-testid="approval-summary">
                    直近 run の要約: {item.last_run.outcome_text}
                  </p>
                )}
                {item.other_verdicts.length > 0 && (
                  <p className="text-gray-600">
                    同 run の他条件:{" "}
                    {item.other_verdicts.map((v) => `#${v.criterion_idx} ${v.pass ? "pass" : "fail"}`).join(", ")}
                  </p>
                )}
                {item.previous_decisions.length > 0 && (
                  <p className="text-gray-600">
                    以前の判定: {item.previous_decisions.map((d) => (d.approved ? "承認" : "却下")).join(", ")}
                  </p>
                )}
                <Form method="post" className="mt-2 flex flex-col gap-1">
                  <input type="hidden" name="task_id" value={item.approval.id} />
                  <input type="hidden" name="expected_status" value="ready" />
                  <textarea
                    name="note"
                    aria-label="判定の note（任意）"
                    data-testid="approval-note"
                    rows={2}
                    className="rounded border px-2 py-1 text-sm"
                  />
                  <p className="text-xs text-gray-500">却下の note は次の run の prior_review に届きます。</p>
                  <div className="flex gap-2">
                    <button
                      type="submit"
                      name="intent"
                      value="approve"
                      disabled={submitting}
                      data-testid="approval-approve"
                      className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
                    >
                      承認
                    </button>
                    <button
                      type="submit"
                      name="intent"
                      value="reject"
                      disabled={submitting}
                      data-testid="approval-reject"
                      className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
                    >
                      却下
                    </button>
                  </div>
                </Form>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section aria-labelledby="questions-heading" data-testid="questions-section">
        <h2 id="questions-heading" className="text-lg font-semibold">
          質問（{inbox.counts.questions}）
        </h2>
        {inbox.questions.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-3">
            {inbox.questions.map((item) => (
              <li
                key={item.task.id}
                data-testid="question-item"
                className="rounded border border-sky-300 bg-sky-50 p-3 text-sm"
              >
                <p className="font-semibold">
                  <Link to={`/tasks/${item.task.id}`} className="hover:underline">
                    {item.task.title}
                  </Link>
                </p>
                <p data-testid="question-text">{item.question}</p>
                <Form method="post" className="mt-2 flex flex-col gap-1">
                  <input type="hidden" name="task_id" value={item.task.id} />
                  <input type="hidden" name="expected_status" value="blocked" />
                  <input type="hidden" name="intent" value="answer" />
                  <textarea
                    name="answer"
                    aria-label="回答"
                    data-testid="question-answer"
                    rows={3}
                    className="rounded border px-2 py-1 text-sm"
                  />
                  <button
                    type="submit"
                    disabled={submitting}
                    data-testid="question-answer-submit"
                    className="w-fit rounded border px-3 py-1 text-sm disabled:text-gray-400"
                  >
                    回答する
                  </button>
                </Form>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section aria-labelledby="drafts-heading" data-testid="drafts-section">
        <h2 id="drafts-heading" className="text-lg font-semibold">
          受け入れ待ちの draft（{inbox.counts.drafts}）
        </h2>
        {inbox.drafts.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-4">
            {inbox.drafts.map((group) => (
              <li key={group.parent?.id ?? "root"} data-testid="draft-group" className="rounded border p-3 text-sm">
                <p className="font-semibold">
                  {group.parent ? (
                    <Link to={`/tasks/${group.parent.id}`} className="hover:underline">
                      {group.parent.title}
                    </Link>
                  ) : (
                    "（親なし）"
                  )}
                </p>
                {group.plan_summary && <p className="text-gray-600">{group.plan_summary}</p>}
                <ul className="mt-2 space-y-1">
                  {group.drafts.map((draft) => (
                    <li key={draft.id} data-testid="draft-item" className="flex flex-col gap-1">
                      <Link to={`/tasks/${draft.id}`} className="hover:underline">
                        {draft.title}
                      </Link>
                      <Form method="post" className="flex gap-2">
                        <input type="hidden" name="task_id" value={draft.id} />
                        <input type="hidden" name="expected_status" value="draft" />
                        <button
                          type="submit"
                          name="intent"
                          value="approve"
                          disabled={submitting}
                          data-testid="draft-approve"
                          className="rounded border px-2 py-0.5 text-xs disabled:text-gray-400"
                        >
                          受け入れ
                        </button>
                        <button
                          type="submit"
                          name="intent"
                          value="cancel"
                          disabled={submitting}
                          data-testid="draft-cancel"
                          className="rounded border px-2 py-0.5 text-xs disabled:text-gray-400"
                        >
                          取り消し
                        </button>
                      </Form>
                    </li>
                  ))}
                </ul>
                {group.drafts.length > 0 && (
                  <Form method="post" className="mt-2 flex flex-col gap-1">
                    {group.drafts.map((draft) => (
                      <input key={draft.id} type="hidden" name="task_id" value={draft.id} />
                    ))}
                    <input type="hidden" name="expected_status" value="draft" />
                    <button
                      type="submit"
                      name="intent"
                      value="approve"
                      disabled={submitting}
                      data-testid="draft-approve-all"
                      className="w-fit rounded border px-2 py-0.5 text-xs disabled:text-gray-400"
                    >
                      この Plan の子を全部受け入れ
                    </button>
                    <p className="text-xs text-gray-500">
                      子ごとに順に承認します（途中で失敗しても残りは続行、原子性はありません）。
                    </p>
                  </Form>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section aria-labelledby="attention-heading" data-testid="attention-section">
        <h2 id="attention-heading" className="text-lg font-semibold">
          注意（{inbox.counts.attention}）
        </h2>
        {inbox.attention.length === 0 ? (
          <p className="text-sm text-gray-500">ありません。</p>
        ) : (
          <ul className="mt-2 space-y-3">
            {inbox.attention.map((item) => (
              <li
                key={`${item.type}-${item.task.id}`}
                data-testid="attention-item"
                data-attention-type={item.type}
                className="rounded border border-red-300 bg-red-50 p-3 text-sm"
              >
                <p className="font-semibold">
                  <Link to={`/tasks/${item.task.id}`} className="hover:underline">
                    {item.task.title}
                  </Link>
                </p>
                <p>{attentionText(item)}</p>
                {item.task.actions.includes("cancel") && (
                  <Form method="post" className="mt-2">
                    <input type="hidden" name="task_id" value={item.task.id} />
                    <input type="hidden" name="expected_status" value={item.task.status} />
                    <input type="hidden" name="intent" value="cancel" />
                    <button
                      type="submit"
                      disabled={submitting}
                      data-testid="attention-cancel"
                      className="rounded border px-3 py-1 text-sm disabled:text-gray-400"
                    >
                      取り消し
                    </button>
                  </Form>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

function attentionText(item: AttentionItem): string {
  switch (item.type) {
    case "failed":
      return `failed: ${item.reason}`;
    case "requeue_limit_near":
      return `requeue が上限間近: ${item.count}/${item.max}`;
    case "unroutable":
      return `経路なし（hint: tier=${item.hint.tier}）`;
    default:
      return "";
  }
}
