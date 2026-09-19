import { useFetcher } from "react-router";
import { ProjectActionFlash } from "~/components/Flash";
import { RepoFields } from "~/components/RepoFields";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { Icon } from "~/components/ui/Icon";
import { DataItem, EmptyState } from "~/components/ui/misc";
import { PRIMARY_REPO_MARK, repoKindLabel, repoRunLabel, repoSyncLabel, SET_PRIMARY_REPO_LABEL } from "~/lib/labels";
import { repoLocationText, repoPlaceOf } from "~/lib/repo-form";
import type { ProjectOpOutcome } from "~/taskd/action-types";
import type { ClusterView, ProjectRepo } from "~/taskd/types";

/**
 * 案件の「リポジトリ」節（ADR-0043 D1、docs/taskd-api-v1.md §3.68〜3.71。Phase 52 / G16）。
 * 案件は複数のリポジトリを持ち、`is_primary` の 1 件が「主なリポジトリ」で、`Project.workspace` は
 * その `location` の写し（上の「作業場所」カードと同じものを指す）。
 *
 * 操作は 4 つ（追加・変更・主にする・削除）で、どれも `/projects/:id` の `action` の intent に流すだけ。
 * 結果は**行ごとの `useFetcher`** に載せる（SSE の再検証で消えないため。監査 H1 / Phase G14 と同じ作り）。
 * GUI 側では検証しない: 409 `repo_in_use` / 422 `validation` は taskd の文言をそのまま出す。
 */
export interface ProjectReposProps {
  projectId: string;
  repos: readonly ProjectRepo[];
  clusters: readonly ClusterView[];
}

export function ProjectRepos({ projectId, repos, clusters }: ProjectReposProps) {
  const fetcher = useFetcher<ProjectOpOutcome>({ key: `repo-create-${projectId}` });
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : undefined;

  return (
    <div className="space-y-4" data-testid="project-repos">
      {repos.length === 0 ? (
        <EmptyState icon="folder" title="リポジトリがありません" data-testid="project-repos-empty">
          下のフォームで足すと、この案件の仕事はそのリポジトリの作業ツリーで走ります。
        </EmptyState>
      ) : (
        <ul className="space-y-3">
          {repos.map((repo) => (
            <li key={repo.id}>
              <RepoRow repo={repo} clusters={clusters} />
            </li>
          ))}
        </ul>
      )}

      <Card>
        <CardHeader
          icon="plus"
          title="リポジトリを追加"
          description="最初の 1 件は自動的に「主」になります。名前は案件の中で一意の slug で、作業ツリーのディレクトリ名になります。"
        />
        <CardBody className="space-y-3">
          <ProjectActionFlash outcome={fetcher.data} />
          <fetcher.Form method="post" className="space-y-3" data-testid="project-repo-create-form">
            <input type="hidden" name="intent" value="repo_create" />
            <RepoFields idPrefix={`repo-create-${projectId}`} clusters={clusters} error={error} />
            <Button type="submit" variant="secondary" size="sm" disabled={submitting} data-testid="project-repo-create">
              <Icon name="plus" />
              追加
            </Button>
          </fetcher.Form>
        </CardBody>
      </Card>
    </div>
  );
}

function RepoRow({ repo, clusters }: { repo: ProjectRepo; clusters: readonly ClusterView[] }) {
  const fetcher = useFetcher<ProjectOpOutcome>({ key: `repo-${repo.id}` });
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : undefined;
  const isPrimary = repo.is_primary === true;

  return (
    <Card data-testid="project-repo-row" data-repo-id={repo.id} data-repo-primary={isPrimary ? "true" : "false"}>
      <CardHeader
        icon="folder"
        title={
          <span className="flex flex-wrap items-center gap-2">
            <span data-testid="project-repo-name">{repo.name}</span>
            {isPrimary && (
              <Badge tone="primary" data-testid="project-repo-primary-badge">
                {PRIMARY_REPO_MARK}
              </Badge>
            )}
            <Badge tone="neutral" data-testid="project-repo-kind">
              {repoKindLabel(repo.kind)}
            </Badge>
          </span>
        }
        description={
          <span className="break-all font-mono text-xs" data-testid="project-repo-location">
            {repoLocationText(repo.location)}
          </span>
        }
      />
      <CardBody className="space-y-3">
        <div className="grid gap-x-6 gap-y-3 sm:grid-cols-3">
          <DataItem label="実行環境">
            <span data-testid="project-repo-run">{repoRunLabel(repo.run ?? "auto")}</span>
          </DataItem>
          <DataItem label="既定のブランチ">
            <span data-testid="project-repo-default-branch">{repo.default_branch ?? "-"}</span>
          </DataItem>
          <DataItem label="同期">
            <span data-testid="project-repo-sync">{repo.sync ? repoSyncLabel(repo.sync) : "-"}</span>
          </DataItem>
        </div>

        <ProjectActionFlash outcome={fetcher.data} />

        <details>
          <summary
            className="cursor-pointer text-sm font-medium text-fg-muted hover:text-fg"
            data-testid="project-repo-edit-toggle"
          >
            変更
          </summary>
          <fetcher.Form method="post" className="mt-3 space-y-3" data-testid="project-repo-edit-form">
            <input type="hidden" name="intent" value="repo_patch" />
            <input type="hidden" name="repo_id" value={repo.id} />
            <RepoFields
              idPrefix={`repo-edit-${repo.id}`}
              clusters={clusters}
              showKind={false}
              defaults={{
                name: repo.name,
                place: repoPlaceOf(repo.location),
                path: repo.location.path,
                cluster: repo.location.kind === "remote" ? repo.location.cluster : undefined,
                defaultBranch: repo.default_branch ?? "",
                run: repo.run ?? "auto",
              }}
              error={error}
            />
            <Button type="submit" variant="secondary" size="sm" disabled={submitting} data-testid="project-repo-save">
              <Icon name="check" />
              保存
            </Button>
          </fetcher.Form>
        </details>

        <div className="flex flex-wrap items-center gap-2">
          {!isPrimary && (
            <fetcher.Form method="post">
              <input type="hidden" name="intent" value="repo_primary" />
              <input type="hidden" name="repo_id" value={repo.id} />
              <Button
                type="submit"
                variant="soft"
                size="sm"
                disabled={submitting}
                data-testid="project-repo-set-primary"
              >
                <Icon name="target" />
                {SET_PRIMARY_REPO_LABEL}
              </Button>
            </fetcher.Form>
          )}
          <fetcher.Form method="post">
            <input type="hidden" name="intent" value="repo_delete" />
            <input type="hidden" name="repo_id" value={repo.id} />
            <Button
              type="submit"
              variant="danger"
              size="sm"
              disabled={submitting}
              data-testid="project-repo-delete"
              onClick={(e) => {
                // 確認は 1 回だけ。ブラウザ以外（テスト・SSR）では `confirm` が無いので、あるときだけ聞く
                // （`~/routes/releases.tsx` の「昇格」と同じ作り）。使用中かどうかは taskd が 409 で決める。
                if (typeof window !== "undefined" && typeof window.confirm === "function") {
                  if (!window.confirm(`リポジトリ「${repo.name}」を案件から外します。よろしいですか？`)) {
                    e.preventDefault();
                  }
                }
              }}
            >
              <Icon name="x" />
              削除
            </Button>
          </fetcher.Form>
        </div>
      </CardBody>
    </Card>
  );
}
