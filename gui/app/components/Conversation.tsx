import { useEffect, useRef, useState } from "react";
import { Form, Link, useFetcher, useRevalidator, useSearchParams } from "react-router";
import { ErrorFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Button } from "~/components/ui/button";
import { Card, CardBody } from "~/components/ui/card";
import { checkboxClass, hintClass, labelClass, selectClass, textareaClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import {
  CONVERSATION_POLL_MS,
  CONVERSATION_WAIT_LIMIT_MS,
  type ConversationData,
  conversationTaskIds,
  conversationTrouble,
  replyArrived,
  SECRETARY_NODE_ID,
} from "~/lib/conversation";
import { cn } from "~/lib/utils";
import type { ConversationOpOutcome } from "~/taskd/action-types";

/**
 * 「人」との対話（SPEC §3.4「組織の木を見て誰に言うかを決め、その担当に直接言う。相手は人なので、
 * 先週の議論の続きとして話せる」、§4 の 1「秘書との対話」と 2。ADR-0033 D4、
 * docs/taskd-api-v1.md §3.54〜3.55）。`/org/secretary` と `/org/:id` が同じ部品を使う。
 *
 * 送信は 202（`{message_id, task_id}`）で、返事は同期では返らない。だから送ったあとは「考え中」を出し、
 * `GET /org/{id}/messages` を {@link CONVERSATION_POLL_MS} ごとに引き直して、送った発言より後ろに
 * `role = "node"` の行が入ったら止める（`~/lib/conversation.ts` の `replyArrived`）。
 */

/** 送信の直後だけ覚えておくもの（「考え中」の終了条件に使う）。 */
interface Waiting {
  /** 202 の `message_id`（案件を作った直後など、分からないときは null） */
  messageId: string | null;
  /** 202 の `task_id`（返事を作る裏方の run。`attention` との突き合わせに使う） */
  taskId: string | null;
  since: number;
}

export function Conversation({ data }: { data: ConversationData }) {
  const { nodeId, node, projects, projectId, messages, attention } = data;
  const isSecretary = nodeId === SECRETARY_NODE_ID;
  const [searchParams] = useSearchParams();
  const fetcher = useFetcher<ConversationOpOutcome>();
  const revalidator = useRevalidator();

  const [waiting, setWaiting] = useState<Waiting | null>(null);
  const [timedOut, setTimedOut] = useState(false);

  // 案件を作った直後（action の redirect `?waiting=1`）は、秘書の最初の返事を待つ（SPEC §7）。
  const waitParam = searchParams.get("waiting") === "1";
  const waitKey = `${projectId ?? ""}:${waitParam}`;
  const handledWaitKey = useRef<string | null>(null);
  useEffect(() => {
    if (!waitParam || handledWaitKey.current === waitKey) return;
    handledWaitKey.current = waitKey;
    setTimedOut(false);
    setWaiting({ messageId: null, taskId: null, since: Date.now() });
  }, [waitParam, waitKey]);

  // 送信（202）が返ったら「考え中」に入る。
  const handledAccepted = useRef<string | null>(null);
  useEffect(() => {
    const outcome = fetcher.data;
    if (!outcome?.ok || outcome.op !== "send") return;
    if (handledAccepted.current === outcome.accepted.message_id) return;
    handledAccepted.current = outcome.accepted.message_id;
    setTimedOut(false);
    setWaiting({
      messageId: outcome.accepted.message_id,
      taskId: outcome.accepted.task_id ?? null,
      since: Date.now(),
    });
  }, [fetcher.data]);

  // 返事を作れない状態（経路なし・run の失敗）が `GET /inbox` の `attention` に出ていたら、待つのをやめる
  //（監査 M1。理由は taskd の値をそのまま出す。`~/lib/conversation.ts::conversationTrouble`）。
  const trouble = conversationTrouble(attention, [...conversationTaskIds(messages), waiting?.taskId]);

  // 返事が入ったら止める（ポーリングの終了条件）。返事が作れない状態になったときも止める。
  useEffect(() => {
    if (!waiting) return;
    if (replyArrived(messages, waiting.messageId) || trouble !== null) setWaiting(null);
  }, [messages, waiting, trouble]);

  // 返事が来るまで GET を引き直す。待ちすぎたら諦める（run が落ちて何も入らない場合の保険）。
  useEffect(() => {
    if (!waiting) return;
    const timer = setInterval(() => {
      if (Date.now() - waiting.since > CONVERSATION_WAIT_LIMIT_MS) {
        setWaiting(null);
        setTimedOut(true);
        return;
      }
      if (revalidator.state === "idle") revalidator.revalidate();
    }, CONVERSATION_POLL_MS);
    return () => clearInterval(timer);
  }, [waiting, revalidator]);

  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : undefined;
  const submitting = fetcher.state !== "idle";
  const project = projects.find((p) => p.id === projectId) ?? null;
  // 送信のたびに入力欄と「新しい案件として」を初期状態へ戻す（案件を切り替えたときも）。
  const formKey = `${projectId ?? "none"}:${handledAccepted.current ?? ""}`;

  return (
    <div className="space-y-6" data-testid="conversation" data-node-id={nodeId}>
      <PageHeader
        icon="message"
        title={
          <>
            {isSecretary ? "秘書" : (node?.name ?? nodeId)}
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description={
          isSecretary
            ? "あなたの相手。案件を受け取り、組織に流し、報告を集めて渡します。案件を投げる・状況を聞く・方針を変えるのはここから。"
            : "この担当に直接言えます。相手は覚えているので、先週の議論の続きとして話せます。"
        }
        actions={
          <>
            {node?.genre && (
              <span className="text-xs text-fg-subtle" data-testid="conversation-genre">
                {node.genre}
              </span>
            )}
            <Link to="/org" className="text-sm underline underline-offset-2">
              組織の木へ
            </Link>
          </>
        }
      />

      {node?.brief && <p className={hintClass}>{node.brief}</p>}

      <div className="grid gap-6 lg:grid-cols-[18rem_1fr]">
        <div className="space-y-3">
          <SectionTitle icon="folder" count={projects.length}>
            案件
          </SectionTitle>
          <Card>
            <CardBody className="space-y-3">
              <Form method="get" data-testid="conversation-project-form" className="space-y-2">
                <label htmlFor="conversation-project" className={labelClass}>
                  どの案件の話をするか
                </label>
                <select
                  // 案件が変わっても部品は付け替わらない（同じルート）ので、key で選択の初期値を入れ直す。
                  key={projectId ?? "none"}
                  id="conversation-project"
                  name="project"
                  data-testid="conversation-project-select"
                  defaultValue={projectId ?? ""}
                  className={cn(selectClass, "w-full")}
                  onChange={(e) => e.currentTarget.form?.requestSubmit()}
                >
                  <option value="">案件なし（雑談）</option>
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.title}
                    </option>
                  ))}
                </select>
                <p className={hintClass}>案件ごとにやり取りは分かれます（「案件なし」に案件の話は混ざりません）。</p>
                <Button type="submit" variant="secondary" size="sm">
                  <Icon name="refresh" />
                  表示
                </Button>
              </Form>
              {project && (
                <p className="text-sm">
                  <Link to={`/projects/${project.id}`} className="underline underline-offset-2">
                    案件の詳細・仕事の木へ
                  </Link>
                </p>
              )}
            </CardBody>
          </Card>
        </div>

        <div className="space-y-3">
          <SectionTitle icon="message" count={messages.length}>
            やり取り
          </SectionTitle>
          <Card>
            <CardBody className="space-y-3">
              {messages.length === 0 ? (
                <EmptyState icon="message" title="まだ何も話していません">
                  {isSecretary
                    ? "下の欄に案件を投げてください（曖昧なままでかまいません）。秘書が理解の確認・大まかな方針・最初の途中目標を返します。"
                    : "下の欄から、この担当に直接言えます。"}
                </EmptyState>
              ) : (
                <ul className="space-y-3">
                  {messages.map((m) => (
                    <li
                      key={m.id}
                      data-testid="conversation-message"
                      data-role={m.role}
                      className={cn("flex", m.role === "user" ? "justify-end" : "justify-start")}
                    >
                      <div
                        className={cn(
                          "max-w-[46rem] min-w-0 rounded-xl px-3 py-2 text-sm",
                          m.role === "user"
                            ? "bg-primary-soft text-primary-soft-fg"
                            : "border border-border bg-surface-2/50 text-fg",
                        )}
                      >
                        {m.role === "node" ? (
                          <MarkdownViewer content={m.text} />
                        ) : (
                          <p className="whitespace-pre-wrap">{m.text}</p>
                        )}
                        <p className="mt-1 flex items-center gap-2 text-[0.7rem] text-fg-subtle">
                          <span>{m.created_at}</span>
                          {/* `Message.task_id`（GUI からの依頼 R4、Phase 27 で追加。migration 0007）で、
                              画面を開き直した後の過去の返事からも裏方の run（対話用タスク）へたどれる。
                              `role = user` の行にも同じ id が入るが、リンクは意味のある返事の行にだけ出す。 */}
                          {m.run_id &&
                            (m.task_id ? (
                              <Link
                                to={`/tasks/${m.task_id}`}
                                data-testid="conversation-run-link"
                                className="underline underline-offset-2"
                              >
                                この返事を作った run（裏方）
                              </Link>
                            ) : (
                              <Mono title="この返事を作った run（裏方）">run {m.run_id}</Mono>
                            ))}
                        </p>
                      </div>
                    </li>
                  ))}
                </ul>
              )}

              {waiting && !trouble && (
                <p data-testid="conversation-thinking" className="flex items-center gap-2 text-sm text-fg-muted">
                  <Icon name="clock" className="size-4 animate-pulse" />
                  考え中…（返事は裏方の作業が終わってから入ります）
                </p>
              )}
              {trouble && (
                <Alert
                  tone="danger"
                  icon="alert"
                  data-testid="conversation-trouble"
                  title="返事を作れない状態です"
                  className="my-0"
                >
                  <p>
                    {trouble.reason}。
                    <Link to="/providers" className="mx-1 underline underline-offset-2">
                      プロバイダの設定
                    </Link>
                    を確認してください（
                    <Link to={`/tasks/${trouble.taskId}`} className="underline underline-offset-2">
                      裏方のタスク
                    </Link>
                    ）。
                  </p>
                </Alert>
              )}
              {timedOut && !trouble && (
                <Alert
                  tone="warning"
                  icon="clock"
                  data-testid="conversation-timeout"
                  title="しばらく待ちましたが返事が入りませんでした"
                  className="my-0"
                >
                  <p>
                    裏方の作業が落ちているかもしれません。
                    <Link to="/providers" className="mx-1 underline underline-offset-2">
                      プロバイダの設定
                    </Link>
                    と
                    <Link to="/tasks" className="mx-1 underline underline-offset-2">
                      裏方のタスク
                    </Link>
                    を確認してください。
                  </p>
                </Alert>
              )}
            </CardBody>
          </Card>

          <Card>
            <CardBody>
              <ErrorFlash error={error} />
              <fetcher.Form key={formKey} method="post" data-testid="conversation-form" className="space-y-3">
                <input type="hidden" name="project_id" value={projectId ?? ""} />
                <label htmlFor="conversation-text" className={labelClass}>
                  話す
                </label>
                <textarea
                  id="conversation-text"
                  name="text"
                  rows={3}
                  data-testid="conversation-input"
                  placeholder={
                    isSecretary
                      ? "例: Pluvio を基盤に用いた新たな研究テーマの模索、検証"
                      : "例: 先週の続きで、隣接分野も見てほしい"
                  }
                  className={cn(textareaClass, "w-full")}
                />
                {isSecretary && (
                  <label className="flex items-center gap-2 text-sm" htmlFor="conversation-new-project">
                    <input
                      id="conversation-new-project"
                      type="checkbox"
                      name="new_project"
                      data-testid="conversation-new-project-toggle"
                      defaultChecked={projectId === null}
                      className={checkboxClass}
                    />
                    新しい案件として投げる（本文の先頭 40 字が案件名になります）
                  </label>
                )}
                <Button type="submit" variant="primary" disabled={submitting} data-testid="conversation-send">
                  <Icon name="send" />
                  送る
                </Button>
                <p className={hintClass}>
                  送ると受け取られ、返事は裏方の作業が終わってから入ります（数分かかることがあります）。
                </p>
              </fetcher.Form>
            </CardBody>
          </Card>
        </div>
      </div>
    </div>
  );
}
