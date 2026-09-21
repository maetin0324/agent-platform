import { useState } from "react";
import { Link, useFetcher } from "react-router";
import type { ReportOpOutcome } from "~/celeris/action-types";
import type { OrgNode, Project, Report, ReportDetail, ReportKind } from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { MarkdownViewer } from "~/components/MarkdownViewer";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { touchLinkClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import type { Tone } from "~/components/ui/tone";
import { shortId } from "~/lib/format";
import { relativeTimeLabel, reportNodeName, reportProjectName } from "~/lib/reports";
import { cn } from "~/lib/utils";

/**
 * 報告 1 件の行（SPEC §3.5・§4 の 4「高速で流し見」、ADR-0033 D3）。`/reports`（報告の流れ）と
 * `/projects/:id` の「報告」タブで同じ部品を流用する（G13b-1 の依頼どおり）。
 * クリックで展開すると `GET /reports/{id}`（resource route `app/routes/reports.$id.tsx`）を呼んで
 * `body` と `sources_expanded` を出す。`sources_expanded` の各報告も同じ行で再帰的に展開できる
 * （「圧縮の元を見に行ける」）。既読は行の中の「既読にする」から 1 件ずつ（`POST /reports/read`。
 * 一括は呼び出し側の画面（`/reports`）に別途ボタンがある）。
 */

const KIND_TONE: Record<ReportKind, Tone> = {
  bad_news: "danger",
  result: "neutral",
  question: "warning",
  proposal: "success",
  progress: "info",
};

const KIND_LABEL: Record<ReportKind, string> = {
  bad_news: "悪い知らせ",
  result: "結果",
  question: "質問",
  proposal: "提案",
  progress: "経過",
};

export interface ReportsListProps {
  items: Report[];
  projects: Project[];
  org: OrgNode[];
  fetchedAt: string;
}

export function ReportsList({ items, projects, org, fetchedAt }: ReportsListProps) {
  return (
    <ul className="space-y-1" data-testid="reports-section">
      {items.map((r) => (
        <ReportRow key={r.id} report={r} depth={0} projects={projects} org={org} fetchedAt={fetchedAt} />
      ))}
    </ul>
  );
}

/** 報告 1 件から辿れる先（監査 6）: 案件・担当・裏方のタスク・その案件の成果物。 */
function ReportLinks({ report, projects }: { report: Report; projects: Project[] }) {
  const projectId = report.project_id ?? null;
  return (
    // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
    <p className="flex flex-wrap items-center gap-3 text-sm lg:text-xs" data-testid="report-links">
      {projectId && (
        <Link
          to={`/projects/${projectId}`}
          data-testid="report-project-link"
          className={cn(touchLinkClass, "underline underline-offset-2")}
        >
          案件へ（{reportProjectName(report, projects)}）
        </Link>
      )}
      <Link
        to={`/org/${encodeURIComponent(report.node_id)}`}
        data-testid="report-talk-link"
        className={cn(touchLinkClass, "underline underline-offset-2")}
      >
        担当に話す
      </Link>
      {report.task_id && (
        <Link
          to={`/tasks/${report.task_id}`}
          data-testid="report-task-link"
          className={cn(touchLinkClass, "underline underline-offset-2")}
        >
          裏方のタスク
        </Link>
      )}
      {projectId && (
        <Link
          to={`/artifacts?project=${encodeURIComponent(projectId)}`}
          data-testid="report-artifacts-link"
          className={cn(touchLinkClass, "underline underline-offset-2")}
        >
          その案件の成果物
        </Link>
      )}
    </p>
  );
}

function ReportRow({
  report,
  depth,
  projects,
  org,
  fetchedAt,
}: {
  report: Report;
  depth: number;
  projects: Project[];
  org: OrgNode[];
  fetchedAt: string;
}) {
  const [open, setOpen] = useState(false);
  const detailFetcher = useFetcher<ReportDetail>();
  const readFetcher = useFetcher<ReportOpOutcome>();

  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next && detailFetcher.state === "idle" && !detailFetcher.data) {
      detailFetcher.load(`/reports/${encodeURIComponent(report.id)}`);
    }
  };

  const markRead = () => {
    const form = new FormData();
    form.set("intent", "reports_read");
    form.append("ids", report.id);
    readFetcher.submit(form, { method: "post", action: "/reports" });
  };

  const justMarkedRead = readFetcher.data?.ok === true && readFetcher.data.op === "reports_read";
  const isRead = report.read_at != null || justMarkedRead;
  const detail = detailFetcher.data;

  return (
    <li
      id={`report-${report.id}`}
      data-testid="report-row"
      data-report-id={report.id}
      data-report-kind={report.kind}
      className={cn(
        // フェーズ 72（ADR-0055 D2 ラウンド 4）: 一覧はカード（縦積み）の規律に揃え、depth 0 の行を
        // 枠付きのカードにした（差し込みの元報告は今までどおり `border-l` の字下げで区別する）。
        depth === 0 ? "rounded-lg border border-border bg-surface" : "ml-3 border-l border-border pl-3",
        // 悪い知らせは行そのものを目立たせる（SPEC §2.4「良い知らせと同じ経路で、目立つ形で届く」。監査 6）。
        report.kind === "bad_news" && "rounded-lg border border-danger-border bg-danger-soft/70",
      )}
    >
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className="flex min-h-11 w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-sm transition-colors hover:bg-surface-2"
      >
        <Icon name={open ? "chevronDown" : "chevronRight"} className="size-3.5 shrink-0 text-fg-subtle" />
        <Badge tone={KIND_TONE[report.kind]}>{KIND_LABEL[report.kind] ?? report.kind}</Badge>
        <span data-testid="report-headline" className="min-w-0 flex-1 truncate font-medium text-fg">
          {report.headline}
        </span>
        {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
        <span className="hidden shrink-0 text-sm text-fg-subtle sm:inline lg:text-xs">
          {reportProjectName(report, projects)}
        </span>
        <span className="hidden shrink-0 text-sm text-fg-subtle md:inline lg:text-xs">
          {reportNodeName(report, org)}
        </span>
        <span className="shrink-0 text-sm tabular-nums text-fg-subtle lg:text-xs">
          {relativeTimeLabel(report.created_at, fetchedAt)}
        </span>
        {!isRead && (
          <Badge tone="info" dot className="shrink-0">
            未読
          </Badge>
        )}
      </button>

      {open && (
        <div className="ml-6 mt-1.5 space-y-2 rounded-lg border border-border bg-surface-2/40 p-3">
          {detailFetcher.state !== "idle" && !detail ? (
            <p className="text-sm text-fg-subtle lg:text-xs">読み込み中…</p>
          ) : detail ? (
            <>
              {/* 本文は Markdown で描く（対話・認可と同じ。監査 6）。 */}
              <div data-testid="report-body" className="text-sm text-fg">
                {detail.report.body ? <MarkdownViewer content={detail.report.body} /> : <p>（本文なし）</p>}
              </div>
              {/* ADR-0055 D2「id は末尾だけ、全文は title」（フェーズ 72）。 */}
              <p className="font-mono text-xs break-all text-fg-subtle" title={detail.report.id}>
                id: {shortId(detail.report.id)}
              </p>
              <ReportLinks report={detail.report} projects={projects} />
              <Button
                variant="secondary"
                size="xs"
                disabled={isRead || readFetcher.state !== "idle"}
                onClick={markRead}
                data-testid="report-mark-read"
              >
                <Icon name="check" />
                {isRead ? "既読" : "既読にする"}
              </Button>
              {readFetcher.data && !readFetcher.data.ok && <ErrorFlash error={readFetcher.data.error} />}
              {detail.sources_expanded.length > 0 && (
                <div data-testid="report-sources" className="space-y-1.5 pt-1">
                  <p className="text-sm font-medium text-fg-subtle lg:text-xs">元になった報告（圧縮元）</p>
                  <ul className="space-y-1.5">
                    {detail.sources_expanded.map((src) => (
                      <ReportRow
                        key={src.id}
                        report={src}
                        depth={depth + 1}
                        projects={projects}
                        org={org}
                        fetchedAt={fetchedAt}
                      />
                    ))}
                  </ul>
                </div>
              )}
            </>
          ) : null}
        </div>
      )}
    </li>
  );
}
