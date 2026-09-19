import { Background, Controls, ReactFlow } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useEffect, useMemo, useState } from "react";
import { Form, isRouteErrorResponse, useSearchParams } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import { Button } from "~/components/ui/button";
import { Card, CardBody } from "~/components/ui/card";
import { inputClass, labelClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { PageHeader } from "~/components/ui/misc";
import { layoutGraph } from "~/lib/graph-layout";
import { cn } from "~/lib/utils";
import { TaskdBanner } from "~/root";
import { getTaskdClient, type TaskdClient } from "~/taskd/client.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { DaemonView, Graph } from "~/taskd/types";
import type { Route } from "./+types/graph";

/**
 * `/graph`（DAG、docs/DESIGN.md §4.2, §6.2, docs/adr/0006-g3-decisions.md D5）。
 * `GET /graph` をそのまま返す（レイアウト・色分けは表示のためだけで、taskd の判断値は増やさない）。
 * ADR-0023 D3: 「部下待ち」は `GET /daemon` の `awaiting_children` をそのまま使う（GUI 側で状態を組み立て直さない）。
 */
export interface GraphData {
  graph: Graph;
  awaitingChildren: string[];
}

export async function loadGraph(client: TaskdClient, request: Request): Promise<GraphData> {
  const url = new URL(request.url);
  const root = url.searchParams.get("root");
  const depth = url.searchParams.get("depth");
  const includeTerminal = url.searchParams.get("include_terminal");
  const [graph, daemon] = await Promise.all([
    client.get<Graph>("/graph", {
      query: {
        root: root ?? undefined,
        depth: depth ?? undefined,
        include_terminal: includeTerminal ?? undefined,
      },
      signal: request.signal,
    }),
    // デーモンが止まっていても DAG は出す（「部下待ち」の印が付かないだけ）。
    client.get<DaemonView>("/daemon", { signal: request.signal }).catch(() => null),
  ]);
  return { graph, awaitingChildren: daemon?.snapshot?.awaiting_children ?? [] };
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "DAG - Celeris" }];
}

export async function loader({ request }: Route.LoaderArgs): Promise<GraphData> {
  try {
    return await loadGraph(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export default function GraphPage({ loaderData }: Route.ComponentProps) {
  const { graph, awaitingChildren } = loaderData;
  // `@xyflow/react` は ResizeObserver 等のブラウザ API に依存するクライアント専用の描画なので、
  // マウント後にだけ本体を描く（SSR とハイドレーション直後は同じプレースホルダを出す。ADR-0006 D5）。
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);
  const { nodes, edges } = useMemo(() => layoutGraph(graph, { awaitingChildren }), [graph, awaitingChildren]);
  const [searchParams] = useSearchParams();

  return (
    <div className="flex h-[calc(100vh-8rem)] min-h-[36rem] flex-col gap-4">
      <PageHeader
        as="h1"
        icon="network"
        title={
          <>
            DAG
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="depends_on の辺と親子関係をグラフで表示します（GET /graph をそのまま描画。レイアウト・色分けは表示のためだけ）。"
        className="shrink-0 pb-0"
      />

      <Card className="shrink-0">
        <CardBody>
          <Form method="get" className="flex flex-wrap items-end gap-3 text-sm" data-testid="graph-filter-form">
            <label className="flex flex-col gap-1">
              <span className={labelClass}>root</span>
              <input name="root" defaultValue={searchParams.get("root") ?? ""} className={cn(inputClass, "w-64")} />
            </label>
            <label className="flex flex-col gap-1">
              <span className={labelClass}>depth</span>
              <input
                name="depth"
                type="number"
                min={0}
                defaultValue={searchParams.get("depth") ?? ""}
                className={cn(inputClass, "w-20")}
              />
            </label>
            <Button type="submit" variant="primary" size="sm">
              <Icon name="filter" />
              絞り込み
            </Button>
          </Form>
        </CardBody>
      </Card>

      <p className="shrink-0 text-xs text-fg-subtle" data-testid="graph-summary">
        {graph.nodes.length} ノード / {graph.edges.length} 辺
      </p>

      <Card className="flex-1 overflow-hidden">
        <div className="h-full w-full" data-testid="graph-canvas">
          {mounted ? (
            <ReactFlow
              nodes={nodes}
              edges={edges}
              fitView
              proOptions={{ hideAttribution: true }}
              nodesConnectable={false}
              colorMode="system"
            >
              <Background gap={18} size={1} />
              <Controls />
            </ReactFlow>
          ) : (
            <div
              className="flex h-full items-center justify-center text-sm text-fg-subtle"
              data-testid="graph-placeholder"
            >
              読み込み中…
            </div>
          )}
        </div>
      </Card>
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
        <p className="mt-2 text-sm text-fg-muted">{data.detail}</p>
      </main>
    );
  }
  return (
    <main className="p-4">
      <h1 className="text-xl font-semibold">エラー</h1>
      <p className="mt-2 text-sm text-fg-muted">予期しないエラーが起きました。</p>
    </main>
  );
}
