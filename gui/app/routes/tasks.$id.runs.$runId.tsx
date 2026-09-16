import { useEffect, useRef, useState } from "react";
import { isRouteErrorResponse, Link } from "react-router";
import { CodeViewer } from "~/components/CodeViewer";
import { classifyStreamJsonLine, type FormattedLine } from "~/lib/stream-json";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { RunList, RunSummary } from "~/taskd/types";
import type { Route } from "./+types/tasks.$id.runs.$runId";

/**
 * `/tasks/:id/runs/:runId`（生ログ、docs/DESIGN.md §4.3「生ログ」、§6.2、docs/adr/0006-g3-decisions.md D4）。
 * 本体（ファイルの中身）は `GET /tasks/{id}/runs` の要約と一緒に loader が taskd から取り、SSR で最初の表示を作る
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

async function readFileText(client: TaskdClient, path: string, signal: AbortSignal): Promise<string | null> {
  try {
    const res = await client.file(path, { signal });
    return await res.text();
  } catch {
    return null;
  }
}

export async function loadRunDetail(
  client: TaskdClient,
  taskId: string,
  runId: string,
  request: Request,
): Promise<RunDetailData> {
  const runs = await client.get<RunList>(`/tasks/${taskId}/runs`, { signal: request.signal });
  const run = runs.runs.find((r) => r.run_id === runId);
  if (!run) {
    const data: TaskdRouteErrorData = {
      kind: "taskd_error",
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
  return [{ title: "run ログ - taskd-gui" }];
}

export async function loader({ params, request }: Route.LoaderArgs): Promise<RunDetailData> {
  try {
    return await loadRunDetail(getTaskdClient(), params.id, params.runId, request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

/** `wc -l` と同じ数え方（末尾の改行 1 つは行として数えない）。 */
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

  // 実行中の run は `?offset=` で追尾する（DESIGN §4.3）。1 秒ごとに新着分だけ取りに行く。
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
      <p>
        <Link to={`/tasks/${taskId}`}>← タスク詳細</Link>
      </p>
      <section data-testid="run-header">
        <h1 className="text-xl font-semibold" data-testid="run-id">
          run {run.run_id}
        </h1>
        <p className="text-sm text-gray-600">
          {run.adapter} / {run.provider ?? "-"} / {run.model} — {run.started_at} 〜 {run.finished_at ?? "実行中"}
        </p>
      </section>

      <section aria-labelledby="stdout-heading" data-testid="stdout-section">
        <div className="flex items-center justify-between">
          <h2 id="stdout-heading" className="text-lg font-semibold">
            stdout.jsonl（{lines.length} 行）
          </h2>
          <button
            type="button"
            onClick={() => setRawMode((v) => !v)}
            className="rounded border px-2 py-0.5 text-sm"
            data-testid="stdout-toggle-raw"
          >
            {rawMode ? "構造化表示" : "生テキスト"}
          </button>
        </div>
        {rawMode ? (
          <CodeViewer content={lines.join("\n")} />
        ) : (
          <ul className="mt-2 space-y-1 text-sm" data-testid="stdout-lines">
            {formatted.map((entry, i) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: 行は追尾で末尾に追加されるだけで並び替えない
              <li key={i} data-testid="stdout-line" data-line-kind={entry.kind} className="rounded border p-1">
                {entry.kind === "utterance" && <p>{entry.text}</p>}
                {entry.kind === "tool" && (
                  <p>
                    <span className="font-mono text-xs text-gray-500">tool:</span> {entry.label}
                    {entry.detail ? ` ${entry.detail}` : ""}
                  </p>
                )}
                {entry.kind === "result" && (
                  <p className={entry.isError ? "text-red-700" : "text-green-700"}>result: {entry.text}</p>
                )}
                {entry.kind === "raw" && <p className="font-mono text-xs">{entry.text}</p>}
              </li>
            ))}
          </ul>
        )}
      </section>

      {stderr !== null && (
        <section aria-labelledby="stderr-heading" data-testid="stderr-section">
          <h2 id="stderr-heading" className="text-lg font-semibold">
            stderr.log（末尾）
          </h2>
          <CodeViewer content={stderrTail(stderr)} />
        </section>
      )}

      {result !== null && (
        <section aria-labelledby="result-heading" data-testid="result-section">
          <h2 id="result-heading" className="text-lg font-semibold">
            result.json
          </h2>
          <CodeViewer content={result} json />
        </section>
      )}

      {/* ADR-0023 M1: claude-code / codex に実際に渡した文面。人が読むのはこちらが早い。 */}
      {prompt !== null && (
        <section aria-labelledby="prompt-heading" data-testid="prompt-section">
          <h2 id="prompt-heading" className="text-lg font-semibold">
            ワーカーに渡した文面（prompt.txt）
          </h2>
          <details>
            <summary className="cursor-pointer text-sm text-gray-600" data-testid="prompt-toggle">
              開く
            </summary>
            <CodeViewer content={prompt} />
          </details>
        </section>
      )}

      {/* ADR-0023 D2: この run でワーカーに渡した指示そのもの（構造）。既定は畳んでおく（長いので）。 */}
      {request !== null && (
        <section aria-labelledby="request-heading" data-testid="request-section">
          <h2 id="request-heading" className="text-lg font-semibold">
            ワーカーに渡した指示（request.json）
          </h2>
          <details>
            <summary className="cursor-pointer text-sm text-gray-600" data-testid="request-toggle">
              開く
            </summary>
            <CodeViewer content={request} json />
          </details>
        </section>
      )}
    </div>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as TaskdRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="p-4">
          <TaskdBanner taskdApiUrl={data.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="p-4">
        <h1 className="text-xl font-semibold">
          {data.status === 404 ? "run が見つかりません" : `エラー ${data.status}`}
        </h1>
        <p className="mt-2 text-sm text-gray-600">{data.detail}</p>
      </main>
    );
  }
  return (
    <main className="p-4">
      <h1 className="text-xl font-semibold">エラー</h1>
      <p className="mt-2 text-sm text-gray-600">予期しないエラーが起きました。</p>
    </main>
  );
}
