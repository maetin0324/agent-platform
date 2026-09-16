import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef, useState } from "react";
import { Form, Link, useFetcher, useSearchParams } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { taskdErrorResponse } from "~/taskd/errors";
import type { Status, TaskList, TaskSummary } from "~/taskd/types";
import type { Route } from "./+types/tasks";

/**
 * `/tasks`（一覧、docs/DESIGN.md §4.2）。`GET /tasks` の応答をそのまま使う（フィルタ・並び替え・ページングは
 * taskd に丸投げ。GUI は再計算しない）。taskd 停止中・エラーは `Response` に変換して投げ、root の
 * ErrorBoundary がバナー等を出す（docs/adr/0003 D4、docs/adr/0004 D6）。
 */

const ALL_STATUSES: Status[] = ["draft", "ready", "running", "blocked", "reviewing", "done", "failed", "cancelled"];

const ORDERS = [
  { value: "updated_desc", label: "更新が新しい順" },
  { value: "dispatch", label: "ディスパッチ順" },
  { value: "created_desc", label: "作成が新しい順" },
] as const;

const ROW_HEIGHT_PX = 48;
const SCROLL_HEIGHT_PX = 480;

/**
 * `request.url` の検索パラメータを `GET /tasks` のクエリにそのまま転送する（docs/taskd-api-v1.md §3.3）。
 * `TaskdClient` を引数に取ることでテスト可能にする（`app/taskd/health.server.ts` の `loadHealth` と同じ形）。
 */
export async function loadTasks(client: TaskdClient, request: Request): Promise<TaskList> {
  const params = new URL(request.url).searchParams;
  return client.get<TaskList>("/tasks", {
    query: {
      status: params.getAll("status"),
      kind: params.getAll("kind"),
      parent: params.get("parent") ?? undefined,
      root_only: params.get("root_only") ?? undefined,
      q: params.get("q") ?? undefined,
      order: params.get("order") ?? undefined,
      limit: params.get("limit") ?? undefined,
      cursor: params.get("cursor") ?? undefined,
    },
    signal: request.signal,
  });
}

export async function loader({ request }: Route.LoaderArgs): Promise<TaskList> {
  try {
    return await loadTasks(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "タスク一覧 - taskd-gui" }];
}

export default function TasksPage({ loaderData }: Route.ComponentProps) {
  const taskList = loaderData;
  const [searchParams] = useSearchParams();
  const fetcher = useFetcher<TaskList>();

  const [items, setItems] = useState<TaskSummary[]>(taskList.items);
  const [nextCursor, setNextCursor] = useState<string | null>(taskList.next_cursor ?? null);

  // フィルタ・並び順が変わって loader が新しい初期ページを返したら、蓄積分をリセットする。
  // `taskList` は SSE（`useTaskdStream`、root で 1 本）による再検証のたびに新しい参照になるが、
  // 中身（1 ページ目の id 列と next_cursor）が同じなら「さらに読む」で蓄積した分を消してはいけない
  // （そうしないと、taskd が動き続ける限り定期的に再検証が走り、蓄積したページが失われ続けてしまう）。
  const firstPageKey = `${taskList.items.map((item) => item.id).join(",")}|${taskList.next_cursor ?? ""}`;
  const lastAppliedKey = useRef<string | null>(null);
  useEffect(() => {
    if (lastAppliedKey.current === firstPageKey) return;
    lastAppliedKey.current = firstPageKey;
    setItems(taskList.items);
    setNextCursor(taskList.next_cursor ?? null);
  }, [firstPageKey, taskList]);

  // 「さらに読む」で取得した追加ページを既存の items に追記する（URL は変えない）。
  // SSE による再検証は route の loader だけでなく、読み込み済みの fetcher も再取得の対象にするため
  // （React Router の revalidate() の仕様）、同じ cursor のページが再取得されて `fetcher.data` の参照が
  // 変わることがある。中身が前回追記した分と同じなら追記しない（二重追記防止。firstPageKey と同じ理由）。
  const lastAppliedFetcherKey = useRef<string | null>(null);
  useEffect(() => {
    const page = fetcher.data;
    if (!page) return;
    const key = `${page.items.map((item) => item.id).join(",")}|${page.next_cursor ?? ""}`;
    if (lastAppliedFetcherKey.current === key) return;
    lastAppliedFetcherKey.current = key;
    setItems((prev) => [...prev, ...page.items]);
    setNextCursor(page.next_cursor ?? null);
  }, [fetcher.data]);

  const handleLoadMore = (): void => {
    if (!nextCursor) return;
    const params = new URLSearchParams(searchParams);
    params.set("cursor", nextCursor);
    fetcher.load(`/tasks?${params.toString()}`);
  };

  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT_PX,
    overscan: 10,
  });

  const selectedStatuses = new Set(searchParams.getAll("status"));

  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-lg font-semibold">
          タスク一覧
          <HelpLink anchor="screens" label="画面ごとの説明" />
        </h2>
        <p data-testid="tasks-total" className="text-sm text-gray-600">
          {taskList.total} 件
        </p>
        <ul className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs text-gray-500" data-testid="tasks-counts-by-status">
          {Object.entries(taskList.counts_by_status).map(([status, count]) => (
            <li key={status}>
              {status}: {count}
            </li>
          ))}
        </ul>
      </div>

      <Form method="get" className="space-y-2 rounded border p-3 text-sm" data-testid="tasks-filter-form">
        <fieldset>
          <legend className="font-semibold">status</legend>
          <div className="flex flex-wrap gap-3">
            {ALL_STATUSES.map((status) => (
              <label key={status} className="flex items-center gap-1">
                <input type="checkbox" name="status" value={status} defaultChecked={selectedStatuses.has(status)} />
                {status}
              </label>
            ))}
          </div>
        </fieldset>
        <div className="flex flex-wrap items-center gap-3">
          <label className="flex items-center gap-1">
            q:
            <input type="text" name="q" defaultValue={searchParams.get("q") ?? ""} maxLength={200} />
          </label>
          <label className="flex items-center gap-1">
            order:
            <select name="order" defaultValue={searchParams.get("order") ?? "updated_desc"}>
              {ORDERS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </label>
          <button type="submit" className="rounded border px-2 py-1">
            絞り込み
          </button>
        </div>
      </Form>

      <div
        ref={scrollRef}
        data-testid="task-list-scroll"
        className="overflow-auto rounded border"
        style={{ height: SCROLL_HEIGHT_PX }}
      >
        <div style={{ height: virtualizer.getTotalSize(), position: "relative", width: "100%" }}>
          {virtualizer.getVirtualItems().map((virtualRow) => {
            const item = items[virtualRow.index];
            if (!item) return null;
            return (
              <div
                key={item.id}
                data-testid="task-row"
                data-task-id={item.id}
                className="absolute left-0 top-0 flex w-full items-center gap-4 border-b px-2 text-sm"
                style={{ height: virtualRow.size, transform: `translateY(${virtualRow.start}px)` }}
              >
                <span className="w-56 truncate font-mono text-xs">{item.id}</span>
                <Link to={`/tasks/${item.id}`} className="flex-1 truncate hover:underline">
                  {item.title}
                </Link>
                <span className="w-20">{item.status}</span>
                <span className="w-20">{item.kind}</span>
                <span className="w-12 text-right">{item.priority}</span>
                <span className="w-44 text-xs text-gray-500">{item.updated_at}</span>
              </div>
            );
          })}
        </div>
      </div>

      {nextCursor !== null && (
        <button
          type="button"
          data-testid="load-more"
          onClick={handleLoadMore}
          disabled={fetcher.state !== "idle"}
          className="rounded border px-3 py-1 text-sm"
        >
          さらに読む
        </button>
      )}
    </div>
  );
}
