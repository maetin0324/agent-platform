import { useState } from "react";
import { data, isRouteErrorResponse, Link, useFetcher } from "react-router";
import type { SkillOpOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { loadSkills, readSkillsQuery, type SkillsData } from "~/celeris/skills";
import { deleteSkill, putSkill, readSkillName, readSkillPutBody } from "~/celeris/skills-admin.server";
import type { SkillSummaryView } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Badge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, textareaClass, touchLinkClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono, PageHeader } from "~/components/ui/misc";
import {
  isValidSkillName,
  skillMarkdownBody,
  skillMarkdownProblem,
  skillMarkdownTemplate,
  skillsHref,
} from "~/lib/skills";
import { cn } from "~/lib/utils";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/knowledge.skills";

/**
 * `/knowledge/skills`（skill の一覧・閲覧・作成・更新・削除。ADR-0056 D3 続き、
 * docs/celeris-api-v1.md §3.112〜3.117。Phase 82 / G35）。
 *
 * **正本は `[knowledge] root` の `skills/<name>/SKILL.md`**（celeris が KB のファイルを読み書きする。
 * `~/routes/knowledge.tsx` と同じ流儀）。ここでは skill を**作る・書き換える・消す**だけで、**mount する
 * 先（どのノードに効かせるか）は `/org` 画面**（ADR-0056 D3「mount が門」）。mount している組織ノードは
 * `mounted_by` として読み取り専用で見せるだけ（ここから外せない。外すのは `/org` の担当詳細）。
 */
export async function loadKnowledgeSkillsPage(client: CelerisClient, request: Request): Promise<SkillsData> {
  return loadSkills(client, readSkillsQuery(request), request.signal);
}

export async function loader({ request }: Route.LoaderArgs): Promise<SkillsData> {
  try {
    return await loadKnowledgeSkillsPage(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  const client = getCelerisClient();

  let outcome: SkillOpOutcome;
  switch (intent) {
    case "skill_put": {
      const name = readSkillName(form);
      outcome = await putSkill(client, name, readSkillPutBody(form), request.signal);
      break;
    }
    case "skill_delete": {
      const name = readSkillName(form);
      outcome = await deleteSkill(client, name, request.signal);
      break;
    }
    default:
      throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "skills - Celeris" }];
}

export default function KnowledgeSkillsRoute({ loaderData }: Route.ComponentProps) {
  const { list, detail, listError, detailError, name, edit, create } = loaderData;
  const fetcher = useFetcher<SkillOpOutcome>({ key: "skills" });
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : null;

  // 削除に成功したら選択を外す（`fetcher.data` は次の loader の再検証まで残るので、消えた skill の
  // 詳細を出し続けないように `name` が一致するときだけ「消しました」を出し、下の一覧に戻す導線にする）。
  const deleted = fetcher.data?.ok && fetcher.data.op === "skill_delete" ? fetcher.data.name : null;

  return (
    <div className="space-y-6" data-testid="knowledge-skills">
      <Link
        to="/knowledge"
        className="inline-flex min-h-11 items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg"
      >
        <Icon name="arrowLeft" />← 知識
      </Link>

      <PageHeader
        icon="sparkles"
        title="skills"
        description="ワーカーに注入できる手順書（SKILL.md）です。ここで作って、どのノードで効かせるかは「組織」画面の担当詳細で mount します。"
        actions={
          list ? (
            <Link
              to={skillsHref({ create: true })}
              className={buttonClass({ variant: "secondary", size: "xs" })}
              data-testid="skills-new"
            >
              <Icon name="plus" />
              新しい skill
            </Link>
          ) : null
        }
      />

      {error && (
        <div data-testid="skills-error">
          <ErrorFlash error={error} />
          {error.code === "skill_mounted" && (
            <Alert tone="warning" data-testid="skills-error-hint">
              <p>
                先に「組織」画面でその skill を mount しているノードから外してから消してください。 （
                <Mono>{error.detail}</Mono>）
              </p>
            </Alert>
          )}
        </div>
      )}
      {fetcher.data?.ok && fetcher.data.op === "skill_put" && (
        <Alert tone="success" data-testid="skills-saved">
          保存しました（<Mono>{fetcher.data.result.path}</Mono>）
        </Alert>
      )}
      {deleted && (
        <Alert tone="success" data-testid="skills-deleted">
          消しました（<Mono>{deleted}</Mono>）
        </Alert>
      )}

      {!list ? (
        <SkillsUnavailable error={listError} />
      ) : (
        <div className="grid gap-4 lg:grid-cols-[minmax(16rem,22rem)_1fr]">
          <SkillsSidebar items={list.items} name={name} />
          <div className="space-y-4">
            {detailError && (
              <div data-testid="skills-detail-error">
                <ErrorFlash error={detailError} />
              </div>
            )}
            {create || (edit && name) ? (
              <SkillEditor
                initialName={create ? "" : (name ?? "")}
                skillMd={create ? skillMarkdownTemplate("") : (detail?.skill_md ?? "")}
                creating={create}
                submitting={submitting}
                fetcher={fetcher}
              />
            ) : detail ? (
              <SkillView detail={detail} submitting={submitting} fetcher={fetcher} />
            ) : (
              <EmptyState icon="sparkles" title="左の skill を選んでください">
                <Link to={skillsHref({ create: true })} className={buttonClass({ variant: "secondary", size: "sm" })}>
                  <Icon name="plus" />
                  新しい skill
                </Link>
              </EmptyState>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/** `[knowledge] root` が設定されていない（409 `knowledge_unavailable`）など、一覧が読めないとき。 */
function SkillsUnavailable({ error }: { error: SkillsData["listError"] }) {
  return (
    <Card data-testid="skills-unavailable">
      <CardHeader icon="sparkles" title={<h2>skills の置き場</h2>} />
      <CardBody className="space-y-3">
        {error && <ErrorFlash error={error} />}
        {error?.code === "knowledge_unavailable" && (
          <Alert tone="warning">
            celeris の <Mono>[knowledge] root</Mono> が設定されていません。設定すると skills もここに置けます。
          </Alert>
        )}
      </CardBody>
    </Card>
  );
}

/** 左の列: skill をカードで並べる。 */
function SkillsSidebar({ items, name }: { items: SkillSummaryView[]; name: string | null }) {
  return (
    <Card data-testid="skills-list">
      <CardHeader icon="list" title={<h2 className="text-sm">一覧</h2>} />
      <CardBody className="space-y-2">
        {items.length === 0 ? (
          <EmptyState icon="sparkles" title="まだ skill がありません">
            <Link
              to={skillsHref({ create: true })}
              className={buttonClass({ variant: "secondary", size: "sm" })}
              data-testid="skills-new-empty"
            >
              <Icon name="plus" />
              新しい skill
            </Link>
          </EmptyState>
        ) : (
          <ul className="space-y-2 text-sm" data-testid="skills-cards">
            {items.map((item) => (
              <li key={item.name}>
                <Link
                  to={skillsHref({ name: item.name })}
                  data-testid="skill-card"
                  data-current={item.name === name ? "true" : "false"}
                  className={cn(
                    "block min-h-11 rounded-lg border p-3 no-underline",
                    item.name === name
                      ? "border-primary-border bg-primary-soft text-primary-soft-fg"
                      : "border-border bg-surface text-fg hover:border-border-strong",
                  )}
                >
                  <span className="block truncate font-medium">{item.name}</span>
                  {item.description && (
                    <span className="mt-0.5 block truncate text-sm text-fg-subtle lg:text-xs">{item.description}</span>
                  )}
                  <span className="mt-1.5 flex flex-wrap items-center gap-1">
                    {(item.mounted_by ?? []).length > 0 ? (
                      (item.mounted_by ?? []).map((nodeId) => (
                        <Badge key={nodeId} tone="teal" data-testid="skill-mounted-by-chip">
                          {nodeId}
                        </Badge>
                      ))
                    ) : (
                      <span className="text-sm text-fg-subtle lg:text-xs">mount 無し</span>
                    )}
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </CardBody>
    </Card>
  );
}

/** 右の列（読むとき）: SKILL.md・付属ファイル・mount 先（読み取り専用）・削除。 */
function SkillView({
  detail,
  submitting,
  fetcher,
}: {
  detail: NonNullable<SkillsData["detail"]>;
  submitting: boolean;
  fetcher: ReturnType<typeof useFetcher<SkillOpOutcome>>;
}) {
  const mountedBy = detail.mounted_by ?? [];
  return (
    <Card data-testid="skill-detail">
      <CardHeader
        icon="sparkles"
        title={<h2 data-testid="skill-detail-name">{detail.name}</h2>}
        actions={
          <Link
            to={skillsHref({ name: detail.name, edit: true })}
            className={buttonClass({ variant: "secondary", size: "xs" })}
            data-testid="skill-edit"
          >
            <Icon name="code" />
            直す
          </Link>
        }
      />
      <CardBody className="space-y-4">
        {detail.updated && (
          <p className="text-sm text-fg-subtle lg:text-xs">
            最終更新: <Mono>{detail.updated}</Mono>
          </p>
        )}

        <div data-testid="skill-mounted-by">
          <p className={labelClass}>mount しているノード</p>
          {mountedBy.length === 0 ? (
            <p className={cn(hintClass, "mt-1")}>まだどこにも mount されていません。</p>
          ) : (
            <div className="mt-1.5 flex flex-wrap gap-1.5">
              {mountedBy.map((nodeId) => (
                // ADR-0055 D1-2: タップ領域 44×44 以上（バッジ自体は小さいので、リンクの当たり判定を広げる）。
                <Link
                  key={nodeId}
                  to={`/org?selected=${encodeURIComponent(nodeId)}`}
                  className="inline-flex min-h-11 items-center"
                >
                  <Badge tone="teal">{nodeId}</Badge>
                </Link>
              ))}
            </div>
          )}
          <p className={cn(hintClass, "mt-1")}>
            mount する・外すのは「組織」画面の担当詳細から（
            <Link to="/org" className={cn(touchLinkClass, "underline underline-offset-2")}>
              組織へ
            </Link>
            ）。
          </p>
        </div>

        {(detail.files ?? []).length > 0 && (
          <div data-testid="skill-files">
            <p className={labelClass}>付属ファイル</p>
            <ul className="mt-1.5 space-y-0.5">
              {(detail.files ?? []).map((file) => (
                <li key={file} className="font-mono text-xs break-all text-fg-subtle">
                  {file}
                </li>
              ))}
            </ul>
          </div>
        )}

        <div>
          <p className={labelClass}>SKILL.md</p>
          <div className="mt-1.5 rounded-lg border border-border bg-surface-2/40 p-3">
            <MarkdownViewer content={skillMarkdownBody(detail.skill_md)} />
          </div>
        </div>

        <details className="group">
          <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-danger-border bg-danger-soft px-3 text-sm text-danger-soft-fg shadow-xs hover:bg-danger hover:text-white">
            <Icon name="xCircle" className="size-4" />
            削除
          </summary>
          <fetcher.Form method="post" className="mt-3 rounded-lg border border-danger-border bg-danger-soft/40 p-3">
            <input type="hidden" name="intent" value="skill_delete" />
            <input type="hidden" name="name" value={detail.name} />
            <p className="mb-2 text-sm text-fg-muted">
              本当に「{detail.name}」を削除しますか？
              {mountedBy.length > 0 && " どこかのノードに mount されている間は消せません。"}
            </p>
            <Button type="submit" variant="danger" size="sm" disabled={submitting} data-testid="skill-delete-submit">
              <Icon name="xCircle" />
              削除する
            </Button>
          </fetcher.Form>
        </details>
      </CardBody>
    </Card>
  );
}

/** 右の列（作る・書くとき）: 名前・本文・付属ファイル。保存は `PUT /skills/{name}`。 */
function SkillEditor({
  initialName,
  skillMd,
  creating,
  submitting,
  fetcher,
}: {
  initialName: string;
  skillMd: string;
  creating: boolean;
  submitting: boolean;
  fetcher: ReturnType<typeof useFetcher<SkillOpOutcome>>;
}) {
  const [name, setName] = useState(initialName);
  const [body, setBody] = useState(skillMd);
  // 名前欄が空のまま（作成フォームでまだ何も入れていない）ときは、frontmatter の検証エラーをまだ出さない。
  const showProblem = name.trim() !== "" ? skillMarkdownProblem(name.trim(), body) : null;

  return (
    <Card data-testid="skill-editor">
      <CardHeader icon="code" title={<h2>{creating ? "skill を作る" : `${initialName} を直す`}</h2>} />
      <CardBody>
        <fetcher.Form method="post" action="/knowledge/skills" className="space-y-3">
          <input type="hidden" name="intent" value="skill_put" />
          <div className="space-y-1">
            <label className={labelClass} htmlFor="skill-name">
              名前
            </label>
            <input
              id="skill-name"
              name="name"
              className={inputClass}
              value={name}
              disabled={!creating}
              onChange={(e) => setName(e.target.value)}
              aria-invalid={creating && name.trim() !== "" && !isValidSkillName(name.trim()) ? true : undefined}
              data-testid="skill-editor-name"
            />
            <p className={hintClass}>
              英小文字・数字・ハイフンだけ（1〜64 文字）。frontmatter の <Mono>name:</Mono> と一致させます。
            </p>
          </div>
          <div className="space-y-1">
            <label className={labelClass} htmlFor="skill-md">
              SKILL.md（frontmatter を含む）
            </label>
            <textarea
              id="skill-md"
              name="skill_md"
              rows={20}
              className={textareaClass}
              value={body}
              onChange={(e) => setBody(e.target.value)}
              data-testid="skill-editor-body"
            />
            <p className={hintClass}>
              先頭に <Mono>---</Mono> で囲った frontmatter（<Mono>name</Mono> / <Mono>description</Mono> 必須）。
            </p>
          </div>
          {showProblem && (
            <p className="text-sm text-danger lg:text-xs" data-testid="skill-editor-problem">
              {showProblem}
            </p>
          )}
          <div className="flex items-center gap-2">
            <Button
              type="submit"
              variant="primary"
              size="sm"
              disabled={submitting || name.trim() === "" || showProblem !== null}
              data-testid="skill-save"
            >
              <Icon name="check" />
              保存
            </Button>
            <Link
              to={initialName ? skillsHref({ name: initialName }) : skillsHref()}
              className={buttonClass({ variant: "ghost", size: "sm" })}
            >
              キャンセル
            </Link>
          </div>
        </fetcher.Form>
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
