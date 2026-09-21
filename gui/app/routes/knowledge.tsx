import { useMemo, useState } from "react";
import { data, isRouteErrorResponse, Link, useFetcher } from "react-router";
import type { KnowledgeOpOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { type KnowledgeData, loadKnowledge, readKnowledgeQuery } from "~/celeris/knowledge";
import { putKnowledgePage, readKnowledgePagePutBody } from "~/celeris/knowledge-admin.server";
import type { DocCommit, KnowledgeItem } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { KnowledgeMeta } from "~/components/KnowledgeMeta";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Badge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, selectClass, textareaClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono, PageHeader } from "~/components/ui/misc";
import { shortDocSha } from "~/lib/docs";
import {
  knowledgeGroups,
  knowledgeHref,
  knowledgeInboxHref,
  knowledgePathProblem,
  parseKnowledgeFrontMatter,
  prepareKnowledgeBody,
} from "~/lib/knowledge";
import {
  KNOWLEDGE_CANCEL_LABEL,
  KNOWLEDGE_EDIT_LABEL,
  KNOWLEDGE_EMPTY_LABEL,
  KNOWLEDGE_HISTORY_LABEL,
  KNOWLEDGE_INBOX_LABEL,
  KNOWLEDGE_NEW_PAGE_LABEL,
  KNOWLEDGE_RELOAD_LABEL,
  KNOWLEDGE_SAVE_LABEL,
  KNOWLEDGE_SCOPE_ALL_LABEL,
  KNOWLEDGE_SCOPE_LABEL,
  KNOWLEDGE_SEARCH_LABEL,
  KNOWLEDGE_SEARCH_RESULT_LABEL,
  KNOWLEDGE_SECTION_DESCRIPTION,
  KNOWLEDGE_TOO_LARGE_LABEL,
  KNOWLEDGE_TRUNCATED_LABEL,
  KNOWLEDGE_UNINITIALIZED_HINT,
  KNOWLEDGE_UNINITIALIZED_TITLE,
  knowledgeErrorHint,
} from "~/lib/labels";
import { cn } from "~/lib/utils";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/knowledge";

/**
 * `/knowledge`（「知識」画面。ADR-0047 D5、docs/celeris-api-v1.md §3.98〜3.103。Phase 61 / G21）。
 *
 * **正本は `[knowledge] root` の Markdown**（celeris が作業ツリーのファイルを読み書きする）。GUI は
 * 知識を持たず、描画に使う `html` も celeris が返すが、`dangerouslySetInnerHTML` は使わない規律
 * （gui/CLAUDE.md）に従って画面では `raw` を `react-markdown` で描く（生 HTML は文字として出る）。
 * `celeris:task/<id>` と `[[相対パス.md]]` は `~/lib/knowledge.ts` の純粋関数でリンクに開く。
 *
 * 用意するボタンは**出さない**（init のエンドポイントは無い。`celerisctl knowledge init` だけ）。
 * 保存は**管理系**で、衝突（409 `etag_mismatch`）は celeris が決めたものをそのまま出す。
 */
export async function loadKnowledgePage(client: CelerisClient, request: Request): Promise<KnowledgeData> {
  return loadKnowledge(client, readKnowledgeQuery(request), request.signal);
}

export async function loader({ request }: Route.LoaderArgs): Promise<KnowledgeData> {
  try {
    return await loadKnowledgePage(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  if (intent !== "save") throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  const outcome = await putKnowledgePage(getCelerisClient(), readKnowledgePagePutBody(form), request.signal);
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "知識 - Celeris" }];
}

export default function KnowledgePageRoute({ loaderData }: Route.ComponentProps) {
  const { tree, page, treeError, pageError, path, q, scope, edit } = loaderData;
  const fetcher = useFetcher<KnowledgeOpOutcome>({ key: "knowledge" });
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : null;

  return (
    <div className="space-y-6" data-testid="knowledge">
      <PageHeader
        icon="database"
        title="知識"
        description={KNOWLEDGE_SECTION_DESCRIPTION}
        actions={
          tree ? (
            // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
            <span className="flex items-center gap-2 text-sm text-fg-subtle lg:text-xs">
              <Mono data-testid="knowledge-root" className="break-all">
                {tree.root}
              </Mono>
              <Link to={knowledgeInboxHref()} className={buttonClass({ variant: "secondary", size: "xs" })}>
                <Icon name="inbox" />
                {KNOWLEDGE_INBOX_LABEL}
                {tree.inbox_count > 0 && (
                  <Badge tone="warning" data-testid="knowledge-inbox-count">
                    {tree.inbox_count}
                  </Badge>
                )}
              </Link>
              {/* ADR-0056 D3 続き（Phase 82 / G35）: skills は `_inbox`/`_retired` と同じ KB の専用ディレクトリ
                  （`skills/`）だが、知識ページの索引には出ないので別の兄弟ルートに分ける（`knowledge.inbox.tsx`
                  と同じ形）。 */}
              <Link
                to="/knowledge/skills"
                className={buttonClass({ variant: "secondary", size: "xs" })}
                data-testid="knowledge-skills-nav"
              >
                <Icon name="sparkles" />
                skills
              </Link>
            </span>
          ) : null
        }
      />

      {error && (
        <div data-testid="knowledge-error">
          <ErrorFlash error={error} />
          {knowledgeErrorHint(error.code) && (
            <Alert tone="warning" data-testid="knowledge-error-hint">
              <p>{knowledgeErrorHint(error.code)}</p>
              {error.code === "etag_mismatch" && path && (
                <Link
                  to={knowledgeHref({ path, q, scope })}
                  className={buttonClass({ variant: "secondary", size: "xs" })}
                  data-testid="knowledge-reload"
                >
                  <Icon name="refresh" />
                  {KNOWLEDGE_RELOAD_LABEL}
                </Link>
              )}
            </Alert>
          )}
        </div>
      )}
      {fetcher.data?.ok && fetcher.data.op === "knowledge_put" && (
        <Alert tone="success" data-testid="knowledge-saved">
          {fetcher.data.result.unchanged ? "変わっていません" : "保存しました"}（<Mono>{fetcher.data.result.path}</Mono>
          ）
        </Alert>
      )}

      {!tree ? (
        <KnowledgeUnavailable error={treeError} />
      ) : !tree.initialized ? (
        <KnowledgeUninitialized root={tree.root} />
      ) : (
        <div className="grid gap-4 lg:grid-cols-[minmax(16rem,22rem)_1fr]">
          <KnowledgeSidebar tree={tree} path={path} q={q} scope={scope} />
          <div className="space-y-4">
            {pageError && (
              <div data-testid="knowledge-page-error">
                <ErrorFlash error={pageError} />
              </div>
            )}
            {edit || (path && !page && !pageError) ? (
              <PageEditor
                path={path ?? ""}
                raw={page?.raw ?? ""}
                etag={page?.etag ?? null}
                q={q}
                scope={scope}
                submitting={submitting}
                fetcher={fetcher}
              />
            ) : page ? (
              <PageView page={page} q={q} scope={scope} />
            ) : (
              <EmptyState icon="database" title="左のページを選んでください">
                <Link
                  to={knowledgeHref({ path: "user/new-page.md", q, scope, edit: true })}
                  className={buttonClass({ variant: "secondary", size: "sm" })}
                >
                  <Icon name="plus" />
                  {KNOWLEDGE_NEW_PAGE_LABEL}
                </Link>
              </EmptyState>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/** `[knowledge] root` が設定されていない（409 `knowledge_unavailable`）など、ツリーが読めないとき。 */
function KnowledgeUnavailable({ error }: { error: KnowledgeData["treeError"] }) {
  return (
    <Card data-testid="knowledge-unavailable">
      <CardHeader icon="database" title={<h2>知識ベースの置き場</h2>} />
      <CardBody className="space-y-3">
        {error && <ErrorFlash error={error} />}
        {error && knowledgeErrorHint(error.code) && <Alert tone="warning">{knowledgeErrorHint(error.code)}</Alert>}
      </CardBody>
    </Card>
  );
}

/**
 * `[knowledge] root` はあるがディレクトリがまだ無い（`initialized: false`）。
 * **用意するボタンは出さない**（init の API は無い。ADR-0047 D5 / §3.98）。
 */
function KnowledgeUninitialized({ root }: { root: string }) {
  return (
    <Card data-testid="knowledge-uninitialized">
      <CardHeader icon="database" title={<h2>{KNOWLEDGE_UNINITIALIZED_TITLE}</h2>} />
      <CardBody className="space-y-3">
        <p className="text-sm text-fg-muted">
          {KNOWLEDGE_UNINITIALIZED_TITLE}。<Mono>celerisctl knowledge init</Mono> で用意してください。
        </p>
        <p className="text-sm text-fg-muted">
          置き場（<Mono data-testid="knowledge-uninitialized-root">{root}</Mono>）は celeris の{" "}
          <Mono>[knowledge] root</Mono> が決めています。GUI からは作れません。
        </p>
        <p className={hintClass}>{KNOWLEDGE_UNINITIALIZED_HINT}</p>
      </CardBody>
    </Card>
  );
}

/** 左の列: 検索・置き場の絞り込みと、置き場ごとに束ねたページ（`?q=` のときは celeris の順位のまま平らに出す）。 */
function KnowledgeSidebar({
  tree,
  path,
  q,
  scope,
}: {
  tree: NonNullable<KnowledgeData["tree"]>;
  path: string | null;
  q: string | null;
  scope: string | null;
}) {
  const groups = useMemo(() => knowledgeGroups(tree.items, tree.scopes ?? []), [tree.items, tree.scopes]);
  const [collapsed, setCollapsed] = useState<string[]>([]);

  return (
    <Card data-testid="knowledge-tree">
      <CardHeader icon="search" title={<h2 className="text-sm">ページ</h2>} />
      <CardBody className="space-y-3">
        <form method="get" action="/knowledge" className="space-y-2">
          <div className="space-y-1">
            <label className={labelClass} htmlFor="knowledge-q">
              {KNOWLEDGE_SEARCH_LABEL}
            </label>
            <div className="flex gap-2">
              <input
                id="knowledge-q"
                name="q"
                type="search"
                defaultValue={q ?? ""}
                className={inputClass}
                data-testid="knowledge-search"
              />
              <Button type="submit" variant="secondary" size="sm">
                <Icon name="search" />
                検索
              </Button>
            </div>
          </div>
          <div className="space-y-1">
            <label className={labelClass} htmlFor="knowledge-scope">
              {KNOWLEDGE_SCOPE_LABEL}
            </label>
            <select
              id="knowledge-scope"
              name="scope"
              defaultValue={scope ?? ""}
              className={selectClass}
              data-testid="knowledge-scope"
            >
              <option value="">{KNOWLEDGE_SCOPE_ALL_LABEL}</option>
              {(tree.scopes ?? []).map((item) => (
                <option key={item} value={item}>
                  {item}
                </option>
              ))}
            </select>
          </div>
          {path && <input type="hidden" name="path" value={path} />}
          <p className={hintClass}>タグ・題名・本文をそのまま探します（並べ替えは celeris が決めます）。</p>
        </form>

        {tree.truncated && <Alert tone="warning">{KNOWLEDGE_TRUNCATED_LABEL}</Alert>}
        {q && tree.items.length > 0 && <p className={hintClass}>{KNOWLEDGE_SEARCH_RESULT_LABEL}</p>}
        {tree.items.length === 0 ? (
          <EmptyState icon="database" title={KNOWLEDGE_EMPTY_LABEL} />
        ) : q ? (
          // フェーズ 72（ADR-0055 D2 ラウンド 4）: 検索結果は「一覧はカード（縦積み）」の規律に揃えた
          // （ツリーの下の並びは今までどおり軽いナビ行のまま。検索結果だけ枠付きのカードにする）。
          <ul className="space-y-2 text-sm" data-testid="knowledge-results">
            {tree.items.map((item) => (
              <li key={item.path}>
                <PageLink item={item} current={item.path === path} q={q} scope={scope} card />
              </li>
            ))}
          </ul>
        ) : (
          <ul className="space-y-2 text-sm">
            {groups.map((group) => {
              const open = !collapsed.includes(group.scope);
              return (
                <li key={`scope:${group.scope}`}>
                  <button
                    type="button"
                    onClick={() =>
                      setCollapsed((current) =>
                        current.includes(group.scope)
                          ? current.filter((item) => item !== group.scope)
                          : [...current, group.scope],
                      )
                    }
                    data-testid="knowledge-scope-folder"
                    data-open={open ? "true" : "false"}
                    className="inline-flex min-h-11 items-center gap-1 rounded px-1.5 py-1 font-medium text-fg-muted hover:text-fg"
                  >
                    <Icon name={open ? "chevronDown" : "chevronRight"} />
                    <Icon name="folder" />
                    {group.label}
                    {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
                    <span className="text-sm text-fg-subtle lg:text-xs">{group.items.length}</span>
                  </button>
                  {open && (
                    <ul className="space-y-0.5 pl-4">
                      {group.items.map((item) => (
                        <li key={item.path}>
                          <PageLink item={item} current={item.path === path} q={q} scope={scope} />
                        </li>
                      ))}
                    </ul>
                  )}
                </li>
              );
            })}
          </ul>
        )}
        <Link
          to={knowledgeHref({ path: `${scope || "user"}/new-page.md`, q, scope, edit: true })}
          className={buttonClass({ variant: "secondary", size: "xs" })}
          data-testid="knowledge-new-page"
        >
          <Icon name="plus" />
          {KNOWLEDGE_NEW_PAGE_LABEL}
        </Link>
      </CardBody>
    </Card>
  );
}

function PageLink({
  item,
  current,
  q,
  scope,
  card = false,
}: {
  item: KnowledgeItem;
  current: boolean;
  q: string | null;
  scope: string | null;
  /** フェーズ 72（ADR-0055 D2）: 検索結果はカード（枠付き）、ツリーの並びは軽いナビ行のまま。 */
  card?: boolean;
}) {
  return (
    <Link
      to={knowledgeHref({ path: item.path, q, scope })}
      data-testid="knowledge-page-link"
      data-current={current ? "true" : "false"}
      className={cn(
        "block min-h-11 no-underline",
        card
          ? cn(
              "rounded-lg border p-3",
              current
                ? "border-primary-border bg-primary-soft text-primary-soft-fg"
                : "border-border bg-surface text-fg hover:border-border-strong",
            )
          : cn("rounded px-1.5 py-1", current ? "bg-surface-2 font-medium text-primary" : "text-fg hover:bg-surface-2"),
      )}
    >
      <span className="block truncate font-medium">{item.title || item.path}</span>
      {card && (
        <span className="mt-0.5 block truncate font-mono text-xs break-all text-fg-subtle" title={item.path}>
          {item.path}
        </span>
      )}
      {(item.tags ?? []).length > 0 && (
        <span className="mt-1 flex flex-wrap gap-1">
          {(item.tags ?? []).map((tag) => (
            <Badge key={tag} tone="neutral">
              {tag}
            </Badge>
          ))}
        </span>
      )}
    </Link>
  );
}

/** 右の列（読むとき）: 見出し・メタ情報・本文・履歴。 */
function PageView({
  page,
  q,
  scope,
}: {
  page: NonNullable<KnowledgeData["page"]>;
  q: string | null;
  scope: string | null;
}) {
  const body = useMemo(() => prepareKnowledgeBody(page.raw, page.path), [page.raw, page.path]);
  return (
    <Card data-testid="knowledge-page">
      <CardHeader
        icon="file"
        title={<h2 data-testid="knowledge-page-title">{page.title}</h2>}
        actions={
          <Link
            to={knowledgeHref({ path: page.path, q, scope, edit: true })}
            className={buttonClass({ variant: "secondary", size: "xs" })}
            data-testid="knowledge-edit"
          >
            <Icon name="code" />
            {KNOWLEDGE_EDIT_LABEL}
          </Link>
        }
      />
      <CardBody className="space-y-4">
        <KnowledgeMeta
          path={page.path}
          scope={page.scope}
          tags={page.tags}
          sources={page.sources}
          confidence={page.confidence}
          updated={page.updated}
        />
        {page.too_large ? <Alert tone="warning">{KNOWLEDGE_TOO_LARGE_LABEL}</Alert> : <MarkdownViewer content={body} />}
        <KnowledgeHistory history={page.history} />
      </CardBody>
    </Card>
  );
}

/** 右の列（書くとき）: テキストとプレビュー。保存は `PUT`（`etag` 付き）。 */
function PageEditor({
  path,
  raw,
  etag,
  q,
  scope,
  submitting,
  fetcher,
}: {
  path: string;
  raw: string;
  etag: string | null;
  q: string | null;
  scope: string | null;
  submitting: boolean;
  fetcher: ReturnType<typeof useFetcher<KnowledgeOpOutcome>>;
}) {
  const [body, setBody] = useState(raw);
  const [target, setTarget] = useState(path);
  // 送る前の検査（celeris の 403 / 422 を先に見せるだけ。判定の正本は celeris）。
  const pathProblem = knowledgePathProblem(target);
  const front = useMemo(() => parseKnowledgeFrontMatter(body), [body]);
  return (
    <Card data-testid="knowledge-editor">
      <CardHeader icon="code" title={<h2>{etag ? "ページを直す" : "ページを作る"}</h2>} />
      <CardBody>
        <fetcher.Form method="post" action="/knowledge" className="space-y-3">
          <input type="hidden" name="intent" value="save" />
          {etag && <input type="hidden" name="etag" value={etag} />}
          <div className="space-y-1">
            <label className={labelClass} htmlFor="knowledge-path">
              パス
            </label>
            <input
              id="knowledge-path"
              name="path"
              className={inputClass}
              value={target}
              onChange={(e) => setTarget(e.target.value)}
              aria-invalid={pathProblem ? true : undefined}
              data-testid="knowledge-editor-path"
            />
            {pathProblem ? (
              <p className="text-sm text-danger lg:text-xs" data-testid="knowledge-path-problem">
                {pathProblem}
              </p>
            ) : (
              <p className={hintClass}>知識ベースの根からの相対パス（`.md`）。例: `environment/clusters/pegasus.md`</p>
            )}
          </div>
          <div className="grid gap-3 lg:grid-cols-2">
            <div className="space-y-1">
              <label className={labelClass} htmlFor="knowledge-body">
                本文（Markdown。front matter を含む）
              </label>
              <textarea
                id="knowledge-body"
                name="body"
                rows={24}
                className={textareaClass}
                value={body}
                onChange={(e) => setBody(e.target.value)}
                data-testid="knowledge-editor-body"
              />
              <p className={hintClass}>
                front matter に <Mono>title</Mono> / <Mono>tags</Mono> / <Mono>scope</Mono> / <Mono>sources</Mono> /{" "}
                <Mono>confidence</Mono> を書きます（読み直すのは celeris です）。
              </p>
            </div>
            <div className="space-y-1">
              <span className={labelClass}>プレビュー（保存すると celeris が読み直します）</span>
              <div className="rounded-lg border border-border bg-surface-2 p-3" data-testid="knowledge-front-matter">
                <KnowledgeMeta
                  scope={front.scope}
                  tags={front.tags}
                  sources={front.sources}
                  confidence={front.confidence}
                  updated={front.updated}
                />
                {front.title && (
                  <p className="mt-1 text-sm font-medium text-fg" data-testid="knowledge-front-title">
                    {front.title}
                  </p>
                )}
              </div>
              <MarkdownViewer content={prepareKnowledgeBody(body, target)} />
            </div>
          </div>
          <div className="space-y-1">
            <label className={labelClass} htmlFor="knowledge-message">
              コミットメッセージ（任意）
            </label>
            <input
              id="knowledge-message"
              name="message"
              className={inputClass}
              data-testid="knowledge-editor-message"
            />
          </div>
          <div className="flex items-center gap-2">
            <Button
              type="submit"
              variant="primary"
              size="sm"
              disabled={submitting || pathProblem !== null}
              data-testid="knowledge-save"
            >
              <Icon name="check" />
              {KNOWLEDGE_SAVE_LABEL}
            </Button>
            <Link
              to={knowledgeHref({ path: etag ? path : null, q, scope })}
              className={buttonClass({ variant: "ghost", size: "sm" })}
            >
              {KNOWLEDGE_CANCEL_LABEL}
            </Link>
          </div>
        </fetcher.Form>
      </CardBody>
    </Card>
  );
}

function KnowledgeHistory({ history }: { history: DocCommit[] }) {
  if (history.length === 0) return null;
  return (
    <section aria-labelledby="knowledge-history-heading" className="space-y-2">
      <h3 id="knowledge-history-heading" className="text-sm font-semibold text-fg">
        {KNOWLEDGE_HISTORY_LABEL}
      </h3>
      <ul className="space-y-1 text-sm text-fg-muted lg:text-xs" data-testid="knowledge-history">
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
