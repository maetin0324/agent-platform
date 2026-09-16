import { ReactFlow } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useEffect, useMemo, useState } from "react";
import { Form, isRouteErrorResponse, useSearchParams } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import { layoutGraph } from "~/lib/graph-layout";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { Graph } from "~/taskd/types";
import type { Route } from "./+types/graph";

/**
 * `/graph`（DAG、docs/DESIGN.md §4.2, §6.2, docs/adr/0006-g3-decisions.md D5）。
 * `GET /graph` をそのまま返す（レイアウト・色分けは表示のためだけで、taskd の判断値は増やさない）。
 */
export async function loadGraph(client: TaskdClient, request: Request): Promise<Graph> {
  const url = new URL(request.url);
  const root = url.searchParams.get("root");
  const depth = url.searchParams.get("depth");
  const includeTerminal = url.searchParams.get("include_terminal");
  return client.get<Graph>("/graph", {
    query: {
      root: root ?? undefined,
      depth: depth ?? undefined,
      include_terminal: includeTerminal ?? undefined,
    },
    signal: request.signal,
  });
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "DAG - taskd-gui" }];
}

export async function loader({ request }: Route.LoaderArgs): Promise<Graph> {
  try {
    return await loadGraph(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export default function GraphPage({ loaderData }: Route.ComponentProps) {
  const graph = loaderData;
  // `@xyflow/react` は ResizeObserver 等のブラウザ API に依存するクライアント専用の描画なので、
  // マウント後にだけ本体を描く（SSR とハイドレーション直後は同じプレースホルダを出す。ADR-0006 D5）。
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  const { nodes, edges } = useMemo(() => layoutGraph(graph), [graph]);
  const [searchParams] = useSearchParams();

  return (
    <div className="flex h-[calc(100vh-10rem)] flex-col gap-2">
      <h1 className="text-xl font-semibold">
        DAG
        <HelpLink anchor="screens" label="画面ごとの説明" />
      </h1>
      <Form method="get" className="flex flex-wrap items-end gap-3 text-sm" data-testid="graph-filter-form">
        <label className="flex flex-col gap-1">
          root
          <input name="root" defaultValue={searchParams.get("root") ?? ""} className="rounded border px-2 py-1" />
        </label>
        <label className="flex flex-col gap-1">
          depth
          <input
            name="depth"
            type="number"
            min={0}
            defaultValue={searchParams.get("depth") ?? ""}
            className="w-20 rounded border px-2 py-1"
          />
        </label>
        <button type="submit" className="rounded border px-3 py-1">
          絞り込み
        </button>
      </Form>
      <p className="text-xs text-gray-500" data-testid="graph-summary">
        {graph.nodes.length} ノード / {graph.edges.length} 辺
      </p>
      <div className="flex-1 rounded border" data-testid="graph-canvas">
        {mounted ? (
          <ReactFlow
            nodes={nodes}
            edges={edges}
            fitView
            proOptions={{ hideAttribution: true }}
            nodesConnectable={false}
          />
        ) : (
          <div
            className="flex h-full items-center justify-center text-sm text-gray-500"
            data-testid="graph-placeholder"
          >
            読み込み中…
          </div>
        )}
      </div>
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
          {data.status === 404 ? "タスクが見つかりません" : `エラー ${data.status}`}
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
