import { useRef, useState } from "react";
import { data, isRouteErrorResponse, Link, useFetcher } from "react-router";
import type { SkillOpOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { loadSkills, readSkillsQuery, type SkillsData } from "~/celeris/skills";
import { deleteSkill, putSkill, readSkillName, readSkillPutBody } from "~/celeris/skills-admin.server";
import type { SkillSummaryView } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { LocalTime } from "~/components/LocalTime";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { RouteRecovery } from "~/components/RouteRecovery";
import { Badge } from "~/components/ui/badge";
import { Button, buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass, textareaClass, touchLinkClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono, PageHeader } from "~/components/ui/misc";
import { shortId } from "~/lib/format";
import { isTransientStatus } from "~/lib/recovery";
import {
  skillBodyProblem,
  skillFilePathProblem,
  skillMarkdownBody,
  skillMarkdownTemplate,
  skillNameProblem,
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
                // create/名前ごとに state（名前・本文・付属ファイル行）をやり直す（別の skill を編集し
                // 始めたのに前の入力が残る事故を防ぐ）。
                key={create ? "create" : (name ?? "")}
                initialName={create ? "" : (name ?? "")}
                skillMd={create ? skillMarkdownTemplate("") : (detail?.skill_md ?? "")}
                existingFiles={create ? [] : (detail?.files ?? [])}
                creating={create}
                submitting={submitting}
                fetcher={fetcher}
              />
            ) : detail ? (
              <SkillView detail={detail} submitting={submitting} fetcher={fetcher} />
            ) : (
              <EmptyState icon="sparkles" title="skill を選んでください">
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
                        // ADR-0055 D2: id は末尾に省略し、全文は title に残す（`~/lib/format.ts::shortId`）。
                        <Badge key={nodeId} tone="teal" title={nodeId} data-testid="skill-mounted-by-chip">
                          {shortId(nodeId)}
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
          // Phase 95（目視点検の所見）: 生の ISO 8601 文字列がそのまま出ていた。`LocalTime`（ADR-0055
          // ラウンド 14、タイムゾーンに安全な時刻表示）に揃える。`mode="datetime"` は視聴者のタイムゾーンで
          // 絶対日時を出す（`fetchedAtIso` は相対表示専用なのでここでは不要）。
          <p className="text-sm text-fg-subtle lg:text-xs">
            最終更新: <LocalTime iso={detail.updated} mode="datetime" />
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
                // Phase 84: id は末尾に省略し、全文は title と（読み上げ用の）aria-label に残す。
                <Link
                  key={nodeId}
                  to={`/org?selected=${encodeURIComponent(nodeId)}`}
                  className="inline-flex min-h-11 items-center"
                  title={nodeId}
                  aria-label={nodeId}
                >
                  <Badge tone="teal">{shortId(nodeId)}</Badge>
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

/** 付属ファイル 1 行（フォーム上だけの状態。`id` は React の key 用でサーバーには送らない）。 */
interface SkillFileRow {
  id: number;
  path: string;
  content: string;
}

/**
 * 右の列（作る・書くとき）: 名前・本文・付属ファイル。保存は `PUT /skills/{name}`。
 *
 * Phase 84（U-G35-2 の解消）: 付属ファイル（`SkillPutBody.files`）を送れる行入力を足した。API は
 * 送った分だけ書く・触れなかった既存ファイルはそのまま残す作り（`task_ops::knowledge::skills_put`）なので、
 * ここでの「追加/削除」は「送信するファイルの集合をこのフォームの中だけで編集する」意味で、既存の付属
 * ファイル自体を KB から消す機能ではない（消すには同じ名前で空/別内容を送って上書きするか、skill ごと
 * 削除する）。`GET /skills/{name}` はファイルの中身を返さない（索引だけ）ので、既存ファイルは名前だけ
 * 参考情報として出し、内容を勝手に空で埋めて上書きしないようにした（`existingFiles`）。
 */
function SkillEditor({
  initialName,
  skillMd,
  existingFiles,
  creating,
  submitting,
  fetcher,
}: {
  initialName: string;
  skillMd: string;
  existingFiles: string[];
  creating: boolean;
  submitting: boolean;
  fetcher: ReturnType<typeof useFetcher<SkillOpOutcome>>;
}) {
  const [name, setName] = useState(initialName);
  const [body, setBody] = useState(skillMd);
  const [files, setFiles] = useState<SkillFileRow[]>([]);
  const nextFileId = useRef(0);

  // 名前欄が空のまま（作成フォームでまだ何も入れていない）ときは、まだ検証エラーを出さない。
  const trimmedName = name.trim();
  const nameProblem = trimmedName !== "" ? skillNameProblem(trimmedName) : null;
  const bodyProblem = trimmedName !== "" ? skillBodyProblem(trimmedName, body) : null;
  const fileProblems = files.map((f) => skillFilePathProblem(f.path));
  const hasFileProblem = fileProblems.some((p) => p !== null);
  const canSave = !submitting && trimmedName !== "" && nameProblem === null && bodyProblem === null && !hasFileProblem;

  function addFile() {
    const id = nextFileId.current;
    nextFileId.current += 1;
    setFiles((rows) => [...rows, { id, path: "", content: "" }]);
  }
  function removeFile(id: number) {
    setFiles((rows) => rows.filter((r) => r.id !== id));
  }
  function updateFile(id: number, patch: Partial<Pick<SkillFileRow, "path" | "content">>) {
    setFiles((rows) => rows.map((r) => (r.id === id ? { ...r, ...patch } : r)));
  }
  function useTemplate() {
    setBody(skillMarkdownTemplate(trimmedName));
  }

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
              aria-invalid={nameProblem !== null ? true : undefined}
              data-testid="skill-editor-name"
            />
            <p className={hintClass}>
              英小文字・数字・ハイフンだけ（1〜64 文字）。frontmatter の <Mono>name:</Mono> と一致させます。
            </p>
            {nameProblem && (
              <p className="text-sm text-danger lg:text-xs" data-testid="skill-editor-name-problem">
                {nameProblem}
              </p>
            )}
          </div>
          <div className="space-y-1">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <label className={labelClass} htmlFor="skill-md">
                SKILL.md（frontmatter を含む）
              </label>
              <Button type="button" variant="ghost" size="xs" onClick={useTemplate} data-testid="skill-editor-template">
                <Icon name="sparkles" />
                雛形を使う
              </Button>
            </div>
            <textarea
              id="skill-md"
              name="skill_md"
              rows={20}
              className={textareaClass}
              value={body}
              onChange={(e) => setBody(e.target.value)}
              aria-invalid={bodyProblem !== null ? true : undefined}
              data-testid="skill-editor-body"
            />
            <p className={hintClass}>
              先頭に <Mono>---</Mono> で囲った frontmatter（<Mono>name</Mono> / <Mono>description</Mono> 必須）。
            </p>
            {bodyProblem && (
              <p className="text-sm text-danger lg:text-xs" data-testid="skill-editor-body-problem">
                {bodyProblem}
              </p>
            )}
          </div>

          <div className="space-y-2">
            <p className={labelClass}>付属ファイル（任意）</p>
            {existingFiles.length > 0 && (
              <p className={hintClass} data-testid="skill-editor-existing-files">
                既存の付属ファイル: <Mono className="break-all">{existingFiles.join(", ")}</Mono>
                （中身はここには読み込みません。同じパスをここに書くと上書きします。触れなければそのまま残ります）。
              </p>
            )}
            {files.length > 0 && (
              <ul className="space-y-2" data-testid="skill-editor-files">
                {files.map((f, i) => {
                  const problem = fileProblems[i];
                  return (
                    <li
                      key={f.id}
                      className="space-y-1.5 rounded-lg border border-border bg-surface-2/30 p-2"
                      data-testid="skill-editor-file-row"
                    >
                      <div className="flex items-center gap-2">
                        <input
                          type="text"
                          name="file_path"
                          value={f.path}
                          onChange={(e) => updateFile(f.id, { path: e.target.value })}
                          placeholder="例: refs/checklist.md"
                          className={cn(inputClass, "min-w-0 flex-1")}
                          aria-invalid={problem !== null ? true : undefined}
                          aria-label={`付属ファイル ${i + 1} のパス`}
                          data-testid="skill-editor-file-path"
                        />
                        <Button
                          type="button"
                          variant="ghost"
                          size="xs"
                          onClick={() => removeFile(f.id)}
                          aria-label={`付属ファイル ${f.path.trim() || i + 1} を削除`}
                          data-testid="skill-editor-file-remove"
                        >
                          <Icon name="xCircle" />
                        </Button>
                      </div>
                      <textarea
                        name="file_content"
                        value={f.content}
                        onChange={(e) => updateFile(f.id, { content: e.target.value })}
                        rows={4}
                        className={textareaClass}
                        placeholder="ファイルの中身"
                        aria-label={`付属ファイル ${i + 1} の中身`}
                        data-testid="skill-editor-file-content"
                      />
                      {problem && (
                        <p className="text-sm text-danger lg:text-xs" data-testid="skill-editor-file-problem">
                          {problem}
                        </p>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
            <Button type="button" variant="secondary" size="xs" onClick={addFile} data-testid="skill-editor-file-add">
              <Icon name="plus" />
              付属ファイルを追加
            </Button>
          </div>

          <div className="flex items-center gap-2">
            <Button type="submit" variant="primary" size="sm" disabled={!canSave} data-testid="skill-save">
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
          <RouteRecovery />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-2xl space-y-3 p-6">
        <h1 className="text-xl font-semibold text-fg">エラー {problem.status}</h1>
        <Alert tone="danger">{problem.detail}</Alert>
        {isTransientStatus(problem.status) && <RouteRecovery />}
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
