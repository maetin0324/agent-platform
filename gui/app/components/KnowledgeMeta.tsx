import { Link } from "react-router";
import type { Confidence } from "~/celeris/types";
import { confidenceLabel, confidenceTone, knowledgeSource } from "~/lib/knowledge";
import { Badge } from "./ui/badge";
import { Mono } from "./ui/misc";

/**
 * 知識ベースのページ・候補に付く見出し情報（ADR-0047 D1 / D5）。celeris が返すものを**そのまま**出す
 * （置き場・タグ・出典・確度・更新日）。並べ替えも言い換えもしない。
 */
export function KnowledgeMeta({
  path,
  scope,
  tags,
  sources,
  confidence,
  updated,
}: {
  path?: string | null;
  scope?: string | null;
  tags?: string[];
  sources?: string[];
  confidence?: Confidence | null;
  updated?: string | null;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs text-fg-subtle" data-testid="knowledge-meta">
      {path && <Mono data-testid="knowledge-page-path">{path}</Mono>}
      {scope && (
        <Badge tone="info" data-testid="knowledge-page-scope">
          {scope}
        </Badge>
      )}
      {(tags ?? []).map((tag) => (
        <Badge key={tag} tone="neutral" data-testid="knowledge-page-tag">
          {tag}
        </Badge>
      ))}
      {confidenceLabel(confidence) && (
        <Badge tone={confidenceTone(confidence)} data-testid="knowledge-page-confidence">
          {confidenceLabel(confidence)}
        </Badge>
      )}
      {updated && <span data-testid="knowledge-page-updated">更新 {updated}</span>}
      <KnowledgeSources sources={sources} />
    </div>
  );
}

/**
 * 出典（`sources[]`）。`task:<ULID>` はタスクの画面へ、`url:<…>` は外へ（`rel="noreferrer noopener"`）、
 * `message:<id>` と `human` は文字のまま。どれに開くかは `~/lib/knowledge.ts` の純粋関数が決める。
 */
export function KnowledgeSources({ sources }: { sources?: string[] }) {
  if (!sources || sources.length === 0) return null;
  return (
    <span className="flex flex-wrap items-center gap-1.5" data-testid="knowledge-sources">
      <span>出典</span>
      {sources.map((raw) => {
        const source = knowledgeSource(raw);
        if (source.kind === "task") {
          return (
            <Link key={raw} to={source.href} data-testid="knowledge-source-task" className="no-underline">
              <Badge tone="info">{source.label}</Badge>
            </Link>
          );
        }
        if (source.kind === "url") {
          return (
            <a
              key={raw}
              href={source.href}
              target="_blank"
              rel="noreferrer noopener"
              data-testid="knowledge-source-url"
              className="no-underline"
            >
              <Badge tone="teal">{source.label}</Badge>
            </a>
          );
        }
        return (
          <Badge key={raw} tone="neutral" data-testid="knowledge-source-plain">
            {source.label}
          </Badge>
        );
      })}
    </span>
  );
}
