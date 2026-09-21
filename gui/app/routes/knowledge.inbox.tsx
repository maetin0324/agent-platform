import { useState } from "react";
import { data, isRouteErrorResponse, Link, useFetcher } from "react-router";
import type { KnowledgeOpOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { formString } from "~/celeris/forms";
import { type KnowledgeInboxData, loadKnowledgeInbox } from "~/celeris/knowledge";
import {
  acceptKnowledgeCandidate,
  readKnowledgeAcceptBody,
  rejectKnowledgeCandidate,
} from "~/celeris/knowledge-admin.server";
import type { KnowledgeCandidate } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { KnowledgeMeta } from "~/components/KnowledgeMeta";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Badge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { checkboxClass, chipLabelClass, hintClass, inputClass, labelClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono, PageHeader } from "~/components/ui/misc";
import {
  knowledgeHref,
  knowledgeOpHint,
  knowledgeOpTone,
  knowledgePathProblem,
  prepareKnowledgeBody,
} from "~/lib/knowledge";
import {
  KNOWLEDGE_ACCEPT_LABEL,
  KNOWLEDGE_CANCEL_LABEL,
  KNOWLEDGE_INBOX_DESCRIPTION,
  KNOWLEDGE_INBOX_EMPTY_LABEL,
  KNOWLEDGE_INBOX_LABEL,
  KNOWLEDGE_OVERWRITE_LABEL,
  KNOWLEDGE_REJECT_CONFIRM_LABEL,
  KNOWLEDGE_REJECT_LABEL,
  KNOWLEDGE_TARGET_EXISTS_LABEL,
  KNOWLEDGE_TARGET_LABEL,
  KNOWLEDGE_UNINITIALIZED_HINT,
  KNOWLEDGE_UNINITIALIZED_TITLE,
  knowledgeErrorHint,
} from "~/lib/labels";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/knowledge.inbox";

/**
 * `/knowledge/inbox`（知識の「候補」。ADR-0047 D5、docs/celeris-api-v1.md §3.101〜3.103。Phase 61 / G21）。
 *
 * `_inbox/` の候補は**索引にも検索にも出ない**（§3.98）。取り込む（accept）か捨てる（reject）かを
 * 人が決める画面で、どちらも**管理系**。宛先が既にあれば celeris が 409 `page_exists` を返すので、
 * GUI は `overwrite` を送れるようにするだけで判定はしない。
 */
export async function loadKnowledgeInboxData(client: CelerisClient, request: Request): Promise<KnowledgeInboxData> {
  return loadKnowledgeInbox(client, request.signal);
}

export async function loader({ request }: Route.LoaderArgs): Promise<KnowledgeInboxData> {
  try {
    return await loadKnowledgeInboxData(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const id = formString(form, "id") ?? "";
  const client = getCelerisClient();

  let outcome: KnowledgeOpOutcome;
  switch (intent) {
    case "accept":
      outcome = await acceptKnowledgeCandidate(client, id, readKnowledgeAcceptBody(form), request.signal);
      break;
    case "reject":
      outcome = await rejectKnowledgeCandidate(client, id, request.signal);
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "知識の候補 - Celeris" }];
}

export default function KnowledgeInboxRoute({ loaderData }: Route.ComponentProps) {
  const { inbox, error } = loaderData;
  return (
    <div className="space-y-6" data-testid="knowledge-inbox">
      <Link
        to={knowledgeHref()}
        className="inline-flex min-h-11 items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg"
      >
        <Icon name="arrowLeft" />← 知識
      </Link>
      <PageHeader
        icon="inbox"
        eyebrow="知識"
        title={KNOWLEDGE_INBOX_LABEL}
        description={KNOWLEDGE_INBOX_DESCRIPTION}
        actions={
          inbox ? (
            // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
            <span className="flex items-center gap-2 text-sm text-fg-subtle lg:text-xs">
              <Mono data-testid="knowledge-inbox-root">{inbox.root}</Mono>
              <Badge tone="neutral" data-testid="knowledge-inbox-total">
                {inbox.items.length}
              </Badge>
            </span>
          ) : null
        }
      />

      {error && (
        <div data-testid="knowledge-inbox-error">
          <ErrorFlash error={error} />
          {knowledgeErrorHint(error.code) && <Alert tone="warning">{knowledgeErrorHint(error.code)}</Alert>}
        </div>
      )}

      {inbox && !inbox.initialized && (
        <Card data-testid="knowledge-inbox-uninitialized">
          <CardHeader icon="database" title={<h2>{KNOWLEDGE_UNINITIALIZED_TITLE}</h2>} />
          <CardBody className="space-y-2">
            <p className="text-sm text-fg-muted">
              {KNOWLEDGE_UNINITIALIZED_TITLE}。<Mono>celerisctl knowledge init</Mono> で用意してください（
              <Mono>{inbox.root}</Mono>）。
            </p>
            <p className={hintClass}>{KNOWLEDGE_UNINITIALIZED_HINT}</p>
          </CardBody>
        </Card>
      )}

      {inbox?.initialized &&
        (inbox.items.length === 0 ? (
          <EmptyState icon="inbox" title={KNOWLEDGE_INBOX_EMPTY_LABEL}>
            組織が <Mono>celerisctl knowledge record</Mono> で書き留めると、ここに並びます。
          </EmptyState>
        ) : (
          <ul className="space-y-4">
            {inbox.items.map((candidate) => (
              <li key={candidate.id}>
                <CandidateCard candidate={candidate} />
              </li>
            ))}
          </ul>
        ))}
    </div>
  );
}

/** 候補 1 件（題名・置き場・タグ・出典・確度・取り込み先・本文と、取り込む / 捨てる）。 */
function CandidateCard({ candidate }: { candidate: KnowledgeCandidate }) {
  const fetcher = useFetcher<KnowledgeOpOutcome>({ key: `knowledge-inbox-${candidate.id}` });
  const submitting = fetcher.state !== "idle";
  const [target, setTarget] = useState(candidate.target);
  const [confirming, setConfirming] = useState(false);
  const pathProblem = knowledgePathProblem(target);
  const outcome = fetcher.data;
  const error = outcome && !outcome.ok ? outcome.error : null;

  return (
    <Card data-testid="knowledge-candidate" data-candidate-id={candidate.id}>
      <CardHeader
        icon="file"
        title={
          <h2 data-testid="knowledge-candidate-title" className="flex flex-wrap items-center gap-2">
            {candidate.title}
            {candidate.op && (
              <Badge tone={knowledgeOpTone(candidate.op)} data-testid="knowledge-candidate-op">
                {candidate.op}
              </Badge>
            )}
          </h2>
        }
        description={
          <span className="flex flex-wrap items-center gap-2">
            <Mono data-testid="knowledge-candidate-path" className="break-all">
              {candidate.path}
            </Mono>
            {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
            {candidate.created && <span className="text-sm text-fg-subtle lg:text-xs">{candidate.created}</span>}
          </span>
        }
      />
      <CardBody className="space-y-4">
        {error && (
          <div data-testid="knowledge-candidate-error">
            <ErrorFlash error={error} />
            {knowledgeErrorHint(error.code) && <Alert tone="warning">{knowledgeErrorHint(error.code)}</Alert>}
          </div>
        )}
        {outcome?.ok && outcome.op === "knowledge_accept" && (
          <Alert tone="success" data-testid="knowledge-accepted">
            取り込みました（
            <Link to={knowledgeHref({ path: outcome.result.path })}>
              <Mono>{outcome.result.path}</Mono>
            </Link>
            ）
          </Alert>
        )}
        {outcome?.ok && outcome.op === "knowledge_reject" && (
          <Alert tone="success" data-testid="knowledge-rejected">
            捨てました（履歴には残ります）
          </Alert>
        )}

        <KnowledgeMeta
          scope={candidate.scope}
          tags={candidate.tags}
          sources={candidate.sources}
          confidence={candidate.confidence}
        />

        <MarkdownViewer content={prepareKnowledgeBody(candidate.body, candidate.target)} />

        {candidate.target_exists && (
          <Alert tone="warning" data-testid="knowledge-target-exists">
            {KNOWLEDGE_TARGET_EXISTS_LABEL}
          </Alert>
        )}

        {knowledgeOpHint(candidate.op) && (
          <Alert tone="info" data-testid="knowledge-op-hint">
            {knowledgeOpHint(candidate.op)}
          </Alert>
        )}

        <fetcher.Form method="post" action="/knowledge/inbox" className="space-y-3">
          <input type="hidden" name="intent" value="accept" />
          <input type="hidden" name="id" value={candidate.id} />
          <div className="space-y-1">
            <label className={labelClass} htmlFor={`knowledge-target-${candidate.id}`}>
              {KNOWLEDGE_TARGET_LABEL}
            </label>
            <input
              id={`knowledge-target-${candidate.id}`}
              name="path"
              className={inputClass}
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              aria-invalid={pathProblem ? true : undefined}
              data-testid="knowledge-candidate-target"
            />
            {pathProblem ? (
              <p className="text-sm text-danger lg:text-xs" data-testid="knowledge-candidate-target-problem">
                {pathProblem}
              </p>
            ) : (
              <p className={hintClass}>知識ベースの根からの相対パス（`.md`）。空にすると候補の既定に任せます。</p>
            )}
          </div>
          <label className={chipLabelClass}>
            <input
              type="checkbox"
              name="overwrite"
              value="1"
              className={checkboxClass}
              defaultChecked={false}
              data-testid="knowledge-candidate-overwrite"
            />
            {KNOWLEDGE_OVERWRITE_LABEL}
          </label>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              type="submit"
              variant="primary"
              size="sm"
              disabled={submitting || pathProblem !== null}
              data-testid="knowledge-accept"
            >
              <Icon name="check" />
              {KNOWLEDGE_ACCEPT_LABEL}
            </Button>
            <Link to={knowledgeHref({ path: target })} className={buttonClass({ variant: "ghost", size: "sm" })}>
              取り込み先を見る
            </Link>
          </div>
        </fetcher.Form>

        {confirming ? (
          <fetcher.Form method="post" action="/knowledge/inbox" className="flex items-center gap-2">
            <input type="hidden" name="intent" value="reject" />
            <input type="hidden" name="id" value={candidate.id} />
            <Button
              type="submit"
              variant="danger"
              size="xs"
              disabled={submitting}
              data-testid="knowledge-reject-confirm"
            >
              {KNOWLEDGE_REJECT_CONFIRM_LABEL}
            </Button>
            <Button type="button" variant="ghost" size="xs" onClick={() => setConfirming(false)}>
              {KNOWLEDGE_CANCEL_LABEL}
            </Button>
          </fetcher.Form>
        ) : (
          <Button
            type="button"
            variant="ghost"
            size="xs"
            onClick={() => setConfirming(true)}
            data-testid="knowledge-reject"
          >
            <Icon name="x" />
            {KNOWLEDGE_REJECT_LABEL}
          </Button>
        )}
      </CardBody>
    </Card>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const problem = error.data as CelerisRouteErrorData;
    if (problem.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={problem.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-2xl space-y-3 p-6">
        <h1 className="text-xl font-semibold text-fg">エラー {problem.status}</h1>
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
