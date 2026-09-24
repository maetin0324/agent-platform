import { isRouteErrorResponse, Link } from "react-router";
import { getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { loadTaskFiles, readTaskFilesQuery, type TaskFilesData } from "~/celeris/task-files";
import { RouteRecovery } from "~/components/RouteRecovery";
import { TaskFiles } from "~/components/task-files";
import { Icon } from "~/components/ui/Icon";
import { Alert, PageHeader } from "~/components/ui/misc";
import { isTransientStatus } from "~/lib/recovery";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/tasks.$id.files";

/**
 * `/tasks/:id/files`（タスクの作業ツリーの閲覧。ADR-0043 D6、docs/celeris-api-v1.md §3.72〜3.73。
 * Phase 52 / G16）。`/tasks/:id/runs/:runId` と同じ**兄弟のルート**で、中身は全部
 * `~/components/task-files.tsx`（自己完結の部品）に入れてある。ADR-0044 B1 のタブの殻ができたら
 * そこにこの部品を 1 行で載せ替えられる。
 *
 * 読み取りだけ（`action` は無い）。移動は `?repo=&path=&file=` のリンクで、そのたびに loader が走る。
 */
export async function loader({ params, request }: Route.LoaderArgs): Promise<TaskFilesData> {
  try {
    return await loadTaskFiles(getCelerisClient(), params.id, readTaskFilesQuery(request), request.signal);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "作業ツリー - celeris-gui" }];
}

export default function TaskFilesPage({ loaderData }: Route.ComponentProps) {
  const { taskId, tree, file, fileError, filePath } = loaderData;
  return (
    <div className="space-y-6">
      <Link
        to={`/tasks/${taskId}`}
        className="inline-flex items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg"
      >
        <Icon name="arrowLeft" />← タスク詳細
      </Link>
      <PageHeader
        icon="folder"
        title="作業ツリー"
        description="このタスクが実際に作業している場所です（git は worktree、ディレクトリはシンボリックリンク）。読み取りだけで、ここからは変更できません。"
      />
      <TaskFiles taskId={taskId} tree={tree} file={file} fileError={fileError} filePath={filePath} />
    </div>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as CelerisRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={data.baseUrl ?? ""} problem={null} />
          <RouteRecovery />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-2xl space-y-3 p-6">
        <h1 className="text-xl font-semibold text-fg">
          {data.status === 404 ? "作業ツリーがありません" : `エラー ${data.status}`}
        </h1>
        {/* celeris の文言をそのまま出す（403 `path_forbidden` / 404 `file_not_found`）。 */}
        <Alert tone="danger">{data.detail}</Alert>
        {isTransientStatus(data.status) && <RouteRecovery />}
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
