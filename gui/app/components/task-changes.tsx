import { useEffect, useState } from "react";
import { Link, useFetcher, useRevalidator } from "react-router";
import type { ActionError, IntegrateOutcome } from "~/celeris/action-types";
import type { ChangeDiffView, ChangesView, RepoChangesView, TaskIntegration } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, EmptyState, Mono } from "~/components/ui/misc";
import {
  CHANGES_MISSING_LABEL,
  CREATE_PR_LABEL,
  changedFileStatusLabel,
  DIFF_TRUNCATED_LABEL,
  DISCARD_CHANGES_CONFIRM_LABEL,
  DISCARD_CHANGES_LABEL,
  integrateMergeLabel,
  integrationMethodLabel,
  integrationStateLabel,
  MERGE_PR_LABEL,
  NO_CHANGES_LABEL,
  prUnavailableReason,
} from "~/lib/labels";
import {
  changedFileStatusTone,
  DIFF_LINE_CLASS,
  fileDeltaChip,
  integrationStateTone,
  parseDiff,
  shortSha,
  statChip,
  taskChangesHref,
} from "~/lib/task-changes";

/**
 * タスクの変更の取り込み（ADR-0043 D5、celeris Phase 54 / G18）。
 * リポジトリごとに「どこから・どこまで・何が変わったか」を出し、人が **merge / PR / 捨てる** を選ぶ。
 * 作業ツリーの閲覧（`~/components/task-files.tsx`）と同じ**自己完結の部品**で、どこに載せても 1 行で済む。
 * いまは `/tasks/:id` の「変更」タブ（ADR-0044 D5 の `?tab=changes`）と、兄弟のルート
 * `~/routes/tasks.$id.changes.tsx`（全画面。差分のリンクと取り込みの送り先）の両方で使う。
 *
 * 差分の切り替えは `<Link>`（`?repo=&file=`）で loader を走らせ、取り込みは
 * `/tasks/:id/changes` の `action` に出す `useFetcher`（**リポジトリごとに 1 つ**。SSE の再検証で
 * 結果が消えないため。監査 H1 / Phase G14 / `ProjectRepos.tsx` と同じ）。クライアントから celeris を
 * 呼ぶコードは無い。
 *
 * 判断はすべて celeris 側（ADR-0043 D5）: 409「main が編集中」も、PR を作れるか（`origin` / `gh`）も、
 * 衝突して「衝突の解消: …」タスクができたことも、celeris が返した値と文言をそのまま出す。
 */
export interface TaskChangesProps {
  taskId: string;
  changes: ChangesView;
  diff: ChangeDiffView | null;
  diffError: ActionError | null;
  diffRepo: string | null;
  diffPath: string | null;
}

export function TaskChanges({ taskId, changes, diff, diffError, diffRepo, diffPath }: TaskChangesProps) {
  const revalidator = useRevalidator();
  const inProgress = changes.delivery != null && !["ready", "blocked"].includes(changes.delivery.state);
  useEffect(() => {
    if (!inProgress) return;
    const timer = setInterval(() => {
      if (revalidator.state === "idle") revalidator.revalidate();
    }, 5000);
    return () => clearInterval(timer);
  }, [inProgress, revalidator]);
  // 差分の相手が一覧に無いとき（知らないリポジトリ名で来た等）も文言は見せる。
  const orphanDiff = diffPath !== null && !changes.repos.some((r) => r.repo === diffRepo);

  return (
    <div className="space-y-4" data-testid="task-changes">
      {changes.delivery && (
        <Card data-testid="delivery-status">
          <CardHeader
            icon="gitBranch"
            title={
              {
                reviewing: "部署内レビュー・マージ判定中",
                merge_queued: "レビュー合格・取り込み待ち",
                merging: "取り込み中",
                preparing: "リリース検証中",
                ready: "デプロイ準備完了",
                blocked: "取り込み・リリース準備の確認が必要",
              }[changes.delivery.state]
            }
          />
          <CardBody className="space-y-3">
            <p className="text-sm break-words">{changes.delivery.detail}</p>
            <p className="text-xs text-fg-muted">実装 → 部署内レビュー・マージ判定 → マージ → 検証 → 人がデプロイ</p>
            <p className="text-sm">レビュー担当: {changes.delivery.department}</p>
            <Link className="text-sm underline" to={`/tasks/${taskId}?tab=runs`}>
              実装・レビューの実行記録を見る
            </Link>
            {changes.delivery.release && (
              <Link
                className="inline-flex min-h-11 items-center rounded-md bg-primary px-4 text-sm font-medium text-primary-fg"
                to={`/releases#release-${changes.delivery.release}`}
              >
                リリース {shortSha(changes.delivery.release)} を確認してデプロイ
              </Link>
            )}
          </CardBody>
        </Card>
      )}
      {changes.repos.length === 0 ? (
        <EmptyState icon="gitBranch" title="git のリポジトリがありません" data-testid="task-changes-empty">
          このタスクは git のリポジトリで作業していないので、取り込むものがありません。
        </EmptyState>
      ) : (
        changes.repos.map((repo) => (
          <RepoChangesCard
            key={repo.repo}
            taskId={taskId}
            repo={repo}
            gh={changes.gh}
            mergeMethod={changes.merge_method}
            diff={diffRepo === repo.repo ? diff : null}
            diffError={diffRepo === repo.repo ? diffError : null}
            diffPath={diffRepo === repo.repo ? diffPath : null}
          />
        ))
      )}
      {orphanDiff && <DiffCard diff={diff} diffError={diffError} diffPath={diffPath} />}
    </div>
  );
}

function RepoChangesCard({
  taskId,
  repo,
  gh,
  mergeMethod,
  diff,
  diffError,
  diffPath,
}: {
  taskId: string;
  repo: RepoChangesView;
  gh: boolean;
  mergeMethod: string;
  diff: ChangeDiffView | null;
  diffError: ActionError | null;
  diffPath: string | null;
}) {
  const fetcher = useFetcher<IntegrateOutcome>({ key: `integrate-${taskId}-${repo.repo}` });
  const [confirming, setConfirming] = useState(false);
  const submitting = fetcher.state !== "idle";
  const prReason = prUnavailableReason(repo.origin, gh);
  const empty = repo.ahead === 0 && repo.files.length === 0;

  return (
    <Card data-testid="task-changes-repo" data-repo={repo.repo} data-repo-missing={repo.missing ? "true" : "false"}>
      <CardHeader
        icon="gitBranch"
        title={
          <span className="flex flex-wrap items-center gap-2">
            <span data-testid="task-changes-repo-name">{repo.repo}</span>
            {repo.dirty && (
              <Badge tone="warning" data-testid="task-changes-dirty">
                未コミットの変更あり
              </Badge>
            )}
            {repo.integration && (
              <Badge tone={integrationStateTone(repo.integration.state)} data-testid="task-changes-integration-state">
                {integrationMethodLabel(repo.integration.method)}: {integrationStateLabel(repo.integration.state)}
              </Badge>
            )}
          </span>
        }
        description={
          <span className="flex flex-wrap items-center gap-1.5" data-testid="task-changes-branch">
            <Mono className="text-xs">{repo.branch}</Mono>
            <Icon name="arrowRight" className="size-3.5" />
            <Mono className="text-xs">{repo.default_branch}</Mono>
          </span>
        }
      />
      <CardBody className="space-y-4">
        {repo.missing ? (
          <Alert tone="neutral" data-testid="task-changes-missing">
            {CHANGES_MISSING_LABEL}
          </Alert>
        ) : (
          <>
            <div className="grid gap-x-6 gap-y-3 sm:grid-cols-3">
              <DataItem label="分岐した地点">
                <Mono data-testid="task-changes-base">{shortSha(repo.base)}</Mono>
              </DataItem>
              <DataItem label="いまの先端">
                <Mono data-testid="task-changes-head">{shortSha(repo.head)}</Mono>
              </DataItem>
              <DataItem label="進んだコミット">
                <span className="tabular-nums" data-testid="task-changes-ahead">
                  {repo.ahead}
                </span>
              </DataItem>
            </div>

            {empty ? (
              <EmptyState icon="checkCircle" title={NO_CHANGES_LABEL} data-testid="task-changes-none">
                このリポジトリでは何も変わっていません（コードを伴わない仕事のこともあります）。
              </EmptyState>
            ) : (
              <div className="space-y-2">
                <div className="text-sm font-medium text-fg-muted" data-testid="task-changes-stat">
                  {statChip(repo.stat)}
                </div>
                <ul className="divide-y divide-border" data-testid="task-changes-files">
                  {repo.files.map((file) => (
                    <li
                      key={file.path}
                      className="flex items-center justify-between gap-3 py-1.5"
                      data-testid="task-changes-file"
                      data-file-status={file.status}
                    >
                      <Link
                        to={taskChangesHref(taskId, { repo: repo.repo, file: file.path })}
                        className="flex min-w-0 items-center gap-2 font-medium text-primary hover:underline"
                        data-testid="task-changes-file-link"
                      >
                        <Badge tone={changedFileStatusTone(file.status)}>
                          {file.status} {changedFileStatusLabel(file.status)}
                        </Badge>
                        <span className="truncate font-mono text-xs">{file.path}</span>
                      </Link>
                      <span className="shrink-0 text-xs text-fg-subtle tabular-nums" data-testid="task-changes-delta">
                        {fileDeltaChip(file)}
                      </span>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </>
        )}

        {repo.integration && (
          <IntegrationCard
            taskId={taskId}
            integration={repo.integration}
            mergeMethod={mergeMethod}
            fetcher={fetcher}
            submitting={submitting}
          />
        )}

        {fetcher.data &&
          (fetcher.data.ok ? (
            <IntegrateResultFlash outcome={fetcher.data} />
          ) : (
            <>
              {/* 409 `default_branch_busy`（「main が編集中」）。`ErrorFlash` は 409 を一律
                  「状態が変わりました」と読むので、celeris の文言と次にやることをここで別に出す。 */}
              {fetcher.data.error.code === "default_branch_busy" && (
                <Alert tone="warning" data-testid="task-changes-busy">
                  <p className="font-semibold">{fetcher.data.error.detail}</p>
                  <p>
                    手元のチェックアウトが {repo.default_branch} で未コミットのままです。片付けてから、もう一度
                    押してください。
                  </p>
                </Alert>
              )}
              <ErrorFlash error={fetcher.data.error} />
            </>
          ))}

        {!repo.missing && (
          <fetcher.Form
            method="post"
            action={`/tasks/${taskId}/changes`}
            className="space-y-3"
            data-testid="task-changes-form"
          >
            <input type="hidden" name="intent" value="integrate" />
            <input type="hidden" name="repo" value={repo.repo} />
            <div className="space-y-1">
              <label className={labelClass} htmlFor={`integrate-note-${repo.repo}`}>
                ひとこと（任意）
              </label>
              <input
                id={`integrate-note-${repo.repo}`}
                name="note"
                type="text"
                className={inputClass}
                data-testid="task-changes-note"
              />
              <p className={hintClass}>記録に残ります（PR の本文には入りません）。</p>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="submit"
                name="method"
                value="merge"
                variant="primary"
                disabled={submitting}
                data-testid="task-changes-merge"
              >
                <Icon name="gitBranch" />
                {integrateMergeLabel(repo.default_branch)}
              </Button>
              <Button
                type="submit"
                name="method"
                value="pr"
                variant="secondary"
                disabled={submitting || prReason !== null}
                title={prReason ?? undefined}
                data-testid="task-changes-pr"
              >
                <Icon name="link" />
                {CREATE_PR_LABEL}
              </Button>
              <Button
                type="button"
                variant="danger"
                disabled={submitting}
                onClick={() => setConfirming(true)}
                data-testid="task-changes-discard"
              >
                <Icon name="x" />
                {DISCARD_CHANGES_LABEL}
              </Button>
            </div>
            {prReason && (
              <p className={hintClass} data-testid="task-changes-pr-reason">
                {prReason}
              </p>
            )}
            {confirming && (
              <Alert tone="danger" data-testid="task-changes-discard-confirm">
                <p>
                  {repo.repo} の作業ツリーとブランチ（{repo.branch}）を消します。取り返しがつきません。
                </p>
                <input type="hidden" name="confirm" value="true" />
                <div className="flex flex-wrap items-center gap-2 pt-1">
                  <Button
                    type="submit"
                    name="method"
                    value="discard"
                    variant="danger"
                    disabled={submitting}
                    data-testid="task-changes-discard-submit"
                  >
                    <Icon name="x" />
                    {DISCARD_CHANGES_CONFIRM_LABEL}
                  </Button>
                  <Button type="button" variant="ghost" onClick={() => setConfirming(false)}>
                    やめる
                  </Button>
                </div>
              </Alert>
            )}
          </fetcher.Form>
        )}

        {diffPath !== null && <DiffCard diff={diff} diffError={diffError} diffPath={diffPath} />}
      </CardBody>
    </Card>
  );
}

/**
 * PR の記録（`integration.method === "pr"`）。状態・番号・URL は celeris が `gh` から取ったものをそのまま出す。
 * 「Celeris で merge」は開いている PR のときだけ（`state === "open"`）。方法は `[github] merge_method`。
 */
function IntegrationCard({
  taskId,
  integration,
  mergeMethod,
  fetcher,
  submitting,
}: {
  taskId: string;
  integration: TaskIntegration;
  mergeMethod: string;
  fetcher: ReturnType<typeof useFetcher<IntegrateOutcome>>;
  submitting: boolean;
}) {
  const isPr = integration.method === "pr";
  return (
    <div
      className="space-y-2 rounded-lg border border-border bg-surface-2 px-4 py-3"
      data-testid="task-changes-integration"
      data-integration-method={integration.method}
      data-integration-state={integration.state}
    >
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <Badge tone={integrationStateTone(integration.state)}>{integrationStateLabel(integration.state)}</Badge>
        <span className="text-fg-muted">{integrationMethodLabel(integration.method)}</span>
        {isPr && integration.pr_number != null && integration.pr_url && (
          <a
            href={integration.pr_url}
            target="_blank"
            rel="noreferrer noopener"
            className="font-medium text-primary underline underline-offset-2"
            data-testid="task-changes-pr-link"
          >
            #{integration.pr_number}
          </a>
        )}
        <span className="text-xs text-fg-subtle tabular-nums">{integration.updated_at}</span>
      </div>
      {integration.detail && (
        <p className="break-words text-sm text-fg-muted" data-testid="task-changes-integration-detail">
          {integration.detail}
        </p>
      )}
      {isPr && integration.state === "open" && (
        <fetcher.Form method="post" action={`/tasks/${taskId}/changes`} className="flex items-center gap-2">
          <input type="hidden" name="intent" value="pr_merge" />
          <input type="hidden" name="repo" value={integration.repo} />
          <Button type="submit" variant="success" disabled={submitting} data-testid="task-changes-pr-merge">
            <Icon name="check" />
            {MERGE_PR_LABEL}
          </Button>
          <span className={hintClass} data-testid="task-changes-merge-method">
            方法: {mergeMethod}
          </span>
        </fetcher.Form>
      )}
    </div>
  );
}

/** 取り込みの結果（衝突・失敗も 200 で返るのでここで出し分ける）。 */
function IntegrateResultFlash({ outcome }: { outcome: Extract<IntegrateOutcome, { ok: true }> }) {
  const { integration, child_task_id } = outcome.result;
  if (integration.state === "conflict") {
    return (
      <Alert role="status" tone="warning" data-testid="flash" data-flash-kind="conflict">
        <p className="font-semibold">rebase が衝突しました。作業ツリーはそのまま残っています。</p>
        {integration.detail && <p data-testid="task-changes-conflict-detail">{integration.detail}</p>}
        {child_task_id && (
          <p>
            <Link
              to={`/tasks/${child_task_id}`}
              className="underline underline-offset-2"
              data-testid="task-changes-child-task"
            >
              衝突の解消タスク（{child_task_id}）
            </Link>
            が done になったら、もう一度「{integration.repo} を取り込む」を押してください。
          </p>
        )}
      </Alert>
    );
  }
  if (integration.state === "failed") {
    return (
      <Alert role="alert" tone="danger" data-testid="flash" data-flash-kind="failed">
        <p className="font-semibold">取り込めませんでした（{integrationMethodLabel(integration.method)}）。</p>
        {integration.detail && <p data-testid="task-changes-failed-detail">{integration.detail}</p>}
      </Alert>
    );
  }
  return (
    <Alert role="status" tone="success" data-testid="flash" data-flash-kind="ok">
      <p>
        {integrationMethodLabel(integration.method)}: {integrationStateLabel(integration.state)}
        {integration.pr_number != null && integration.pr_url && (
          <>
            {" "}
            <a
              href={integration.pr_url}
              target="_blank"
              rel="noreferrer noopener"
              className="underline underline-offset-2"
              data-testid="task-changes-result-pr-link"
            >
              #{integration.pr_number}
            </a>
          </>
        )}
      </p>
      {integration.detail && <p data-testid="task-changes-result-detail">{integration.detail}</p>}
    </Alert>
  );
}

/** 選んだファイルの unified diff。色は行の種類だけで、中身は `<span>` にそのまま入れる。 */
function DiffCard({
  diff,
  diffError,
  diffPath,
}: {
  diff: ChangeDiffView | null;
  diffError: ActionError | null;
  diffPath: string | null;
}) {
  return (
    <Card data-testid="task-changes-diff">
      <CardHeader
        icon="code"
        title={
          <span className="break-all" data-testid="task-changes-diff-path">
            <Mono className="text-sm">{diffPath}</Mono>
          </span>
        }
        description={
          diff?.truncated ? (
            <span className="text-warning-soft-fg" data-testid="task-changes-diff-truncated">
              {DIFF_TRUNCATED_LABEL}
            </span>
          ) : undefined
        }
      />
      <CardBody className="p-0">
        {diffError ? (
          <div className="px-5 py-4">
            <ErrorFlash error={diffError} />
          </div>
        ) : diff ? (
          <DiffBody text={diff.diff} />
        ) : null}
      </CardBody>
    </Card>
  );
}

function DiffBody({ text }: { text: string }) {
  // 同じ本文の行が並ぶので、読み込んだ順の番号を鍵にする（差分は読み取り専用で並べ替えも増減もしない）。
  const lines = parseDiff(text).map((line, index) => ({ ...line, key: `l${index}` }));
  if (lines.length === 0) {
    return (
      <div className="px-5 py-4">
        <Alert tone="neutral" data-testid="task-changes-diff-empty">
          {NO_CHANGES_LABEL}
        </Alert>
      </div>
    );
  }
  return (
    <pre className="overflow-x-auto px-0 py-2 font-mono text-xs leading-5" data-testid="task-changes-diff-body">
      {lines.map((line) => (
        <span
          key={line.key}
          className={`block whitespace-pre px-5 ${DIFF_LINE_CLASS[line.kind]}`}
          data-diff-kind={line.kind}
        >
          {line.text === "" ? " " : line.text}
        </span>
      ))}
    </pre>
  );
}
