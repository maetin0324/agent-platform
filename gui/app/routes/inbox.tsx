import { type ReactNode, useEffect } from "react";
import { data, type FetcherWithComponents, Link, useFetcher, useNavigate } from "react-router";
import type { RetryOutcome, TransitionOutcome } from "~/celeris/action-types";
import type { CelerisClient } from "~/celeris/client.server";
import { getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse, isCelerisUnavailable } from "~/celeris/errors";
import { runInboxAction } from "~/celeris/route-actions.server";
import type { ApprovalItem, AttentionItem, DraftGroup, Inbox, QuestionItem } from "~/celeris/types";
import { RetryFlash, TransitionFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Button } from "~/components/ui/button";
import { checkboxClass, hintClass, textareaClass } from "~/components/ui/form";
import { Icon, type IconName } from "~/components/ui/Icon";
import { Alert, EmptyState, PageHeader, SectionTitle, StatCard } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { relativeTimeLabel } from "~/lib/reports";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import type { Route } from "./+types/inbox";

export function meta(_: Route.MetaArgs) {
  return [{ title: "受信箱 - Celeris" }];
}

/**
 * `GET /inbox` をそのまま返す（派生値は celeris 側で計算済み。GUI は再計算しない）。`/inbox` は root と同じく
 * celeris 停止中も 200 で返す契約（docs/DESIGN.md §10 Phase G0 受け入れ条件 4、docs/adr/0003 D4）があるため、
 * `CelerisUnavailable` はここで catch して `null` にする（root のバナーが既に状況を伝えている）。
 * それ以外の `CelerisError` 等は `Response` に変換して投げる（G1 の他の子ルートと同じ、docs/adr/0004 D6）。
 * `CelerisClient` を引数に取ることでテスト可能にする（`app/celeris/health.server.ts` の `loadHealth` と同じ形）。
 */
export async function loadInbox(client: CelerisClient, request: Request): Promise<Inbox | null> {
  try {
    return await client.get<Inbox>("/inbox", { signal: request.signal });
  } catch (e) {
    if (isCelerisUnavailable(e)) return null;
    throw celerisErrorResponse(e);
  }
}

/**
 * `/inbox`（受信箱、docs/DESIGN.md §4.1）。Phase G13f-1 で `/`（最初の画面）は秘書（`/org/secretary`）に
 * なり、受信箱は裏方の区画に下がった（SPEC §4 の 6 画面に受信箱は無い）。
 */
// 409 / 422 の action 後も再検証する（docs/adr/0005 D2）。
export const shouldRevalidate = revalidateAfterActionErrors;

/**
 * `fetchedAt`（Phase 86、ADR-0055 ラウンド 11）は表示用の相対時刻の基準時刻でしかない
 * （`~/lib/reports.ts::relativeTimeLabel` と同じ規律。celeris への問い合わせ自体は `loadInbox` のまま）。
 * `loadInbox` 自体のテスト（`test/unit/inbox.loader.test.ts`）は変えない。
 */
export interface InboxLoaderData {
  inbox: Inbox | null;
  fetchedAt: string;
}

export async function loader({ request }: Route.LoaderArgs): Promise<InboxLoaderData> {
  const inbox = await loadInbox(getCelerisClient(), request);
  return { inbox, fetchedAt: new Date().toISOString() };
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const outcomes = await runInboxAction(getCelerisClient(), form, request.signal);
  const status = outcomes.every((o) => o.ok) ? 200 : (outcomes.find((o) => !o.ok)?.error.status ?? 500);
  return data(outcomes, { status });
}

/**
 * 操作の結果は **fetcher** に載せる（Phase G13f-1、監査 H1）。ナビゲーション方式の `<Form>` + `actionData` だと、
 * SSE の `daemon` イベント（celeris は tick ごとに無条件で流す）で root が再検証されるたびに `actionData` が
 * 消え、失敗の表示が 0.3 秒で消えてしまう。fetcher の `data` は再検証では消えない。
 */
type InboxFetcher = FetcherWithComponents<TransitionOutcome[] | undefined>;

/** 1 つの fetcher の結果をその場に出す（項目ごとに 1 つ持つので、他の項目の結果と混ざらない）。 */
function InboxFlash({ fetcher }: { fetcher: InboxFetcher }) {
  const outcomes = fetcher.data;
  if (!outcomes || outcomes.length === 0) return null;
  return (
    <div className="space-y-2">
      {outcomes.map((o) => (
        <TransitionFlash key={`${o.taskId}-${o.intent}`} outcome={o} />
      ))}
    </div>
  );
}

export default function InboxPage({ loaderData }: Route.ComponentProps) {
  const { inbox, fetchedAt } = loaderData;
  if (!inbox) {
    return (
      <Alert tone="danger" icon="wifiOff" data-testid="inbox-unavailable">
        celeris に接続できないため受信箱を表示できません。
      </Alert>
    );
  }
  const isEmpty =
    inbox.counts.approvals === 0 &&
    inbox.counts.questions === 0 &&
    inbox.counts.drafts === 0 &&
    inbox.counts.attention === 0;

  return (
    <div className="space-y-8">
      <PageHeader
        as="h1"
        icon="inbox"
        title={
          <>
            受信箱
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="裏方の画面です。人間の対応が要る項目（承認待ち・質問・受け入れ待ちの draft・注意）だけを集めています。普段は「Console」から始めてください。"
      />

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <StatCard label="承認待ち" value={inbox.counts.approvals} icon="checkCircle" tone="warning" />
        <StatCard label="質問" value={inbox.counts.questions} icon="message" tone="info" />
        <StatCard label="受け入れ待ちの draft" value={inbox.counts.drafts} icon="file" tone="neutral" />
        <StatCard label="注意" value={inbox.counts.attention} icon="alert" tone="danger" />
      </div>

      {isEmpty && (
        <Alert tone="info" icon="sparkles" data-testid="inbox-empty-help">
          対応が要る項目はありません。初めて使うなら
          <Link
            to="/help"
            className="mx-1 font-semibold underline underline-offset-2"
            data-testid="inbox-help-onboarding-link"
          >
            使い方を見る
          </Link>
          とこの GUI で何ができるかが分かります。
        </Alert>
      )}

      <SectionCard
        sectionTestId="approvals-section"
        headingId="approvals-heading"
        icon="checkCircle"
        tone="warning"
        heading={`承認待ち（${inbox.counts.approvals}）`}
      >
        {inbox.approvals.length === 0 ? (
          <EmptyState title="ありません。" />
        ) : (
          <ul className="space-y-3">
            {inbox.approvals.map((item) => (
              <ApprovalRow key={item.approval.id} item={item} fetchedAt={fetchedAt} />
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        sectionTestId="questions-section"
        headingId="questions-heading"
        icon="message"
        tone="info"
        heading={`質問（${inbox.counts.questions}）`}
      >
        {inbox.questions.length === 0 ? (
          <EmptyState title="ありません。" />
        ) : (
          <ul className="space-y-3">
            {inbox.questions.map((item) => (
              <QuestionRow key={item.task.id} item={item} fetchedAt={fetchedAt} />
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        sectionTestId="drafts-section"
        headingId="drafts-heading"
        icon="file"
        tone="neutral"
        heading={`受け入れ待ちの draft（${inbox.counts.drafts}）`}
      >
        {inbox.drafts.length === 0 ? (
          <EmptyState title="ありません。" />
        ) : (
          <ul className="space-y-4">
            {inbox.drafts.map((group) => (
              <DraftGroupRow key={group.parent?.id ?? "root"} group={group} />
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard
        sectionTestId="attention-section"
        headingId="attention-heading"
        icon="alert"
        tone="danger"
        heading={`注意（${inbox.counts.attention}）`}
      >
        {inbox.attention.length === 0 ? (
          <EmptyState title="ありません。" />
        ) : (
          <ul className="space-y-3">
            {inbox.attention.map((item) =>
              item.type === "cluster_unavailable" ? (
                // `AttentionItem` の他のバリアントと違い `task` を持たない（クラスタ単位の集約）ので別枝のまま
                // にする（ADR-0009 D5）。押すと `/clusters` に遷移する（Phase G7、ADR-0010 D7）。
                <li
                  key={`cluster_unavailable-${item.cluster}`}
                  data-testid="attention-item"
                  data-attention-type={item.type}
                  className="rounded-lg border border-danger-border bg-danger-soft p-4 text-sm shadow-xs"
                >
                  <p className="font-semibold text-danger-soft-fg">
                    <Link to="/clusters" className="hover:underline" data-testid="attention-cluster-link">
                      {item.cluster}
                    </Link>
                  </p>
                  <p className="mt-1 text-fg">{attentionText(item)}</p>
                </li>
              ) : (
                <AttentionRow key={`${item.type}-${item.task.id}`} item={item} />
              ),
            )}
          </ul>
        )}
      </SectionCard>
    </div>
  );
}

function ApprovalRow({ item, fetchedAt }: { item: ApprovalItem; fetchedAt: string }) {
  const fetcher = useFetcher<TransitionOutcome[]>({ key: `inbox-approval-${item.approval.id}` });
  const submitting = fetcher.state !== "idle";
  return (
    <li
      data-testid="approval-item"
      className="rounded-lg border border-border bg-surface p-4 text-sm shadow-xs transition-shadow hover:shadow-sm"
    >
      <p
        className="flex flex-wrap items-baseline justify-between gap-2 font-semibold text-fg"
        data-testid="approval-title"
      >
        <Link to={`/tasks/${item.approval.id}`} className="hover:underline">
          {item.approval.title}
        </Link>
        <time
          dateTime={item.requested_at}
          title={item.requested_at}
          className="text-xs font-normal text-fg-subtle"
          data-testid="approval-requested-at"
        >
          {relativeTimeLabel(item.requested_at, fetchedAt)}
        </time>
      </p>
      {item.parent && (
        <p className="mt-1 flex flex-wrap items-center gap-1.5 text-fg-muted" data-testid="approval-parent-title">
          親:{" "}
          <Link to={`/tasks/${item.parent.id}`} className="hover:underline">
            {item.parent.title}
          </Link>
          （{item.parent.status}）
        </p>
      )}
      <p className="mt-1 text-fg" data-testid="approval-criterion-text">
        条件: {item.criterion_text}
      </p>
      {item.last_run?.outcome_text && (
        <p className="mt-1 text-fg-muted" data-testid="approval-summary">
          直近 run の要約: {item.last_run.outcome_text}
        </p>
      )}
      {item.other_verdicts.length > 0 && (
        <p className="mt-1 text-fg-muted">
          同 run の他条件:{" "}
          {item.other_verdicts.map((v) => `#${v.criterion_idx} ${v.pass ? "pass" : "fail"}`).join(", ")}
        </p>
      )}
      {item.previous_decisions.length > 0 && (
        <p className="mt-1 text-fg-muted">
          以前の判定: {item.previous_decisions.map((d) => (d.approved ? "承認" : "却下")).join(", ")}
        </p>
      )}
      <fetcher.Form method="post" action="/inbox" className="mt-3 flex flex-col gap-2 border-t border-border pt-3">
        <input type="hidden" name="task_id" value={item.approval.id} />
        <input type="hidden" name="expected_status" value="ready" />
        <textarea
          name="note"
          aria-label="判定の note（任意）"
          data-testid="approval-note"
          rows={2}
          placeholder="判定の note（任意）"
          className={textareaClass}
        />
        <p className={hintClass}>却下の note は次の run の prior_review に届きます。</p>
        <div className="flex gap-2">
          <Button
            type="submit"
            name="intent"
            value="approve"
            variant="success"
            size="sm"
            disabled={submitting}
            data-testid="approval-approve"
          >
            <Icon name="check" />
            承認
          </Button>
          <Button
            type="submit"
            name="intent"
            value="reject"
            variant="danger"
            size="sm"
            disabled={submitting}
            data-testid="approval-reject"
          >
            <Icon name="x" />
            却下
          </Button>
        </div>
      </fetcher.Form>
      <InboxFlash fetcher={fetcher} />
    </li>
  );
}

function QuestionRow({ item, fetchedAt }: { item: QuestionItem; fetchedAt: string }) {
  const fetcher = useFetcher<TransitionOutcome[]>({ key: `inbox-question-${item.task.id}` });
  const submitting = fetcher.state !== "idle";
  // 監査 H2: 質問への回答は「認可」の画面に一本化する（Phase 29 で `approval_id` が付いた）。
  // `approval_id` が無い（まだ行が無い／既に決めた）ときだけ、従来どおりここで回答できるようにする。
  const approvalId = item.approval_id ?? null;
  return (
    <li
      data-testid="question-item"
      className="rounded-lg border border-border bg-surface p-4 text-sm shadow-xs transition-shadow hover:shadow-sm"
    >
      <p className="flex flex-wrap items-baseline justify-between gap-2 font-semibold text-fg">
        <Link to={`/tasks/${item.task.id}`} className="hover:underline">
          {item.task.title}
        </Link>
        {item.asked_at && (
          <time
            dateTime={item.asked_at}
            title={item.asked_at}
            className="text-xs font-normal text-fg-subtle"
            data-testid="question-asked-at"
          >
            {relativeTimeLabel(item.asked_at, fetchedAt)}
          </time>
        )}
      </p>
      <p className="mt-1 text-fg" data-testid="question-text">
        {item.question}
      </p>
      {approvalId !== null ? (
        <p className="mt-3 border-t border-border pt-3">
          <Link
            to={`/approvals#approval-${approvalId}`}
            data-testid="question-approval-link"
            className="font-medium underline underline-offset-2"
          >
            「認可」の画面で答える
          </Link>
          <span className="ml-2 text-fg-subtle">
            （「今回だけ」か「今後ずっと」で答えます。同じ問いへの答えはそちらに集まります）
          </span>
        </p>
      ) : (
        <fetcher.Form method="post" action="/inbox" className="mt-3 flex flex-col gap-2 border-t border-border pt-3">
          <input type="hidden" name="task_id" value={item.task.id} />
          <input type="hidden" name="expected_status" value="blocked" />
          <input type="hidden" name="intent" value="answer" />
          <textarea
            name="answer"
            aria-label="回答"
            data-testid="question-answer"
            rows={3}
            placeholder="回答"
            className={textareaClass}
          />
          <Button
            type="submit"
            variant="primary"
            size="sm"
            disabled={submitting}
            data-testid="question-answer-submit"
            className="w-fit"
          >
            <Icon name="send" />
            回答する
          </Button>
        </fetcher.Form>
      )}
      <InboxFlash fetcher={fetcher} />
    </li>
  );
}

function DraftGroupRow({ group }: { group: DraftGroup }) {
  const fetcher = useFetcher<TransitionOutcome[]>({ key: `inbox-draft-${group.parent?.id ?? "root"}` });
  const submitting = fetcher.state !== "idle";
  return (
    <li data-testid="draft-group" className="rounded-lg border border-border bg-surface p-4 text-sm shadow-xs">
      <p className="font-semibold text-fg">
        {group.parent ? (
          <Link to={`/tasks/${group.parent.id}`} className="hover:underline">
            {group.parent.title}
          </Link>
        ) : (
          "（親なし）"
        )}
      </p>
      {group.plan_summary && <p className="mt-1 text-fg-muted">{group.plan_summary}</p>}
      <ul className="mt-3 space-y-2 divide-y divide-border">
        {group.drafts.map((draft) => (
          <li
            key={draft.id}
            data-testid="draft-item"
            className="flex flex-wrap items-center justify-between gap-2 pt-2 first:pt-0"
          >
            <Link to={`/tasks/${draft.id}`} className="hover:underline">
              {draft.title}
            </Link>
            <fetcher.Form method="post" action="/inbox" className="flex gap-2">
              <input type="hidden" name="task_id" value={draft.id} />
              <input type="hidden" name="expected_status" value="draft" />
              <Button
                type="submit"
                name="intent"
                value="approve"
                variant="success"
                size="xs"
                disabled={submitting}
                data-testid="draft-approve"
              >
                受け入れ
              </Button>
              <Button
                type="submit"
                name="intent"
                value="cancel"
                variant="danger"
                size="xs"
                disabled={submitting}
                data-testid="draft-cancel"
              >
                取り消し
              </Button>
            </fetcher.Form>
          </li>
        ))}
      </ul>
      {group.drafts.length > 0 && (
        <fetcher.Form method="post" action="/inbox" className="mt-3 flex flex-col gap-1 border-t border-border pt-3">
          {group.drafts.map((draft) => (
            <input key={draft.id} type="hidden" name="task_id" value={draft.id} />
          ))}
          <input type="hidden" name="expected_status" value="draft" />
          <Button
            type="submit"
            name="intent"
            value="approve"
            variant="soft"
            size="xs"
            disabled={submitting}
            data-testid="draft-approve-all"
            className="w-fit"
          >
            この Plan の子を全部受け入れ
          </Button>
          <p className={hintClass}>子ごとに順に承認します（途中で失敗しても残りは続行、原子性はありません）。</p>
        </fetcher.Form>
      )}
      <InboxFlash fetcher={fetcher} />
    </li>
  );
}

function AttentionRow({ item }: { item: Exclude<AttentionItem, { type: "cluster_unavailable" }> }) {
  const fetcher = useFetcher<TransitionOutcome[]>({ key: `inbox-attention-${item.task.id}` });
  const submitting = fetcher.state !== "idle";
  // Phase 31: 「やり直す」は新しいタスクを作る（`TransitionOutcome[]` とは形が違う）ので別の fetcher にし、
  // `/tasks/:id` の action（既に retry を扱う）へ直接投げる。成功したら新しいタスクへ遷移する。
  const retryFetcher = useFetcher<RetryOutcome>({ key: `inbox-retry-${item.task.id}` });
  const retrying = retryFetcher.state !== "idle";
  const navigate = useNavigate();
  useEffect(() => {
    if (retryFetcher.data?.ok) {
      navigate(`/tasks/${retryFetcher.data.result.task_id}`);
    }
  }, [retryFetcher.data, navigate]);
  return (
    <li
      data-testid="attention-item"
      data-attention-type={item.type}
      className="rounded-lg border border-danger-border bg-danger-soft p-4 text-sm shadow-xs"
    >
      <p className="font-semibold text-danger-soft-fg">
        <Link to={`/tasks/${item.task.id}`} className="hover:underline">
          {item.task.title}
        </Link>
      </p>
      <p className="mt-1 text-fg">{attentionText(item)}</p>
      {item.task.actions.includes("cancel") && (
        <fetcher.Form method="post" action="/inbox" className="mt-3 border-t border-danger-border/60 pt-3">
          <input type="hidden" name="task_id" value={item.task.id} />
          <input type="hidden" name="expected_status" value={item.task.status} />
          <input type="hidden" name="intent" value="cancel" />
          <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="attention-cancel">
            <Icon name="ban" />
            取り消し
          </Button>
        </fetcher.Form>
      )}
      {item.task.actions.includes("retry") && (
        <retryFetcher.Form
          method="post"
          action={`/tasks/${item.task.id}`}
          className="mt-3 flex flex-col gap-2 border-t border-danger-border/60 pt-3"
        >
          <input type="hidden" name="intent" value="retry" />
          <label className="flex items-center gap-2 text-fg">
            <input type="checkbox" name="accept" value="true" className={checkboxClass} />
            受け入れ済み（ready）で始める
          </label>
          <Button
            type="submit"
            variant="primary"
            size="sm"
            disabled={retrying}
            data-testid="attention-retry"
            className="w-fit"
          >
            <Icon name="rotate" />
            やり直す
          </Button>
        </retryFetcher.Form>
      )}
      <InboxFlash fetcher={fetcher} />
      <RetryFlash outcome={retryFetcher.data} />
    </li>
  );
}

/** 節（承認待ち・質問・draft・注意）の共通の見た目。見出しの文字列・id、section の data-testid は呼び出し側が渡す。 */
function SectionCard({
  sectionTestId,
  headingId,
  icon,
  tone,
  heading,
  children,
}: {
  sectionTestId: string;
  headingId: string;
  icon: IconName;
  tone: Tone;
  heading: string;
  children: ReactNode;
}) {
  return (
    <section aria-labelledby={headingId} data-testid={sectionTestId} className="space-y-3">
      <SectionTitle id={headingId} icon={icon} tone={tone}>
        {heading}
      </SectionTitle>
      {children}
    </section>
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
    case "cluster_unavailable":
      return `クラスタに接続できません（host: ${item.host}、対象 ${item.tasks} 件）`;
    default:
      return "";
  }
}
