import { useEffect, useState } from "react";
import { Link } from "react-router";
import { CodeViewer } from "~/components/CodeViewer";
import { ImageViewer } from "~/components/ImageViewer";
import { LocalTime } from "~/components/LocalTime";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Sha256Badge } from "~/components/Sha256Badge";
import { Badge, statusTone } from "~/components/ui/badge";
import { buttonClass } from "~/components/ui/button";
import { Icon } from "~/components/ui/Icon";
import { artifactStatusMessage, isJson, pickViewer } from "~/lib/artifact-view";
import { isSourcesArtifact, type ProjectArtifactRow, parseSourcesJson } from "~/lib/artifacts";
import { taskStatusLabel } from "~/lib/labels";

/**
 * 案件を横断した成果物一覧の部品（SPEC §2.2「調査結果の文書と見るべき関連研究へのリンクがまとまって読める」）。
 * `/artifacts` と `/projects/:id` の「成果物」節で共有する（`~/components/ReportsList.tsx` と同じ作り。
 * G13b-1 の依頼どおり）。本体は「開く」を押したときだけ `/files/tasks/:id/artifacts/:idx` を fetch し
 * （`~/routes/tasks.$id.tsx::ArtifactRow` と同じ規則。celeris が返した実際の `Content-Type` でビューアを選ぶ）、
 * Markdown はその場で描画、`sources.json`（名前で判定。中身の形は
 * docs/adr/0031-web-research-evidence-gate.md）はリンク集、それ以外の JSON は整形表示。
 */

export interface ArtifactsListProps {
  rows: ProjectArtifactRow[];
  fetchedAt: string;
}

export function ArtifactsList({ rows, fetchedAt }: ArtifactsListProps) {
  return (
    <ul className="space-y-3">
      {rows.map((row) => (
        <ArtifactRow key={`${row.taskId}:${row.artifact.idx}`} row={row} fetchedAt={fetchedAt} />
      ))}
    </ul>
  );
}

function ArtifactRow({ row, fetchedAt }: { row: ProjectArtifactRow; fetchedAt: string }) {
  const { artifact, taskId, taskTitle, taskStatus, assigneeName, workspace } = row;
  const [open, setOpen] = useState(false);
  const [body, setBody] = useState<{ contentType: string; content?: string } | null>(null);
  const href = `/files/tasks/${taskId}/artifacts/${artifact.idx}`;
  const canOpen = artifact.exists && !artifact.forbidden;
  const statusMessage = artifactStatusMessage(artifact);
  const name = artifact.artifact.name;

  useEffect(() => {
    if (!open || body || !canOpen) return;
    let cancelled = false;
    (async () => {
      const res = await fetch(href);
      const contentType = res.headers.get("content-type") ?? "application/octet-stream";
      if (pickViewer(contentType, name) === "image") {
        if (!cancelled) setBody({ contentType });
        return;
      }
      const content = await res.text();
      if (!cancelled) setBody({ contentType, content });
    })();
    return () => {
      cancelled = true;
    };
  }, [open, body, canOpen, href, name]);

  const sourcesLinks = body?.content !== undefined && isSourcesArtifact(name) ? parseSourcesJson(body.content) : null;

  return (
    <li
      data-testid="artifact-row"
      data-artifact-name={name}
      data-task-id={taskId}
      className="rounded-lg border border-border p-3 text-sm"
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <p className="truncate font-mono text-xs text-fg" title={name}>
            {name}
          </p>
          <p className="flex flex-wrap items-center gap-1.5 text-xs text-fg-subtle">
            <Badge tone="neutral">{artifact.artifact.kind}</Badge>
            <Link to={`/tasks/${taskId}`} className="font-medium text-primary hover:underline">
              {taskTitle}
            </Link>
            {assigneeName && <span>（{assigneeName}）</span>}
            <Badge tone={statusTone(taskStatus)} dot>
              {taskStatusLabel(taskStatus)}
            </Badge>
          </p>
          <p className="text-xs text-fg-subtle" data-testid="artifact-workspace" data-task-id={taskId}>
            置き場所:{" "}
            {workspace.vscodeHref ? (
              <a href={workspace.vscodeHref} className="font-mono text-primary hover:underline">
                {workspace.text}
              </a>
            ) : (
              <code className="font-mono">{workspace.text}</code>
            )}
            {/* ADR-0039 D3（Phase G13k）: Remote は編集を手元、検証をリモートで行う。手元の写しの場所も添える。 */}
            {workspace.localCopyNote && (
              <span className="ml-1" data-testid="artifact-workspace-local-copy">
                （{workspace.localCopyNote}）
              </span>
            )}
          </p>
          <p className="text-xs tabular-nums text-fg-subtle">
            <LocalTime iso={artifact.ts} fetchedAtIso={fetchedAt} />
          </p>
        </div>
        {canOpen && (
          <div className="flex shrink-0 gap-2">
            <button
              type="button"
              onClick={() => setOpen((v) => !v)}
              data-testid="artifact-toggle"
              className={buttonClass({ variant: "secondary", size: "xs" })}
            >
              <Icon name={open ? "chevronDown" : "chevronRight"} />
              {open ? "閉じる" : "開く"}
            </button>
            <a
              href={`${href}?download=1`}
              download
              data-testid="artifact-download"
              className={buttonClass({ variant: "ghost", size: "xs" })}
            >
              保存
            </a>
          </div>
        )}
      </div>
      {statusMessage && (
        <p
          data-testid={artifact.forbidden ? "artifact-forbidden" : "artifact-missing"}
          className="mt-2 rounded-md border border-danger-border bg-danger-soft px-2.5 py-1.5 text-danger-soft-fg"
        >
          {statusMessage}
        </p>
      )}
      <Sha256Badge
        recorded={artifact.artifact.sha256}
        current={artifact.sha256_current}
        matches={artifact.sha256_matches}
      />
      {open && body && (
        <div className="mt-3" data-testid="artifact-view">
          {isSourcesArtifact(name) && sourcesLinks ? (
            <ul className="space-y-1.5 rounded-lg border border-border bg-surface p-3" data-testid="artifact-links">
              {sourcesLinks.length === 0 ? (
                <li className="text-xs text-fg-subtle">リンクがありません。</li>
              ) : (
                sourcesLinks.map((link) => (
                  <li key={link.url} className="flex items-center gap-2">
                    <Icon name="link" className="size-3.5 shrink-0 text-fg-subtle" />
                    <a
                      href={link.url}
                      target="_blank"
                      rel="noreferrer"
                      className="min-w-0 flex-1 truncate text-primary hover:underline"
                      title={link.title}
                    >
                      {link.title}
                    </a>
                    {link.cited && <Badge tone="success">引用</Badge>}
                  </li>
                ))
              )}
            </ul>
          ) : pickViewer(body.contentType, name) === "image" ? (
            <ImageViewer src={href} alt={name} />
          ) : pickViewer(body.contentType, name) === "markdown" ? (
            <MarkdownViewer content={body.content ?? ""} />
          ) : (
            <CodeViewer content={body.content ?? ""} json={isJson(body.contentType)} />
          )}
        </div>
      )}
    </li>
  );
}
