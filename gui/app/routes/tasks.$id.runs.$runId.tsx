import { useEffect, useRef, useState } from "react";
import { isRouteErrorResponse, Link } from "react-router";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import type { RunList, RunSummary } from "~/celeris/types";
import { CodeViewer } from "~/components/CodeViewer";
import { RouteRecovery } from "~/components/RouteRecovery";
import { Badge } from "~/components/ui/badge";
import { buttonClass } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { Icon } from "~/components/ui/Icon";
import { Alert } from "~/components/ui/misc";
import { isTransientStatus } from "~/lib/recovery";
import { classifyStreamJsonLine, type FormattedLine } from "~/lib/stream-json";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/tasks.$id.runs.$runId";

/**
 * `/tasks/:id/runs/:runId`（生ログ、docs/DESIGN.md §4.3「生ログ」、§6.2、docs/adr/0006-g3-decisions.md D4）。
 * 本体（ファイルの中身）は `GET /tasks/{id}/runs` の要約と一緒に loader が celeris から取り、SSR で最初の表示を作る
 * （初回表示のためだけに追加のラウンドトリップを増やさない）。実行中の run の追尾は画面側が `/files/...` を
 * `?offset=` 付きで 1 秒ごとに叩く（DESIGN §4.3）。
 */
export interface RunDetailData {
  taskId: string;
  run: RunSummary;
  stdout: string | null;
  stderr: string | null;
  result: string | null;
  /// ADR-0023 D2: ワーカーに渡した指示（`runs/<run_id>/request.json`）。導入前の run には無いので `null`。
  request: string | null;
  /// ADR-0023 M1: claude-code / codex に実際に渡した文面（`runs/<run_id>/prompt.txt`）。fake には無い。
  prompt: string | null;
}

async function readFileText(client: CelerisClient, path: string, signal: AbortSignal): Promise<string | null> {
  try {
    const res = await client.file(path, { signal });
    return await res.text();
  } catch {
    return null;
  }
}

export async function loadRunDetail(
  client: CelerisClient,
  taskId: string,
  runId: string,
  request: Request,
): Promise<RunDetailData> {
  const runs = await client.get<RunList>(`/tasks/${taskId}/runs`, { signal: request.signal });
  const run = runs.runs.find((r) => r.run_id === runId);
  if (!run) {
    const data: CelerisRouteErrorData = {
      kind: "celeris_error",
      status: 404,
      code: "run_not_found",
      detail: `run ${runId} not found`,
    };
    throw new Response(JSON.stringify(data), { status: 404, headers: { "Content-Type": "application/json" } });
  }
  const files = run.files;
  const base = `/tasks/${taskId}/runs/${runId}`;
  const [stdout, stderr, result, requestJson, promptText] = await Promise.all([
    files?.stdout ? readFileText(client, `${base}/stdout`, request.signal) : Promise.resolve(null),
    files?.stderr ? readFileText(client, `${base}/stderr`, request.signal) : Promise.resolve(null),
    files?.result ? readFileText(client, `${base}/result`, request.signal) : Promise.resolve(null),
    files?.request ? readFileText(client, `${base}/request`, request.signal) : Promise.resolve(null),
    files?.prompt ? readFileText(client, `${base}/prompt`, request.signal) : Promise.resolve(null),
  ]);
  return { taskId, run, stdout, stderr, result, request: requestJson, prompt: promptText };
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "run ログ - Celeris" }];
}

export async function loader({ params, request }: Route.LoaderArgs): Promise<RunDetailData> {
  try {
    return await loadRunDetail(getCelerisClient(), params.id, params.runId, request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

/** `wc -l` と同じ数え方(末尾の改行 1 つは行として数えない)。 */
function splitLines(content: string): string[] {
  const lines = content.split("\n");
  if (lines.at(-1) === "") lines.pop();
  return lines;
}

function stderrTail(content: string, maxLines = 200): string {
  const lines = content.split("\n");
  return lines.slice(-maxLines).join("\n");
}

export default function RunDetailPage({ loaderData }: Route.ComponentProps) {
  const { taskId, run, stdout, stderr, result, request, prompt } = loaderData;
  const [rawMode, setRawMode] = useState(false);
  const [lines, setLines] = useState<string[]>(() => splitLines(stdout ?? ""));
  const offsetRef = useRef(new TextEncoder().encode(stdout ?? "").length);
  const running = !run.finished_at;

  // 実行中の run は `?offset=` で追尾する(DESIGN §4.3)。1 秒ごとに新着分だけ取りに行く。
  useEffect(() => {
    if (!running) return;
    let cancelled = false;
    const id = setInterval(async () => {
      try {
        const res = await fetch(`/files/tasks/${taskId}/runs/${run.run_id}/stdout?offset=${offsetRef.current}`);
        if (!res.ok || cancelled) return;
        const chunk = await res.text();
        if (chunk.length === 0) return;
        offsetRef.current += new TextEncoder().encode(chunk).length;
        setLines((prev) => {
          const combined = prev.join("\n") + (prev.length > 0 ? "\n" : "") + chunk;
          return splitLines(combined);
        });
      } catch {
        // 追尾は best-effort。次の tick に任せる。
      }
    }, 1_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [running, taskId, run.run_id]);

  const formatted: FormattedLine[] = lines.map((line) => classifyStreamJsonLine(line));

  return (
    <div className="space-y-6">
      <Link
        to={`/tasks/${taskId}`}
        className="inline-flex items-center gap-1.5 text-sm font-medium text-fg-muted hover:text-fg"
      >
        <Icon name="arrowLeft" />← タスク詳細
      </Link>

      <section data-testid="run-header" className="rounded-2xl border border-border bg-surface p-6 shadow-sm">
        <div className="flex flex-wrap items-center gap-2">
          <Badge tone={running ? "primary" : "neutral"} dot pulse={running}>
            {running ? "実行中" : "終了"}
          </Badge>
          <Badge tone="neutral">{run.role}</Badge>
        </div>
        <h1 className="mt-2 text-xl font-semibold text-fg" data-testid="run-id">
          run <span className="font-mono text-base text-fg-subtle">{run.run_id}</span>
        </h1>
        <p className="mt-1 text-sm text-fg-muted">
          {run.adapter} / {run.provider ?? "-"} / {run.model} — {run.started_at} 〜 {run.finished_at ?? "実行中"}
        </p>
      </section>

      <section aria-labelledby="stdout-heading" data-testid="stdout-section">
        <Card>
          <CardHeader
            icon="terminal"
            title={
              <h2 id="stdout-heading" className="text-[0.95rem] font-semibold text-fg">
                stdout.jsonl（{lines.length} 行）
              </h2>
            }
            actions={
              <button
                type="button"
                onClick={() => setRawMode((v) => !v)}
                className={buttonClass({ variant: "secondary", size: "xs" })}
                data-testid="stdout-toggle-raw"
              >
                <Icon name="code" />
                {rawMode ? "構造化表示" : "生テキスト"}
              </button>
            }
          />
          <CardBody className={rawMode ? "p-0" : undefined}>
            {rawMode ? (
              <CodeViewer content={lines.join("\n")} />
            ) : (
              <ul
                className="space-y-1.5 rounded-lg bg-surface-2 p-3 font-mono text-xs leading-relaxed text-fg"
                data-testid="stdout-lines"
              >
                {formatted.map((entry, i) => (
                  <li
                    // biome-ignore lint/suspicious/noArrayIndexKey: 行は追尾で末尾に追加されるだけで並び替えない
                    key={i}
                    data-testid="stdout-line"
                    data-line-kind={entry.kind}
                    className="rounded-md border border-border-strong/40 bg-surface/40 px-2.5 py-1.5"
                  >
                    {entry.kind === "utterance" && <p className="whitespace-pre-wrap">{entry.text}</p>}
                    {entry.kind === "tool" && (
                      <p>
                        <span className="text-fg-subtle">tool:</span> {entry.label}
                        {entry.detail ? ` ${entry.detail}` : ""}
                      </p>
                    )}
                    {entry.kind === "result" && (
                      <p className={entry.isError ? "text-danger" : "text-success"}>result: {entry.text}</p>
                    )}
                    {entry.kind === "raw" && <p className="text-fg-subtle">{entry.text}</p>}
                  </li>
                ))}
              </ul>
            )}
          </CardBody>
        </Card>
      </section>

      {stderr !== null && (
        <section aria-labelledby="stderr-heading" data-testid="stderr-section">
          <Card>
            <CardHeader
              icon="alert"
              tone="warning"
              title={
                <h2 id="stderr-heading" className="text-[0.95rem] font-semibold text-fg">
                  stderr.log（末尾）
                </h2>
              }
            />
            <CardBody className="p-0">
              <CodeViewer content={stderrTail(stderr)} />
            </CardBody>
          </Card>
        </section>
      )}

      {result !== null && (
        <section aria-labelledby="result-heading" data-testid="result-section">
          <Card>
            <CardHeader
              icon="checkCircle"
              tone="success"
              title={
                <h2 id="result-heading" className="text-[0.95rem] font-semibold text-fg">
                  result.json
                </h2>
              }
            />
            <CardBody className="p-0">
              <CodeViewer content={result} json />
            </CardBody>
          </Card>
        </section>
      )}

      {/* ADR-0023 M1: claude-code / codex に実際に渡した文面。人が読むのはこちらが早い。 */}
      {prompt !== null && (
        <section aria-labelledby="prompt-heading" data-testid="prompt-section">
          <Card>
            <CardHeader
              icon="message"
              title={
                <h2 id="prompt-heading" className="text-[0.95rem] font-semibold text-fg">
                  ワーカーに渡した文面（prompt.txt）
                </h2>
              }
            />
            <CardBody>
              <details>
                <summary
                  className="cursor-pointer text-sm font-medium text-fg-muted hover:text-fg"
                  data-testid="prompt-toggle"
                >
                  開く
                </summary>
                <div className="mt-3">
                  <CodeViewer content={prompt} />
                </div>
              </details>
            </CardBody>
          </Card>
        </section>
      )}

      {/* ADR-0023 D2: この run でワーカーに渡した指示そのもの(構造)。既定は畳んでおく(長いので)。 */}
      {request !== null && (
        <section aria-labelledby="request-heading" data-testid="request-section">
          <Card>
            <CardHeader
              icon="file"
              title={
                <h2 id="request-heading" className="text-[0.95rem] font-semibold text-fg">
                  ワーカーに渡した指示（request.json）
                </h2>
              }
            />
            <CardBody>
              <details>
                <summary
                  className="cursor-pointer text-sm font-medium text-fg-muted hover:text-fg"
                  data-testid="request-toggle"
                >
                  開く
                </summary>
                <div className="mt-3">
                  <CodeViewer content={request} json />
                </div>
              </details>
            </CardBody>
          </Card>
        </section>
      )}
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
          {data.status === 404 ? "run が見つかりません" : `エラー ${data.status}`}
        </h1>
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
