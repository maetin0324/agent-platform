import { Link } from "react-router";
import type { ActionError } from "~/celeris/action-types";
import type { TreeFileView, TreeView } from "~/celeris/types";
import { CodeViewer } from "~/components/CodeViewer";
import { ErrorFlash } from "~/components/Flash";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Badge } from "~/components/ui/badge";
import { buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { touchLinkClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, Mono } from "~/components/ui/misc";
import { fileSizeLabel, repoKindLabel, treeEntryKindLabel } from "~/lib/labels";
import { fileBody, isJsonPath, parentPath, pickTreeFileViewer, taskFilesHref, treeBreadcrumbs } from "~/lib/task-files";
import { cn } from "~/lib/utils";

/**
 * タスクの作業ツリーの閲覧（ADR-0043 D6、docs/celeris-api-v1.md §3.72〜3.73。Phase 52 / G16）。
 * リポジトリの選択・パンくず・一覧・選んだファイルの本文を 1 つにまとめた**自己完結の部品**で、
 * どこに載せても 1 行で済むようにしてある（いまは `~/routes/tasks.$id.files.tsx`。
 * ADR-0044 B1 のタブの殻ができたらそこへ移す）。
 *
 * 移動はすべて `<Link>`（`?repo=&path=&file=`）で、クライアントから celeris を呼ぶコードは無い。
 * 並び（ディレクトリが先、あとは名前順）は celeris が決めたものをそのまま出す。
 * 403 / 404 は celeris の文言をそのまま出す（`ErrorFlash`）。
 */
export interface TaskFilesProps {
  taskId: string;
  tree: TreeView;
  file: TreeFileView | null;
  fileError: ActionError | null;
  filePath: string | null;
}

export function TaskFiles({ taskId, tree, file, fileError, filePath }: TaskFilesProps) {
  const crumbs = treeBreadcrumbs(tree.path);
  const up = parentPath(tree.path);

  return (
    <div className="space-y-4" data-testid="task-files">
      {tree.repos.length > 1 && (
        <div className="flex flex-wrap items-center gap-2" data-testid="task-files-repos">
          {tree.repos.map((repo) => {
            const current = repo.name === tree.repo;
            return (
              <Link
                key={repo.name}
                to={taskFilesHref(taskId, { repo: repo.name })}
                data-testid="task-files-repo"
                data-repo-current={current ? "true" : "false"}
                className={buttonClass({ variant: current ? "soft" : "secondary", size: "sm" })}
              >
                <Icon name="database" />
                {repo.name}
                <Badge tone="neutral">{repoKindLabel(repo.kind)}</Badge>
              </Link>
            );
          })}
        </div>
      )}

      <Card>
        <CardHeader
          icon="folder"
          title={
            <nav
              aria-label="パンくず"
              className="flex flex-wrap items-center gap-1 text-sm"
              data-testid="task-files-breadcrumb"
            >
              <Link
                to={taskFilesHref(taskId, { repo: tree.repo })}
                className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
                data-testid="task-files-crumb"
              >
                {tree.repo}
              </Link>
              {crumbs.map((crumb) => (
                <span key={crumb.path} className="flex items-center gap-1">
                  <span className="text-fg-subtle">/</span>
                  <Link
                    to={taskFilesHref(taskId, { repo: tree.repo, path: crumb.path })}
                    className={cn(touchLinkClass, "font-medium text-primary hover:underline")}
                    data-testid="task-files-crumb"
                  >
                    {crumb.name}
                  </Link>
                </span>
              ))}
            </nav>
          }
          description={
            <span data-testid="task-files-repo-dir" className="break-all font-mono text-xs">
              {tree.repos.find((r) => r.name === tree.repo)?.dir ?? ""}
            </span>
          }
        />
        <CardBody className="space-y-2">
          {up !== null && (
            <Link
              to={taskFilesHref(taskId, { repo: tree.repo, path: up })}
              className="inline-flex min-h-11 items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg"
              data-testid="task-files-up"
            >
              <Icon name="arrowLeft" />
              上へ
            </Link>
          )}
          {tree.entries.length === 0 ? (
            <EmptyState icon="folder" title="このディレクトリは空です" data-testid="task-files-empty" />
          ) : (
            <ul className="divide-y divide-border" data-testid="task-files-entries">
              {tree.entries.map((entry) => (
                <li
                  key={entry.path}
                  className="flex items-center justify-between gap-3 py-1.5"
                  data-testid="task-files-entry"
                  data-entry-kind={entry.kind}
                >
                  {entry.kind === "dir" ? (
                    <Link
                      to={taskFilesHref(taskId, { repo: tree.repo, path: entry.path })}
                      className="flex min-h-11 min-w-0 items-center gap-2 font-medium text-primary hover:underline"
                      data-testid="task-files-entry-link"
                    >
                      <Icon name="folder" />
                      <span className="truncate" title={entry.name}>
                        {entry.name}
                      </span>
                    </Link>
                  ) : entry.kind === "file" ? (
                    <Link
                      to={taskFilesHref(taskId, { repo: tree.repo, path: tree.path, file: entry.path })}
                      className="flex min-h-11 min-w-0 items-center gap-2 font-medium text-primary hover:underline"
                      data-testid="task-files-entry-link"
                    >
                      <Icon name="file" />
                      <span className="truncate" title={entry.name}>
                        {entry.name}
                      </span>
                    </Link>
                  ) : (
                    <span className="flex min-w-0 items-center gap-2 text-fg-muted">
                      <Icon name="file" />
                      <span className="truncate" title={entry.name}>
                        {entry.name}
                      </span>
                    </span>
                  )}
                  {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
                  <span className="shrink-0 text-sm text-fg-subtle tabular-nums lg:text-xs">
                    {entry.kind === "file" && entry.size != null
                      ? fileSizeLabel(entry.size)
                      : treeEntryKindLabel(entry.kind)}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </CardBody>
      </Card>

      {filePath && (
        <Card>
          <CardHeader
            icon="file"
            title={
              <span className="break-all" data-testid="task-files-file-path">
                <Mono className="text-sm">{filePath}</Mono>
              </span>
            }
            description={file ? <span data-testid="task-files-file-size">{fileSizeLabel(file.size)}</span> : undefined}
          />
          <CardBody className="p-0">
            {fileError ? (
              <div className="px-5 py-4">
                <ErrorFlash error={fileError} />
              </div>
            ) : file ? (
              <FileBodyView file={file} />
            ) : null}
          </CardBody>
        </Card>
      )}
    </div>
  );
}

function FileBodyView({ file }: { file: TreeFileView }) {
  const body = fileBody(file);
  if (body.kind !== "text") {
    return (
      <div className="px-5 py-4">
        <Alert tone="warning" data-testid="task-files-file-notice" data-file-notice={body.kind}>
          {body.message}
        </Alert>
      </div>
    );
  }
  if (pickTreeFileViewer(file.path) === "markdown") {
    return (
      <div className="px-5 py-4">
        <MarkdownViewer content={body.text ?? ""} />
      </div>
    );
  }
  return <CodeViewer content={body.text ?? ""} json={isJsonPath(file.path)} />;
}
