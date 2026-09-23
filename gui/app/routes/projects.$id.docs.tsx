import { useMemo, useState } from "react";
import { data, isRouteErrorResponse, Link, useFetcher } from "react-router";
import type { DocsOpOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type DocsData, loadDocs, readDocsQuery } from "~/celeris/docs";
import { deleteDocPage, initDocs, putDocPage, readDocPagePutBody } from "~/celeris/docs-admin.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { formString } from "~/celeris/forms";
import type { DocCommit, ProjectDetail } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { RouteRecovery } from "~/components/RouteRecovery";
import { Badge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, textareaClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono, PageHeader } from "~/components/ui/misc";
import { type DocTreeNode, docsHref, docTree, insideFolder, prepareDocBody, shortDocSha } from "~/lib/docs";
import {
  DOCS_CANCEL_LABEL,
  DOCS_DELETE_CONFIRM_LABEL,
  DOCS_DELETE_LABEL,
  DOCS_EDIT_LABEL,
  DOCS_EMPTY_LABEL,
  DOCS_HISTORY_LABEL,
  DOCS_INIT_LABEL,
  DOCS_NEW_PAGE_LABEL,
  DOCS_RELOAD_LABEL,
  DOCS_SAVE_LABEL,
  DOCS_SEARCH_LABEL,
  DOCS_SECTION_DESCRIPTION,
  DOCS_TOO_LARGE_LABEL,
  DOCS_TRUNCATED_LABEL,
  docsErrorHint,
} from "~/lib/labels";
import { isTransientStatus } from "~/lib/recovery";
import { cn } from "~/lib/utils";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/projects.$id.docs";

/**
 * `/projects/:id/docs`（案件の「文書」。ADR-0044 D7、docs/celeris-api-v1.md §3.92〜3.97。Phase 57 / G20）。
 *
 * **正本は git**（celeris が主なリポジトリの `docs/**.md` を読み書きする）。GUI は文書を持たず、
 * 描画に使う HTML も celeris が返すが、`dangerouslySetInnerHTML` は使わない規律（gui/CLAUDE.md）に
 * 従って画面では `react-markdown` で描く（生 HTML は文字として出る）。`celeris:task/<id>` と
 * `[[相対パス.md]]` は `~/lib/docs.ts` の純粋関数でリンクに開く。
 *
 * 変更（用意する・保存・削除）は全部**管理系**で、衝突（409 `etag_mismatch` /
 * `default_branch_busy`）は celeris が決めたものをそのまま出す（GUI では判定しない）。
 */
export async function loadProjectDocs(client: CelerisClient, id: string, request: Request): Promise<DocsData> {
  const detail = await client.get<ProjectDetail>(`/projects/${encodeURIComponent(id)}`, {
    signal: request.signal,
  });
  return loadDocs(client, id, detail.project.title, readDocsQuery(request), request.signal);
}

export async function loader({ params, request }: Route.LoaderArgs): Promise<DocsData> {
  try {
    return await loadProjectDocs(getCelerisClient(), params.id, request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export async function action({ params, request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getCelerisClient();

  let outcome: DocsOpOutcome;
  switch (intent) {
    case "init":
      outcome = await initDocs(client, params.id, request.signal);
      break;
    case "save":
      outcome = await putDocPage(client, params.id, readDocPagePutBody(form), request.signal);
      break;
    case "delete":
      outcome = await deleteDocPage(
        client,
        params.id,
        formString(form, "path") ?? "",
        formString(form, "etag"),
        request.signal,
      );
      break;
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "文書 - Celeris" }];
}

export default function ProjectDocsPage({ loaderData }: Route.ComponentProps) {
  const { projectId, projectTitle, tree, page, treeError, pageError, path, q, edit } = loaderData;
  const fetcher = useFetcher<DocsOpOutcome>({ key: `docs-${projectId}` });
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : null;

  return (
    <div className="space-y-6" data-testid="project-docs">
      <Link
        to={`/projects/${projectId}`}
        className="inline-flex min-h-11 items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg"
      >
        <Icon name="arrowLeft" />← 案件詳細
      </Link>
      <PageHeader
        icon="book"
        eyebrow={projectTitle}
        title="文書"
        description={DOCS_SECTION_DESCRIPTION}
        actions={
          tree ? (
            // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
            <span className="flex items-center gap-2 text-sm text-fg-subtle lg:text-xs">
              <Badge tone="neutral">{tree.repo}</Badge>
              <Mono>
                {tree.root}/ @ {tree.default_branch}
              </Mono>
            </span>
          ) : null
        }
      />

      {error && (
        <div data-testid="docs-error">
          <ErrorFlash error={error} />
          {docsErrorHint(error.code) && (
            <Alert tone="warning" data-testid="docs-error-hint">
              <p>{docsErrorHint(error.code)}</p>
              {error.code === "etag_mismatch" && path && (
                <Link
                  to={docsHref(projectId, { path, q })}
                  className={buttonClass({ variant: "secondary", size: "xs" })}
                >
                  <Icon name="refresh" />
                  {DOCS_RELOAD_LABEL}
                </Link>
              )}
            </Alert>
          )}
        </div>
      )}
      {fetcher.data?.ok && fetcher.data.op === "docs_init" && (
        <Alert tone="success" data-testid="docs-init-done">
          文書リポジトリを用意しました（<Mono>{fetcher.data.result.path}</Mono>）。
        </Alert>
      )}
      {fetcher.data?.ok && fetcher.data.op !== "docs_init" && (
        <Alert tone="success" data-testid="docs-saved">
          {fetcher.data.result.deleted
            ? "削除しました"
            : fetcher.data.result.unchanged
              ? "変わっていません"
              : "保存しました"}
          （<Mono>{fetcher.data.result.path}</Mono>）
        </Alert>
      )}

      {!tree ? (
        <DocsUnavailable projectId={projectId} error={treeError} submitting={submitting} fetcher={fetcher} />
      ) : (
        <div className="grid gap-4 lg:grid-cols-[minmax(16rem,22rem)_1fr]">
          <DocsSidebar projectId={projectId} tree={tree} path={path} q={q} />
          <div className="space-y-4">
            {pageError && (
              <div data-testid="docs-page-error">
                <ErrorFlash error={pageError} />
              </div>
            )}
            {edit || (path && !page && !pageError) ? (
              <PageEditor
                projectId={projectId}
                path={path ?? ""}
                raw={page?.raw ?? ""}
                etag={page?.etag ?? null}
                q={q}
                submitting={submitting}
                fetcher={fetcher}
              />
            ) : page ? (
              <PageView projectId={projectId} page={page} q={q} submitting={submitting} fetcher={fetcher} />
            ) : (
              <EmptyState icon="book" title="ページを選んでください">
                <Link
                  to={docsHref(projectId, { path: `${tree.root}/new-page.md`, q })}
                  className={buttonClass({ variant: "secondary", size: "sm" })}
                >
                  <Icon name="plus" />
                  {DOCS_NEW_PAGE_LABEL}
                </Link>
              </EmptyState>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/** 文書リポジトリがまだ無い案件（409 `docs_unavailable`）。「用意する」だけを出す。 */
function DocsUnavailable({
  projectId,
  error,
  submitting,
  fetcher,
}: {
  projectId: string;
  error: DocsData["treeError"];
  submitting: boolean;
  fetcher: ReturnType<typeof useFetcher<DocsOpOutcome>>;
}) {
  const missing = error?.code === "docs_unavailable";
  return (
    <Card data-testid="docs-unavailable">
      <CardHeader icon="book" title={<h2>文書の置き場</h2>} />
      <CardBody className="space-y-3">
        {error && <ErrorFlash error={error} />}
        {missing && (
          <>
            <p className="text-sm text-fg-muted">
              案件の主なリポジトリ（git）の <Mono>docs/</Mono> が文書の置き場になります。まだ無い案件には、 Celeris が{" "}
              <Mono>~/workspace/&lt;案件&gt;/</Mono> に文書リポジトリを作って主なリポジトリに登録します。
            </p>
            <fetcher.Form method="post" action={docsHref(projectId)}>
              <input type="hidden" name="intent" value="init" />
              <Button type="submit" variant="primary" size="sm" disabled={submitting} data-testid="docs-init">
                <Icon name="plus" />
                {DOCS_INIT_LABEL}
              </Button>
            </fetcher.Form>
          </>
        )}
      </CardBody>
    </Card>
  );
}

/** 左の列: 検索とツリー（フォルダは畳める）。 */
function DocsSidebar({
  projectId,
  tree,
  path,
  q,
}: {
  projectId: string;
  tree: NonNullable<DocsData["tree"]>;
  path: string | null;
  q: string | null;
}) {
  const nodes = useMemo(() => docTree(tree.items, tree.root), [tree.items, tree.root]);
  const [collapsed, setCollapsed] = useState<string[]>([]);
  // 畳んだフォルダの中の行は出さない（判定は `~/lib/docs.ts` の純粋関数）。
  const hidden = (node: DocTreeNode) => collapsed.some((folder) => insideFolder(node, folder));

  return (
    <Card data-testid="docs-tree">
      <CardHeader icon="search" title={<h2 className="text-sm">ページ</h2>} />
      <CardBody className="space-y-3">
        <form method="get" action={docsHref(projectId)} className="space-y-1">
          <label className={labelClass} htmlFor="docs-q">
            {DOCS_SEARCH_LABEL}
          </label>
          <div className="flex gap-2">
            <input
              id="docs-q"
              name="q"
              type="search"
              defaultValue={q ?? ""}
              className={inputClass}
              data-testid="docs-search"
            />
            <Button type="submit" variant="secondary" size="sm">
              <Icon name="search" />
              検索
            </Button>
          </div>
          {path && <input type="hidden" name="path" value={path} />}
          <p className={hintClass}>本文をそのまま探します（大文字小文字は区別しません）。</p>
        </form>

        {tree.truncated && <Alert tone="warning">{DOCS_TRUNCATED_LABEL}</Alert>}
        {nodes.length === 0 ? (
          <EmptyState icon="book" title={DOCS_EMPTY_LABEL} />
        ) : (
          <ul className="space-y-0.5 text-sm">
            {nodes.map((node) => {
              if (hidden(node)) return null;
              if (node.kind === "folder") {
                const open = !collapsed.includes(node.path);
                return (
                  <li key={`folder:${node.path}`} style={{ paddingLeft: `${node.depth * 0.75}rem` }}>
                    <button
                      type="button"
                      onClick={() =>
                        setCollapsed((current) =>
                          current.includes(node.path)
                            ? current.filter((folder) => folder !== node.path)
                            : [...current, node.path],
                        )
                      }
                      data-testid="docs-folder"
                      data-open={open ? "true" : "false"}
                      className="inline-flex min-h-11 items-center gap-1 rounded px-1.5 py-1 font-medium text-fg-muted hover:text-fg"
                    >
                      <Icon name={open ? "chevronDown" : "chevronRight"} />
                      <Icon name="folder" />
                      {node.label}
                    </button>
                  </li>
                );
              }
              const current = node.path === path;
              return (
                <li key={`page:${node.path}`} style={{ paddingLeft: `${node.depth * 0.75 + 0.75}rem` }}>
                  <Link
                    to={docsHref(projectId, { path: node.path, q })}
                    data-testid="docs-page-link"
                    data-current={current ? "true" : "false"}
                    className={cn(
                      "flex min-h-11 items-center rounded px-1.5 py-1 no-underline",
                      current ? "bg-surface-strong font-medium text-primary" : "text-fg hover:bg-surface-strong",
                    )}
                  >
                    {node.label}
                  </Link>
                </li>
              );
            })}
          </ul>
        )}
        <Link
          to={docsHref(projectId, { path: `${tree.root}/new-page.md`, q })}
          className={buttonClass({ variant: "secondary", size: "xs" })}
          data-testid="docs-new-page"
        >
          <Icon name="plus" />
          {DOCS_NEW_PAGE_LABEL}
        </Link>
      </CardBody>
    </Card>
  );
}

/** 右の列（読むとき）: 描画・履歴・編集/削除のボタン。 */
function PageView({
  projectId,
  page,
  q,
  submitting,
  fetcher,
}: {
  projectId: string;
  page: NonNullable<DocsData["page"]>;
  q: string | null;
  submitting: boolean;
  fetcher: ReturnType<typeof useFetcher<DocsOpOutcome>>;
}) {
  const [confirming, setConfirming] = useState(false);
  const body = useMemo(
    () => prepareDocBody(page.raw, projectId, page.root, page.path),
    [page.raw, page.root, page.path, projectId],
  );
  return (
    <Card data-testid="docs-page">
      <CardHeader
        icon="file"
        title={<h2 data-testid="docs-page-title">{page.title}</h2>}
        actions={
          <div className="flex flex-wrap items-center gap-2">
            <Link
              to={`${docsHref(projectId, { path: page.path, q })}${q ? "&" : "?"}edit=1`}
              className={buttonClass({ variant: "secondary", size: "xs" })}
              data-testid="docs-edit"
            >
              <Icon name="code" />
              {DOCS_EDIT_LABEL}
            </Link>
            {confirming ? (
              <fetcher.Form method="post" action={docsHref(projectId)} className="flex items-center gap-2">
                <input type="hidden" name="intent" value="delete" />
                <input type="hidden" name="path" value={page.path} />
                <input type="hidden" name="etag" value={page.etag ?? ""} />
                <Button
                  type="submit"
                  variant="danger"
                  size="xs"
                  disabled={submitting}
                  data-testid="docs-delete-confirm"
                >
                  {DOCS_DELETE_CONFIRM_LABEL}
                </Button>
                <Button type="button" variant="ghost" size="xs" onClick={() => setConfirming(false)}>
                  {DOCS_CANCEL_LABEL}
                </Button>
              </fetcher.Form>
            ) : (
              <Button
                type="button"
                variant="ghost"
                size="xs"
                onClick={() => setConfirming(true)}
                data-testid="docs-delete"
              >
                <Icon name="x" />
                {DOCS_DELETE_LABEL}
              </Button>
            )}
          </div>
        }
      />
      <CardBody className="space-y-4">
        {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
        <div className="flex flex-wrap items-center gap-2 text-sm text-fg-subtle lg:text-xs">
          <Mono data-testid="docs-page-path">{page.path}</Mono>
          {page.tags?.map((tag) => (
            <Badge key={tag} tone="neutral">
              {tag}
            </Badge>
          ))}
          {page.tasks?.map((task) => (
            <Link key={task} to={`/tasks/${task}`} data-testid="docs-page-task" className="no-underline">
              <Badge tone="info">タスク {task.slice(-6)}</Badge>
            </Link>
          ))}
        </div>
        {page.too_large ? <Alert tone="warning">{DOCS_TOO_LARGE_LABEL}</Alert> : <MarkdownViewer content={body} />}
        <DocHistory history={page.history} />
      </CardBody>
    </Card>
  );
}

/** 右の列（書くとき）: テキストとプレビュー。保存は `PUT`（`etag` 付き）。 */
function PageEditor({
  projectId,
  path,
  raw,
  etag,
  q,
  submitting,
  fetcher,
}: {
  projectId: string;
  path: string;
  raw: string;
  etag: string | null;
  q: string | null;
  submitting: boolean;
  fetcher: ReturnType<typeof useFetcher<DocsOpOutcome>>;
}) {
  const [body, setBody] = useState(raw);
  const [target, setTarget] = useState(path);
  return (
    <Card data-testid="docs-editor">
      <CardHeader icon="code" title={<h2>{etag ? "ページを直す" : "ページを作る"}</h2>} />
      <CardBody>
        <fetcher.Form method="post" action={docsHref(projectId)} className="space-y-3">
          <input type="hidden" name="intent" value="save" />
          {etag && <input type="hidden" name="etag" value={etag} />}
          <div className="space-y-1">
            <label className={labelClass} htmlFor="docs-path">
              パス
            </label>
            <input
              id="docs-path"
              name="path"
              className={inputClass}
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              data-testid="docs-editor-path"
            />
            <p className={hintClass}>文書の根からの相対パスでも、リポジトリ相対でも構いません（`.md`）。</p>
          </div>
          <div className="grid gap-3 lg:grid-cols-2">
            <div className="space-y-1">
              <label className={labelClass} htmlFor="docs-body">
                本文（Markdown）
              </label>
              <textarea
                id="docs-body"
                name="body"
                rows={24}
                className={textareaClass}
                value={body}
                onChange={(e) => setBody(e.target.value)}
                data-testid="docs-editor-body"
              />
              <p className={hintClass}>
                題名は 1 行目の <Mono># </Mono>、タスクとの紐付けは front matter の <Mono>tasks: [&lt;id&gt;]</Mono>。
              </p>
            </div>
            <div className="space-y-1">
              <span className={labelClass}>プレビュー（保存すると celeris が描き直します）</span>
              <MarkdownViewer content={prepareDocBody(body, projectId, "", target)} />
            </div>
          </div>
          <div className="space-y-1">
            <label className={labelClass} htmlFor="docs-message">
              コミットメッセージ（任意）
            </label>
            <input id="docs-message" name="message" className={inputClass} data-testid="docs-editor-message" />
          </div>
          <div className="flex items-center gap-2">
            <Button type="submit" variant="primary" size="sm" disabled={submitting} data-testid="docs-save">
              <Icon name="check" />
              {DOCS_SAVE_LABEL}
            </Button>
            <Link
              to={docsHref(projectId, { path: etag ? path : null, q })}
              className={buttonClass({ variant: "ghost", size: "sm" })}
            >
              {DOCS_CANCEL_LABEL}
            </Link>
          </div>
        </fetcher.Form>
      </CardBody>
    </Card>
  );
}

function DocHistory({ history }: { history: DocCommit[] }) {
  if (history.length === 0) return null;
  return (
    <section aria-labelledby="docs-history-heading" className="space-y-2">
      <h3 id="docs-history-heading" className="text-sm font-semibold text-fg">
        {DOCS_HISTORY_LABEL}
      </h3>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <ul className="space-y-1 text-sm text-fg-muted lg:text-xs" data-testid="docs-history">
        {history.map((commit) => (
          <li key={commit.sha} className="flex flex-wrap items-center gap-2">
            <Mono>{shortDocSha(commit.sha)}</Mono>
            <span>{commit.at}</span>
            <span>{commit.author}</span>
            <span className="text-fg">{commit.subject}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const problem = error.data as CelerisRouteErrorData;
    if (problem.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={problem.baseUrl ?? ""} problem={null} />
          <RouteRecovery />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-2xl space-y-3 p-6">
        <h1 className="text-xl font-semibold text-fg">
          {problem.status === 404 ? "案件がありません" : `エラー ${problem.status}`}
        </h1>
        <Alert tone="danger">{problem.detail}</Alert>
        {isTransientStatus(problem.status) && <RouteRecovery />}
      </main>
    );
  }
  return (
    <main className="mx-auto max-w-2xl space-y-3 p-6">
      <h1 className="text-xl font-semibold text-fg">エラー</h1>
      <Alert tone="danger">予期しないエラーが起きました。</Alert>
      <RouteRecovery />
    </main>
  );
}
