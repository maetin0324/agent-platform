import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef, useState } from "react";
import { Form, Link, useFetcher, useSearchParams } from "react-router";
import { HelpLink } from "~/components/HelpLink";
import { KindBadge, RoleLabel, StatusBadge, statusTone } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { checkboxClass, chipLabelClass, inputClass, labelClass, selectClass, theadClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { EmptyState, PageHeader } from "~/components/ui/misc";
import { TONE_SOFT, TONE_SOLID_BG } from "~/components/ui/tone";
import { buildTaskPlacements, type TaskPlacement } from "~/lib/project-index";
import { cn } from "~/lib/utils";
import { isSupportTask } from "~/lib/work-tree";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { taskdErrorResponse } from "~/taskd/errors";
import type { ConfigView, OrgList, ProjectDetail, ProjectList, Status, TaskList, TaskSummary } from "~/taskd/types";
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

const ROW_HEIGHT_PX = 56;
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
      genre: params.getAll("genre"),
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

export interface TasksData {
  tasks: TaskList;
  /** `GET /config` の `genres[]`（ADR-0027 D1）を絞り込みの選択肢に使う。 */
  config: ConfigView;
  /** タスク id → どの案件・どの途中目標か（監査 M2「裏方から戻れる」。`~/lib/project-index.ts`）。 */
  placements: Record<string, TaskPlacement>;
  /** 担当 id → 名前（`GET /org`。落ちても一覧は出す）。 */
  assigneeNames: Record<string, string>;
}

/**
 * `/tasks` の loader 本体。`GET /tasks` と `GET /config` を並列に呼ぶ（`config` は分野の絞り込み UI 用。
 * `tasks.new.tsx` の `loadNewTask` と同じ形）。
 */
/**
 * 裏方のタスクから案件・途中目標へ戻るための索引（監査 M2）。`TaskSummary` に `project_id` が無いので
 * `GET /projects` + 各案件の `GET /projects/{id}` を束ねる（`app/routes/artifacts.tsx` と同じく、taskd への
 * 問い合わせは loader に閉じた私的ヘルパーにする。公開関数から `.server.ts` を参照するとクライアント
 * バンドルからのサーバコード除去に引っかかるため）。**落ちても一覧は出す**（索引が空になるだけ）。
 * taskd 側に `TaskSummary.project_id` が入ったら、この束ねはやめられる（`docs/taskd-requests.md` R3）。
 */
async function loadTaskPlacements(
  client: TaskdClient,
  signal: AbortSignal | undefined,
): Promise<Record<string, TaskPlacement>> {
  try {
    const list = await client.get<ProjectList>("/projects", { signal });
    const details = await Promise.all(
      list.items.map((p) =>
        client
          .get<ProjectDetail>(`/projects/${encodeURIComponent(p.id)}`, { signal })
          .catch(() => null as ProjectDetail | null),
      ),
    );
    return buildTaskPlacements(details.filter((d): d is ProjectDetail => d !== null));
  } catch {
    return {};
  }
}

export async function loadTasksPage(client: TaskdClient, request: Request): Promise<TasksData> {
  const [tasks, config, placements, org] = await Promise.all([
    loadTasks(client, request),
    client.get<ConfigView>("/config", { signal: request.signal }),
    loadTaskPlacements(client, request.signal),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
  ]);
  const assigneeNames = Object.fromEntries(org.items.map((n) => [n.id, n.name]));
  return { tasks, config, placements, assigneeNames };
}

export async function loader({ request }: Route.LoaderArgs): Promise<TasksData> {
  try {
    return await loadTasksPage(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "タスク一覧 - taskd-gui" }];
}

export default function TasksPage({ loaderData }: Route.ComponentProps) {
  const { tasks: taskList, config, placements, assigneeNames } = loaderData;
  const genres = config.genres ?? [];
  const [searchParams] = useSearchParams();
  const fetcher = useFetcher<TaskList>();

  const [items, setItems] = useState<TaskSummary[]>(taskList.items);
  const [nextCursor, setNextCursor] = useState<string | null>(taskList.next_cursor ?? null);

  // 裏方のタスク（`TaskSummary.support`: 対話・報告のまとめ・承認待ち・レビュー。Phase 29）は既定で隠す
  // （SPEC「タスクは裏方」/ ADR-0033 D8）。`show_support=1` はここだけの表示切り替えで、`GET /tasks` には
  // 送らない（taskd に絞り込みは無い。`loadTasks` が転送するクエリの一覧に含めていないので taskd 側には
  // 届かない）。ページングは裏方を含めた元の `items` に対して行い、表示だけをこの真偽値でフィルタする。
  const showSupport = searchParams.get("show_support") === "1";
  const visibleItems = showSupport ? items : items.filter((item) => !isSupportTask(item));

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
    count: visibleItems.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT_PX,
    overscan: 10,
  });

  const selectedStatuses = new Set(searchParams.getAll("status"));
  const selectedGenres = new Set(searchParams.getAll("genre"));

  return (
    <div className="space-y-6">
      <PageHeader
        as="h2"
        icon="list"
        title={
          <>
            タスク一覧
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="taskd に登録されたタスクを状態・種別・キーワードで絞り込んで確認します（GET /tasks をそのまま表示）。"
      />

      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <p data-testid="tasks-total" className="text-sm font-semibold text-fg">
          {taskList.total} 件
        </p>
        <ul className="flex flex-wrap gap-1.5" data-testid="tasks-counts-by-status">
          {Object.entries(taskList.counts_by_status).map(([status, count]) => (
            <li
              key={status}
              className={cn(
                "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium",
                TONE_SOFT[statusTone(status)],
              )}
            >
              <span
                aria-hidden="true"
                className={cn("size-1.5 shrink-0 rounded-full", TONE_SOLID_BG[statusTone(status)])}
              />
              {status}: <span className="tabular-nums font-semibold">{count}</span>
            </li>
          ))}
        </ul>
      </div>

      <Card>
        <CardHeader
          icon="filter"
          title="絞り込み"
          description="status・キーワード・並び順を指定して GET /tasks に転送します（GUI 側では再計算しません）。"
        />
        <CardBody>
          <Form method="get" className="space-y-4" data-testid="tasks-filter-form">
            <fieldset>
              <legend className={labelClass}>status</legend>
              <div className="mt-2 flex flex-wrap gap-2">
                {ALL_STATUSES.map((status) => (
                  <label key={status} className={chipLabelClass}>
                    <input
                      type="checkbox"
                      name="status"
                      value={status}
                      defaultChecked={selectedStatuses.has(status)}
                      className={checkboxClass}
                    />
                    {status}
                  </label>
                ))}
              </div>
            </fieldset>
            <fieldset data-testid="genre-filter">
              <legend className={labelClass}>genre</legend>
              {genres.length > 0 ? (
                <div className="mt-2 flex flex-wrap gap-2">
                  {genres.map((g) => (
                    <label
                      key={g.id}
                      className={chipLabelClass}
                      title={
                        g.capabilities && g.capabilities.length > 0
                          ? `${g.description}\nできること: ${g.capabilities.join(" / ")}`
                          : g.description
                      }
                    >
                      <input
                        type="checkbox"
                        name="genre"
                        value={g.id}
                        defaultChecked={selectedGenres.has(g.id)}
                        className={checkboxClass}
                      />
                      {g.id}
                    </label>
                  ))}
                </div>
              ) : (
                <input
                  type="text"
                  name="genre"
                  data-testid="genre-filter-input"
                  defaultValue={searchParams.get("genre") ?? ""}
                  className={cn(inputClass, "mt-2 w-56")}
                />
              )}
            </fieldset>
            <div className="flex flex-wrap items-end gap-3">
              <label className="flex items-center gap-2 text-sm text-fg-muted">
                q:
                <span className="relative">
                  <Icon
                    name="search"
                    className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-fg-subtle"
                  />
                  <input
                    type="text"
                    name="q"
                    defaultValue={searchParams.get("q") ?? ""}
                    maxLength={200}
                    className={cn(inputClass, "w-56 pl-8")}
                  />
                </span>
              </label>
              <label className="flex items-center gap-2 text-sm text-fg-muted">
                order:
                <select
                  name="order"
                  defaultValue={searchParams.get("order") ?? "updated_desc"}
                  className={cn(selectClass, "w-44")}
                >
                  {ORDERS.map((o) => (
                    <option key={o.value} value={o.value}>
                      {o.label}
                    </option>
                  ))}
                </select>
              </label>
              {/* 裏方のタスク（対話の返事・報告のまとめ・承認待ち・レビュー）は既定で隠す（SPEC「タスクは裏方」）。
                  taskd には絞り込みが無いので、表示だけを GUI 側で `TaskSummary.support` で切り替える。 */}
              <label className={chipLabelClass}>
                <input
                  type="checkbox"
                  name="show_support"
                  value="1"
                  data-testid="tasks-show-support"
                  defaultChecked={showSupport}
                  className={checkboxClass}
                />
                裏方も表示
              </label>
              <Button type="submit" variant="primary" size="sm">
                <Icon name="filter" />
                絞り込み
              </Button>
            </div>
          </Form>
        </CardBody>
      </Card>

      <Card className="overflow-hidden">
        <div className={cn(theadClass, "flex items-center gap-4 border-b border-border px-4 py-2.5 sm:px-5")}>
          <span className="min-w-0 flex-1">タイトル</span>
          <span className="w-28 shrink-0">状態</span>
          <span className="w-20 shrink-0">種別</span>
          <span className="w-20 shrink-0">役割</span>
          <span className="w-32 shrink-0">案件</span>
          <span className="w-28 shrink-0">担当</span>
          <span className="w-32 shrink-0">途中目標</span>
          <span className="w-28 shrink-0">更新日時</span>
        </div>
        <div
          ref={scrollRef}
          data-testid="task-list-scroll"
          className="overflow-auto"
          style={{ height: SCROLL_HEIGHT_PX }}
        >
          {visibleItems.length === 0 ? (
            <div className="flex h-full items-center justify-center p-6">
              <EmptyState icon="list" title="タスクが見つかりません">
                {items.length > 0 && !showSupport
                  ? "裏方のタスクしかありません。上の「裏方も表示」を付けてください。"
                  : "条件を変えて絞り込んでください。"}
              </EmptyState>
            </div>
          ) : (
            <div style={{ height: virtualizer.getTotalSize(), position: "relative", width: "100%" }}>
              {virtualizer.getVirtualItems().map((virtualRow) => {
                const item = visibleItems[virtualRow.index];
                if (!item) return null;
                return (
                  <div
                    key={item.id}
                    data-testid="task-row"
                    data-task-id={item.id}
                    className="absolute left-0 top-0 flex w-full items-center gap-4 border-b border-border px-4 text-sm transition-colors hover:bg-surface-2/60 sm:px-5"
                    style={{ height: virtualRow.size, transform: `translateY(${virtualRow.start}px)` }}
                  >
                    <Link
                      to={`/tasks/${item.id}`}
                      title={item.id}
                      className="min-w-0 flex-1 truncate font-medium text-fg no-underline hover:text-primary hover:underline"
                    >
                      {item.title}
                    </Link>
                    <span className="w-28 shrink-0">
                      <StatusBadge status={item.status} />
                    </span>
                    <span className="w-20 shrink-0">
                      <KindBadge kind={item.kind} />
                    </span>
                    {/* 役割（ADR-0016 D1、taskd-requests R2）。色分けはせずテキストのラベルだけ。役割なしは空欄。 */}
                    <span className="w-20 shrink-0 truncate" data-testid="task-role" title={item.role ?? ""}>
                      {item.role ? <RoleLabel role={item.role} /> : ""}
                    </span>
                    {/* 案件・担当・途中目標（監査 M2「裏方から戻れる」）。分野は詳細（/tasks/:id）で見る。 */}
                    <span className="w-32 shrink-0 truncate" data-testid="task-project">
                      {placements[item.id] ? (
                        <Link
                          to={`/projects/${placements[item.id].projectId}`}
                          className="underline underline-offset-2"
                          title={placements[item.id].projectTitle}
                        >
                          {placements[item.id].projectTitle}
                        </Link>
                      ) : (
                        <span className="text-fg-subtle">-</span>
                      )}
                    </span>
                    <span className="w-28 shrink-0 truncate" data-testid="task-assignee">
                      {item.assignee ? (
                        <Link
                          to={
                            item.assignee === "secretary"
                              ? "/org/secretary"
                              : `/org/${encodeURIComponent(item.assignee)}`
                          }
                          className="underline underline-offset-2"
                          title={item.assignee}
                        >
                          {assigneeNames[item.assignee] ?? item.assignee}
                        </Link>
                      ) : (
                        <span className="text-fg-subtle">-</span>
                      )}
                    </span>
                    <span
                      className="w-32 shrink-0 truncate text-xs text-fg-muted"
                      data-testid="task-milestone"
                      title={placements[item.id]?.milestoneTitle ?? ""}
                    >
                      {placements[item.id]?.milestoneTitle ?? "-"}
                    </span>
                    <span className="w-28 shrink-0 truncate text-xs text-fg-subtle" title={item.updated_at}>
                      {item.updated_at}
                    </span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </Card>

      {nextCursor !== null && (
        <div className="flex justify-center">
          <Button
            type="button"
            data-testid="load-more"
            variant="secondary"
            size="sm"
            onClick={handleLoadMore}
            disabled={fetcher.state !== "idle"}
          >
            <Icon name="chevronDown" />
            さらに読む
          </Button>
        </div>
      )}
    </div>
  );
}
