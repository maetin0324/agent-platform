import { Link } from "react-router";
import { Badge, StatusBadge } from "~/components/ui/badge";
import { Card, CardBody } from "~/components/ui/card";
import { EmptyState, Mono } from "~/components/ui/misc";
import { integrationMethodLabel, integrationStateLabel } from "~/lib/labels";
import { integrationStateTone } from "~/lib/task-changes";
import type { ProjectIntegrationItem } from "~/taskd/types";

/**
 * 案件の「PR と取り込み」節（ADR-0043 D5、taskd Phase 54 / G17。`GET /projects/{id}/integrations`）。
 * タスク × リポジトリごとに**最新の 1 件**を新しい順で taskd が返すので、並べ替えも集計もしない。
 * 操作はここには置かない（取り込みはタスクの `/tasks/:id/changes` で行う。ここは「いまどうなっているか」だけ）。
 */
export interface ProjectIntegrationsProps {
  items: readonly ProjectIntegrationItem[];
}

export function ProjectIntegrations({ items }: ProjectIntegrationsProps) {
  if (items.length === 0) {
    return (
      <EmptyState icon="gitBranch" title="取り込みの記録がありません" data-testid="project-integrations-empty">
        タスクの「変更」から取り込む・PR を作ると、ここに残ります。
      </EmptyState>
    );
  }
  return (
    <Card data-testid="project-integrations">
      <CardBody className="p-0">
        <ul className="divide-y divide-border">
          {items.map((item) => (
            <IntegrationRow key={item.integration.id} item={item} />
          ))}
        </ul>
      </CardBody>
    </Card>
  );
}

function IntegrationRow({ item }: { item: ProjectIntegrationItem }) {
  const { integration } = item;
  return (
    <li
      className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-5 py-3"
      data-testid="project-integration-row"
      data-integration-method={integration.method}
      data-integration-state={integration.state}
    >
      <Link
        to={`/tasks/${integration.task_id}`}
        className="min-w-0 flex-1 truncate font-medium text-primary hover:underline"
        data-testid="project-integration-task"
      >
        {item.task_title}
      </Link>
      <StatusBadge status={item.task_status} />
      <Mono className="text-xs" data-testid="project-integration-repo">
        {integration.repo}
      </Mono>
      <Badge tone={integrationStateTone(integration.state)} data-testid="project-integration-state">
        {integrationMethodLabel(integration.method)}: {integrationStateLabel(integration.state)}
      </Badge>
      {integration.pr_number != null && integration.pr_url && (
        <a
          href={integration.pr_url}
          target="_blank"
          rel="noreferrer noopener"
          className="text-sm font-medium text-primary underline underline-offset-2"
          data-testid="project-integration-pr-link"
        >
          #{integration.pr_number}
        </a>
      )}
      <span className="text-xs text-fg-subtle tabular-nums" data-testid="project-integration-updated-at">
        {integration.updated_at}
      </span>
    </li>
  );
}
