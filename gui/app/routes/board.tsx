import { useState } from "react";
import { data, Form, isRouteErrorResponse, Link, useFetcher, useNavigation, useSearchParams } from "react-router";
import type { TaskEditOutcome } from "~/celeris/action-types";
import type { CelerisClient } from "~/celeris/client.server";
import { getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { formString } from "~/celeris/forms";
import { buildTaskEdit, editTask } from "~/celeris/tasks-admin.server";
import type {
  MilestoneView,
  OrgList,
  OrgNode,
  ProjectDetail,
  ProjectList,
  TaskList,
  TaskSummary,
} from "~/celeris/types";
import { ErrorFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Badge, StatusBadge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import {
  checkboxClass,
  chipLabelClass,
  inputClass,
  labelClass,
  selectClass,
  touchLinkClass,
} from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, EmptyState, PageHeader } from "~/components/ui/misc";
import { Skeleton } from "~/components/ui/skeleton";
import {
  BOARD_COLUMNS,
  type BoardColumnId,
  boardFilterIsEmpty,
  boardFilterToQuery,
  groupByColumn,
  PRIORITY_LABELS,
  parseBoardFilter,
  summaryPriorityLabel,
} from "~/lib/board";
import {
  boardColumnLabel,
  MILESTONE_PAUSED_BANNER,
  PROJECT_CANCELLED_BANNER,
  PROJECT_PAUSED_BANNER,
  priorityFullLabel,
  TASK_CATEGORIES,
  TIERS,
  taskCategoryLabel,
  tierLabel,
} from "~/lib/labels";
import { milestoneIsPaused, projectIsPaused } from "~/lib/lifecycle";
import { isLiveStatusScreen } from "~/lib/live-status";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { cn } from "~/lib/utils";
import { isSupportTask } from "~/lib/work-tree";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/board";

/**
 * `/board`（ボード、ADR-0044 D4）。案件を選んで、6 列（待ち・進行中・止まっている・完了・失敗・中止）で見る。
 *
 * - **URL がそのまま状態**（`project` / `label` / `category` / `assignee` / `milestone` / `tier` / `priority` / `q`）。
 *   絞り込みは `GET /tasks` にそのまま転送する（GUI 側で絞り直さない）。
 * - 列の束ね方と並び（優先度の降順 → `created_at` の昇順）は `~/lib/board.ts` の純関数。
 * - カードの上で優先度・レベル・担当をその場で変えられる（`PATCH /tasks/{id}`。ADR-0044 D1）。
 * - **ドラッグで列を跨がせない**（状態は状態機械の仕事。ADR-0044 D4 の「採らない」）。
 */

/** 1 度に読む上限。ボードは列に全部出すのでページングはしない（超えたら画面に断りを出す）。 */
const BOARD_LIMIT = 500;

export interface BoardData {
  tasks: TaskList;
  projects: ProjectList;
  /** 選んだ案件の途中目標（絞り込みの選択肢とカードの表示）。案件未選択のときは空。 */
  milestones: MilestoneView[];
  /** 担当の名前とプルダウンの選択肢（`GET /org`。落ちてもボードは出す）。 */
  org: OrgNode[];
}

/**
 * `/board` の loader 本体。検索パラメータを `BoardFilter` に読み、`GET /tasks` のクエリへ写す
 * （`app/routes/tasks.tsx` の `loadTasks` と同じ形。`CelerisClient` を引数に取ってテスト可能にする）。
 */
export async function loadBoard(client: CelerisClient, request: Request): Promise<BoardData> {
  const params = new URL(request.url).searchParams;
  const filter = parseBoardFilter(params);
  const [tasks, projects, org] = await Promise.all([
    client.get<TaskList>("/tasks", {
      query: { ...boardFilterToQuery(filter), limit: BOARD_LIMIT, order: "created_desc" },
      signal: request.signal,
    }),
    client.get<ProjectList>("/projects", { signal: request.signal }).catch(() => ({ items: [] }) as ProjectList),
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
  ]);
  // 途中目標は案件を選んだときだけ（絞り込みの選択肢とカードの表示。落ちても空で出す）。
  const milestones = filter.project
    ? await client
        .get<ProjectDetail>(`/projects/${encodeURIComponent(filter.project)}`, { signal: request.signal })
        .then((d) => d.milestones)
        .catch(() => [] as MilestoneView[])
    : [];
  return { tasks, projects, milestones, org: org.items };
}

export async function loader({ request }: Route.LoaderArgs): Promise<BoardData> {
  try {
    return await loadBoard(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "ボード - Celeris" }];
}

export const shouldRevalidate = revalidateAfterActionErrors;

/** カードの行内編集（ADR-0044 D1 / D4）。送るのは変えた 1 項目だけ（`TaskEdit` は「省略 = 変えない」）。 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const taskId = formString(form, "task_id");
  if (form.get("intent") !== "edit" || !taskId) {
    throw data({ error: "unknown intent" }, { status: 400 });
  }
  const outcome = await editTask(getCelerisClient(), taskId, buildTaskEdit(form), request.signal);
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

export default function BoardPage({ loaderData }: Route.ComponentProps) {
  const { tasks, projects, milestones, org } = loaderData;
  // フェーズ 71（ADR-0055 D2 ラウンド 3）: モバイルは 6 列を縦積みにすると 1 画面に収まらないので、
  // 上部の segmented control で 1 列だけ選んで全幅で見せる（`lg:` はこれまでどおりの列グリッド）。
  // ADR-0055 D3「D2 の直し方の規律」の「一度に 1 画面ずつ」に合わせ、選択肢はこの画面のカードと
  // 同じ 6 列（横スワイプの行より、絞り込みフォームと同じ「選ぶ」操作に揃えた方が一貫すると判断した。
  // 詳細は `docs/PROGRESS.md` Phase 71 を参照）。
  const [activeColumn, setActiveColumn] = useState<BoardColumnId>(BOARD_COLUMNS[0].id);
  const [searchParams] = useSearchParams();
  const filter = parseBoardFilter(searchParams);
  // Phase 77（ADR-0055 D3「体感速度」）: 絞り込みフォーム（`board-filter-form`、method="get"）を送ると
  // `/board` への再ナビゲーションになり、新しい `loaderData` が届くまで直前の列がそのまま残る。その間は
  // 列をスケルトンに差し替える（グリッドの列数はそのまま。カードの枚数は前回のものを概算に使うので
  // 高さのガタつきは小さい）。
  const navigation = useNavigation();
  const isBoardNavigationPending = navigation.state === "loading" && navigation.location?.pathname === "/board";
  // 裏方のタスク（`TaskSummary.support`: 対話・報告のまとめ・承認待ち・レビュー）は既定で隠す
  // （SPEC「タスクは裏方」/ ADR-0033 D8。`/tasks` と同じ扱い）。判定は celeris の `support` をそのまま使い、
  // `show_support=1` は表示の切り替えだけ（`GET /tasks` には送らない。celeris に絞り込みが無い）。
  const showSupport = searchParams.get("show_support") === "1";
  const visibleItems = showSupport ? tasks.items : tasks.items.filter((t) => !isSupportTask(t));
  const columns = groupByColumn(visibleItems);
  const orgNames = Object.fromEntries(org.map((n) => [n.id, n.name]));
  const milestoneNames = Object.fromEntries(milestones.map((m) => [m.id, `#${m.seq} ${m.title}`]));
  const truncated = tasks.items.length >= BOARD_LIMIT;
  // ADR-0044 D6（Phase 55 / G19）: 止まっている案件・途中目標を選んでいるときは、
  // 「新しい仕事は始まらない」ことをボードの上で言う（状態は celeris が返したものをそのまま読む）。
  const selectedProject = filter.project ? projects.items.find((p) => p.id === filter.project) : undefined;
  const selectedMilestone = filter.milestone ? milestones.find((m) => m.id === filter.milestone) : undefined;

  return (
    <div className="space-y-6">
      <PageHeader
        icon="layers"
        title={
          <>
            ボード
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="案件のタスクを状態ごとに並べて見ます。カードの上で優先度・レベル・担当をその場で変えられます（状態は状態機械が決めるので、ドラッグでは動かせません）。"
      />

      {/* 一時停止・中止・アーカイブ（ADR-0044 D6、Phase 55 / G19）。選んだ案件・途中目標が止まっていたら、
          カードが並んでいても新しい仕事は始まらないので必ず断る。 */}
      {selectedProject && projectIsPaused(selectedProject) && (
        <Alert tone="warning" data-testid="board-project-paused">
          {PROJECT_PAUSED_BANNER}
        </Alert>
      )}
      {selectedProject?.status === "cancelled" && (
        <Alert tone="warning" data-testid="board-project-cancelled">
          {PROJECT_CANCELLED_BANNER}
        </Alert>
      )}
      {selectedMilestone && milestoneIsPaused(selectedMilestone) && (
        <Alert tone="warning" data-testid="board-milestone-paused">
          {MILESTONE_PAUSED_BANNER}
        </Alert>
      )}

      <Card>
        <CardHeader icon="filter" title="絞り込み" description="ここで選んだ条件はそのまま URL になります。" />
        <CardBody>
          <Form method="get" className="space-y-4" data-testid="board-filter-form">
            <div className="flex flex-wrap items-end gap-3">
              <div>
                <label htmlFor="board-project" className={labelClass}>
                  案件
                </label>
                <select
                  id="board-project"
                  name="project"
                  defaultValue={filter.project ?? ""}
                  data-testid="board-project"
                  className={cn(selectClass, "mt-1.5 w-56")}
                >
                  <option value="">すべての案件</option>
                  {projects.items.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.title}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="board-assignee" className={labelClass}>
                  担当
                </label>
                <select
                  id="board-assignee"
                  name="assignee"
                  defaultValue={filter.assignee ?? ""}
                  data-testid="board-assignee"
                  className={cn(selectClass, "mt-1.5 w-44")}
                >
                  <option value="">すべての担当</option>
                  {org.map((node) => (
                    <option key={node.id} value={node.id}>
                      {node.name}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="board-milestone" className={labelClass}>
                  途中目標
                </label>
                <select
                  id="board-milestone"
                  name="milestone"
                  defaultValue={filter.milestone ?? ""}
                  data-testid="board-milestone"
                  className={cn(selectClass, "mt-1.5 w-52")}
                >
                  <option value="">すべての途中目標</option>
                  {milestones
                    .slice()
                    .sort((a, b) => a.seq - b.seq)
                    .map((m) => (
                      <option key={m.id} value={m.id}>
                        #{m.seq} {m.title}
                      </option>
                    ))}
                </select>
              </div>
              <div>
                <label htmlFor="board-label" className={labelClass}>
                  ラベル
                </label>
                <input
                  id="board-label"
                  type="text"
                  name="label"
                  defaultValue={filter.labels[0] ?? ""}
                  placeholder="例: pluvio"
                  data-testid="board-label"
                  className={cn(inputClass, "mt-1.5 w-40")}
                />
              </div>
              <div>
                <label htmlFor="board-q" className={labelClass}>
                  検索
                </label>
                <input
                  id="board-q"
                  type="text"
                  name="q"
                  maxLength={200}
                  defaultValue={filter.q ?? ""}
                  placeholder="題名・目的・コメント"
                  data-testid="board-q"
                  className={cn(inputClass, "mt-1.5 w-56")}
                />
              </div>
            </div>

            <fieldset data-testid="board-category-filter">
              <legend className={labelClass}>種類</legend>
              <div className="mt-1.5 flex flex-wrap gap-2">
                {TASK_CATEGORIES.map((c) => (
                  <label key={c} className={chipLabelClass}>
                    <input
                      type="checkbox"
                      name="category"
                      value={c}
                      defaultChecked={filter.categories.includes(c)}
                      className={checkboxClass}
                    />
                    {taskCategoryLabel(c)}
                  </label>
                ))}
              </div>
            </fieldset>

            <fieldset data-testid="board-priority-filter">
              <legend className={labelClass}>優先度</legend>
              <div className="mt-1.5 flex flex-wrap gap-2">
                {PRIORITY_LABELS.map((p) => (
                  <label key={p} className={chipLabelClass}>
                    <input
                      type="checkbox"
                      name="priority"
                      value={p}
                      defaultChecked={filter.priorities.includes(p)}
                      className={checkboxClass}
                    />
                    {priorityFullLabel(p)}
                  </label>
                ))}
              </div>
            </fieldset>

            <fieldset data-testid="board-tier-filter">
              <legend className={labelClass}>レベル</legend>
              <div className="mt-1.5 flex flex-wrap gap-2">
                {TIERS.map((t) => (
                  <label key={t} className={chipLabelClass}>
                    <input
                      type="checkbox"
                      name="tier"
                      value={t}
                      defaultChecked={filter.tiers.includes(t)}
                      className={checkboxClass}
                    />
                    {tierLabel(t)}
                  </label>
                ))}
              </div>
            </fieldset>

            <div className="flex flex-wrap items-center gap-2">
              {/* 裏方のタスク（対話の返事・報告のまとめ・承認待ち・レビュー）は既定で隠す。
                  celeris には絞り込みが無いので、表示だけを `TaskSummary.support` で切り替える。 */}
              <label className={chipLabelClass}>
                <input
                  type="checkbox"
                  name="show_support"
                  value="1"
                  data-testid="board-show-support"
                  defaultChecked={showSupport}
                  className={checkboxClass}
                />
                裏方も表示
              </label>
              <Button type="submit" variant="primary" size="sm" data-testid="board-filter-submit">
                <Icon name="filter" />
                絞り込み
              </Button>
              <Link to="/board" className={cn(touchLinkClass, "text-sm text-fg-muted underline underline-offset-2")}>
                条件を消す
              </Link>
            </div>
          </Form>
        </CardBody>
      </Card>

      {truncated && (
        // ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
        <p className="text-sm text-fg-subtle lg:text-xs" data-testid="board-truncated">
          多すぎるので先頭 {BOARD_LIMIT} 件だけ出しています。案件やラベルで絞ってください。
        </p>
      )}

      {visibleItems.length === 0 && (
        <EmptyState icon="layers" title="出せるタスクがありません" data-testid="board-empty">
          {boardFilterIsEmpty(filter)
            ? showSupport
              ? "タスクがまだありません。案件の画面から「タスクを追加」してください。"
              : "人が見る仕事がありません（裏方のタスクしかない可能性があります）。上の「裏方も表示」を付けてください。"
            : "条件に合うタスクがありません。条件を変えてください。"}
        </EmptyState>
      )}

      {/* ADR-0055 D2 ラウンド 4（U10）: 6 択は 393px では横スクロールでは 1 画面に入らない（6 番目が
          隠れがちだった）ので、横スクロールをやめて 3 列 × 2 行のグリッドにし、6 つ全部を一度に見える
          ようにした（`md:` 以上は列グリッドがそのまま出るので不要）。ラベルが狭い列幅で折り返しても
          横はみ出しにはならない（折り返す分だけ縦に伸びる）。念のため `title` にも全文を持たせる。 */}
      <div
        role="tablist"
        aria-label="ボードの列を選ぶ"
        data-testid="board-column-picker"
        className="sticky top-14 z-10 -mx-4 -mt-2 grid grid-cols-3 gap-1.5 bg-bg/95 px-4 py-2 backdrop-blur md:hidden"
      >
        {BOARD_COLUMNS.map((column) => {
          const active = column.id === activeColumn;
          const count = columns[column.id].length;
          const label = boardColumnLabel(column.id);
          return (
            <button
              key={column.id}
              type="button"
              role="tab"
              aria-selected={active}
              title={label}
              data-testid="board-column-picker-item"
              onClick={() => setActiveColumn(column.id)}
              className={cn(
                "flex min-h-11 min-w-0 items-center justify-center gap-1 rounded-full border px-2 text-sm font-medium",
                active
                  ? "border-primary-border bg-primary-soft text-primary-soft-fg"
                  : "border-border bg-surface text-fg-muted",
              )}
            >
              <span className="min-w-0 truncate">{label}</span>
              <span
                className={cn(
                  "shrink-0 rounded-full px-1.5 py-0.5 text-sm tabular-nums",
                  active ? "bg-surface/70" : "bg-surface-2 text-fg-subtle",
                )}
              >
                {count}
              </span>
            </button>
          );
        })}
      </div>

      <div
        className="grid gap-4 md:grid-cols-2 xl:grid-cols-3"
        data-testid="board-columns"
        aria-busy={isBoardNavigationPending || undefined}
      >
        {isBoardNavigationPending
          ? BOARD_COLUMNS.map((column) => (
              <BoardColumnSkeleton
                key={column.id}
                id={column.id}
                cardCount={columns[column.id].length}
                active={column.id === activeColumn}
              />
            ))
          : BOARD_COLUMNS.map((column) => (
              <BoardColumn
                key={column.id}
                id={column.id}
                items={columns[column.id]}
                orgNames={orgNames}
                milestoneNames={milestoneNames}
                org={org}
                active={column.id === activeColumn}
              />
            ))}
      </div>
    </div>
  );
}

function BoardColumn({
  id,
  items,
  orgNames,
  milestoneNames,
  org,
  active,
}: {
  id: BoardColumnId;
  items: TaskSummary[];
  orgNames: Record<string, string>;
  milestoneNames: Record<string, string>;
  org: OrgNode[];
  /** この列が今 segmented control で選ばれているか（`md:` 未満だけで効く。D2 の picker と対）。 */
  active: boolean;
}) {
  return (
    <section
      aria-label={boardColumnLabel(id)}
      data-testid="board-column"
      data-column={id}
      className={cn("rounded-xl border border-border bg-surface-2/40 p-3", active ? "block" : "hidden", "md:block")}
    >
      {/* モバイルは segmented control が列名を出すので、列の中の見出しは `md:` 以上だけに絞る
          （二重に出さない）。`md:` からは元どおり sticky（縦スクロールしても見出しが見える）。 */}
      <h2 className="hidden items-center gap-2 text-sm font-semibold text-fg md:sticky md:top-14 md:z-10 md:-mx-3 md:-mt-3 md:flex md:bg-surface-2/90 md:px-3 md:py-2 md:backdrop-blur">
        {boardColumnLabel(id)}
        {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
        <span className="rounded-full bg-surface px-2 py-0.5 text-sm font-semibold tabular-nums text-fg-subtle lg:text-xs">
          {items.length}
        </span>
      </h2>
      {items.length === 0 ? (
        <p className="mt-3 text-sm text-fg-subtle lg:text-xs">ありません。</p>
      ) : (
        <ul className="mt-3 space-y-2">
          {items.map((item) => (
            <BoardCard key={item.id} item={item} orgNames={orgNames} milestoneNames={milestoneNames} org={org} />
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * `BoardColumn` の読み込み中プレースホルダ（Phase 77、ADR-0055 D3「体感速度」）。前回のカード枚数
 * （`cardCount`）ぶんだけ出して、列の高さが差し替え前後でだいたい揃うようにする（0〜4 枚に丸めて、
 * 空でも真っ平らにならないようにする）。
 */
function BoardColumnSkeleton({ id, cardCount, active }: { id: BoardColumnId; cardCount: number; active: boolean }) {
  const rows = Math.min(Math.max(cardCount, 1), 4);
  return (
    <section
      aria-hidden="true"
      data-testid="board-column-skeleton"
      data-column={id}
      className={cn("rounded-xl border border-border bg-surface-2/40 p-3", active ? "block" : "hidden", "md:block")}
    >
      <div className="hidden items-center gap-2 md:flex">
        <Skeleton className="h-4 w-20" />
      </div>
      <ul className="mt-3 space-y-2">
        {Array.from({ length: rows }, (_, i) => `${id}-${i}`).map((key) => (
          <li key={key} className="space-y-2 rounded-lg border border-border bg-surface p-3">
            <Skeleton className="h-4 w-2/3" />
            <Skeleton className="h-3 w-1/3" />
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * カード 1 枚（ADR-0044 D4）。題名・担当・レベル・優先度・ラベル・種類・途中目標を出し、
 * 優先度・レベル・担当は選んだ瞬間に `PATCH /tasks/{id}` を送る（`useFetcher`。画面遷移はしない）。
 *
 * フェーズ 71（ADR-0055 D2 ラウンド 3）: モバイルは「折り目の上」を題名・状態（1 語）・優先度・担当だけに
 * 絞る。種類・レベル・途中目標・ラベル・行内編集は 2 番目の情報として折りたたみ（`~/components/Console.tsx`
 * の `ScopePicker` と同じ「`lg:` は常に開き、それ未満は state で開閉する」作り。要素を複製しない）。
 */
function BoardCard({
  item,
  orgNames,
  milestoneNames,
  org,
}: {
  item: TaskSummary;
  orgNames: Record<string, string>;
  milestoneNames: Record<string, string>;
  org: OrgNode[];
}) {
  const fetcher = useFetcher<TaskEditOutcome>({ key: `board-edit-${item.id}` });
  const busy = fetcher.state !== "idle";
  const [detailsOpen, setDetailsOpen] = useState(false);
  // 終端のタスクは celeris が 409 を返す（ADR-0044 D1）ので、行内編集そのものを出さない。
  const editable = item.actions.includes("edit");
  const priority = summaryPriorityLabel(item);

  function submitField(name: string, value: string) {
    fetcher.submit({ intent: "edit", task_id: item.id, [name]: value }, { method: "post", action: "/board" });
  }

  return (
    <li
      data-testid="board-card"
      data-task-id={item.id}
      data-status={item.status}
      className="rounded-lg border border-border bg-surface p-3 shadow-xs"
    >
      {/* Phase 75（ADR-0055 D2 ラウンド 7）: カードの余白・間隔を 4/8/12/16 のスケールに揃える
          （6px の gap-1.5 をやめて gap-2 = 8px に）。 */}
      <div className="flex flex-wrap items-center gap-2">
        {/* U-G32-2 の解消（Phase 84）: ボードは開いたまま SSE の再検証で更新され続ける画面なので、
            状態バッジをライブリージョンにする（`~/lib/live-status.ts` が画面ごとに判断を集約）。 */}
        <StatusBadge status={item.status} role={isLiveStatusScreen("board") ? "status" : undefined} />
        <Badge tone="primary" data-testid="board-card-priority">
          {priority}
        </Badge>
      </div>
      <Link
        to={`/tasks/${item.id}`}
        data-testid="board-card-title"
        title={item.title}
        className="mt-2 flex min-h-11 items-center font-medium text-fg no-underline hover:text-primary hover:underline"
      >
        {/* 題名は 2 行までに丸め、全文は `title` 属性（ホバー）に残す。`overflow-wrap: anywhere` で
            日本語混じりの長い題名（id やパスを含む等）が単語の途中でも折り返せるようにする
            （`line-clamp-2` と両立させるため、`flex` を持つ Link 自身ではなく内側の `span` に掛ける）。 */}
        <span className="line-clamp-2 min-w-0 [overflow-wrap:anywhere]">{item.title}</span>
      </Link>
      {/* ADR-0055 D1-4: モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。 */}
      <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-fg-subtle lg:text-xs">
        <span data-testid="board-card-assignee">
          担当: {item.assignee ? (orgNames[item.assignee] ?? item.assignee) : "（なし）"}
        </span>
      </div>

      <button
        type="button"
        onClick={() => setDetailsOpen((v) => !v)}
        data-testid="board-card-more-toggle"
        className="mt-2 flex min-h-11 items-center gap-1 text-sm text-fg-subtle underline underline-offset-2 lg:hidden"
      >
        {detailsOpen ? "閉じる" : "種類・レベル・ラベルを見る"}
      </button>

      <div className={cn("mt-2 space-y-2 lg:mt-1.5 lg:block", detailsOpen ? "block" : "hidden")}>
        <div className="flex flex-wrap items-center gap-2">
          <Badge tone="neutral" data-testid="board-card-category">
            {taskCategoryLabel(item.category)}
          </Badge>
          <Badge tone="neutral" data-testid="board-card-tier">
            {item.tier}
          </Badge>
          {item.milestone_id && (
            <span className="text-sm text-fg-subtle lg:text-xs" data-testid="board-card-milestone">
              途中目標: {milestoneNames[item.milestone_id] ?? item.milestone_id}
            </span>
          )}
        </div>
        {item.labels.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {item.labels.map((label) => (
              <Badge key={label} tone="teal" data-testid="board-card-label">
                {label}
              </Badge>
            ))}
          </div>
        )}
        {editable && (
          <div className="flex flex-wrap gap-2" data-testid="board-card-edit">
            <select
              aria-label={`${item.title} の優先度`}
              value={priority}
              disabled={busy}
              data-testid="board-card-priority-select"
              onChange={(e) => submitField("priority", e.target.value)}
              className={cn(selectClass, "h-11 w-24 text-sm lg:h-7 lg:text-xs")}
            >
              {PRIORITY_LABELS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
            <select
              aria-label={`${item.title} のレベル`}
              value={item.tier}
              disabled={busy}
              data-testid="board-card-tier-select"
              onChange={(e) => submitField("tier", e.target.value)}
              className={cn(selectClass, "h-11 w-28 text-sm lg:h-7 lg:text-xs")}
            >
              {TIERS.map((t) => (
                <option key={t} value={t}>
                  {t}
                </option>
              ))}
            </select>
            <select
              aria-label={`${item.title} の担当`}
              value={item.assignee ?? ""}
              disabled={busy}
              data-testid="board-card-assignee-select"
              onChange={(e) => submitField("assignee", e.target.value)}
              className={cn(selectClass, "h-11 w-36 text-sm lg:h-7 lg:text-xs")}
            >
              <option value="">（決めない）</option>
              {org.map((node) => (
                <option key={node.id} value={node.id}>
                  {node.name}
                </option>
              ))}
            </select>
          </div>
        )}
      </div>
      {fetcher.data && !fetcher.data.ok && <ErrorFlash error={fetcher.data.error} />}
    </li>
  );
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const errorData = error.data as CelerisRouteErrorData;
    if (errorData.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={errorData.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="mx-auto max-w-2xl space-y-3 p-6">
        <h1 className="text-xl font-semibold text-fg">エラー {errorData.status}</h1>
        <EmptyState icon="alert" title={errorData.detail ?? "読み込めませんでした"} />
      </main>
    );
  }
  return (
    <main className="mx-auto max-w-2xl space-y-3 p-6">
      <h1 className="text-xl font-semibold text-fg">エラー</h1>
      <EmptyState icon="alert" title="予期しないエラーが起きました。" />
    </main>
  );
}
