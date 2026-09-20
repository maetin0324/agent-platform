import { useRef, useState } from "react";
import { data, redirect, useFetcher } from "react-router";
import type { CreateFailure } from "~/celeris/action-types";
import type { CelerisClient } from "~/celeris/client.server";
import { getCelerisClient } from "~/celeris/client.server";
import { celerisErrorResponse } from "~/celeris/errors";
import { formString } from "~/celeris/forms";
import { createTask } from "~/celeris/route-actions.server";
import type { ConfigView, CriterionSpec, NewTaskSpec, TaskList } from "~/celeris/types";
import { ErrorFlash, FieldErrors } from "~/components/Flash";
import { StatusBadge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import {
  checkboxClass,
  chipLabelClass,
  hintClass,
  inputClass,
  labelClass,
  selectClass,
  textareaClass,
} from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { EmptyState, PageHeader } from "~/components/ui/misc";
import { cn } from "~/lib/utils";
import type { Route } from "./+types/tasks.new";

/**
 * `/tasks/new`（タスク作成、docs/DESIGN.md §4.4）。フォームは `NewTaskSpec` と 1:1。
 * 検証は celeris（task-ops）が行い、422 の `errors[]` をそのままフィールドの下に出す
 * （docs/celeris-api-v1.md §1.5, §3.4）。GUI 側の検証はしない。
 */

const KIND_OPTIONS: { value: NonNullable<NewTaskSpec["kind"]>; label: string }[] = [
  { value: "execute", label: "execute" },
  { value: "approval", label: "approval" },
  { value: "review", label: "review" },
  { value: "plan", label: "plan" },
];

const TIER_OPTIONS: { value: NonNullable<NewTaskSpec["tier"]>; label: string }[] = [
  { value: "standard", label: "standard" },
  { value: "frontier", label: "frontier" },
  { value: "cheap", label: "cheap" },
];

const CRITERION_TYPES: { value: CriterionSpec["type"]; label: string }[] = [
  { value: "human", label: "Human" },
  { value: "command", label: "Command" },
  { value: "artifact_exists", label: "ArtifactExists" },
  { value: "reviewer", label: "Reviewer" },
];

export interface NewTaskData {
  candidates: TaskList;
  config: ConfigView;
}

/**
 * `GET /tasks`（`limit=500`, `order=created_desc`）で depends_on / parent の候補一覧を、
 * `GET /config` で `workspace_root` 等の表示用の設定を取る（並列）。`CelerisClient` を引数に取ることで
 * テスト可能にする（`app/routes/tasks.tsx` の `loadTasks` と同じ形）。
 */
export async function loadNewTask(client: CelerisClient, request: Request): Promise<NewTaskData> {
  const [candidates, config] = await Promise.all([
    client.get<TaskList>("/tasks", { query: { limit: 500, order: "created_desc" }, signal: request.signal }),
    client.get<ConfigView>("/config", { signal: request.signal }),
  ]);
  return { candidates, config };
}

export async function loader({ request }: Route.LoaderArgs): Promise<NewTaskData> {
  try {
    return await loadNewTask(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "タスク作成 - Celeris" }];
}

/** `form` の文字列値（無ければ `undefined`）。空文字は `""` のまま返す（title/objective 用）。 */
function rawString(form: FormData, name: string): string {
  const v = form.get(name);
  return typeof v === "string" ? v : "";
}

/** 数値欄。空・非数値なら `undefined`（省略。celeris の既定を使う）。 */
function numberField(form: FormData, name: string): number | undefined {
  const v = formString(form, name);
  if (v === null) return undefined;
  const n = Number(v);
  return Number.isNaN(n) ? undefined : n;
}

/**
 * 受け入れ条件ビルダーの行を `criterion_type[i]` / `criterion_value[i]`（`form.getAll` で index を揃える）
 * から組み立てる。値が空白だけの行は送らない（全行空なら `acceptance: []` になり celeris が 422 を返す）。
 */
export function buildCriteria(form: FormData): CriterionSpec[] {
  const types = form.getAll("criterion_type").map((v) => String(v));
  const values = form.getAll("criterion_value").map((v) => String(v));
  const len = Math.min(types.length, values.length);
  const criteria: CriterionSpec[] = [];
  for (let i = 0; i < len; i += 1) {
    const value = values[i].trim();
    if (value === "") continue;
    switch (types[i]) {
      case "human":
        criteria.push({ type: "human", text: value });
        break;
      case "reviewer":
        criteria.push({ type: "reviewer", text: value });
        break;
      case "command":
        criteria.push({ type: "command", cmd: value, expect_exit: 0 });
        break;
      case "artifact_exists":
        criteria.push({ type: "artifact_exists", name: value });
        break;
      default:
        break;
    }
  }
  return criteria;
}

/**
 * `depends_on` チェックボックス（存在する候補）と `depends_on_extra`（空白 / カンマ区切りの id。
 * 存在しない id も指定できるようにするための自由入力欄）を結合し、重複を除く。
 */
export function buildDependsOn(form: FormData): string[] {
  const checked = form.getAll("depends_on").map((v) => String(v));
  const extra = formString(form, "depends_on_extra");
  const extraIds = extra
    ? extra
        .split(/[\s,]+/)
        .map((s) => s.trim())
        .filter((s) => s !== "")
    : [];
  return Array.from(new Set([...checked, ...extraIds]));
}

/**
 * フォーム全体を `NewTaskSpec` にする（純関数、テスト可能）。空の欄は本文から省く
 * （celeris の既定を使う。docs/celeris-api-v1.md §3.4）。`title` / `objective` は空でも必須フィールドとして送り、
 * celeris の 422 文言をそのまま出す（`required` 属性は付けない）。
 */
export function buildNewTaskSpec(form: FormData): NewTaskSpec {
  const spec: NewTaskSpec = {
    title: rawString(form, "title"),
    objective: rawString(form, "objective"),
    acceptance: buildCriteria(form),
  };

  const kind = formString(form, "kind");
  if (kind) spec.kind = kind as NewTaskSpec["kind"];

  const tier = formString(form, "tier");
  if (tier) spec.tier = tier as NewTaskSpec["tier"];

  const adapter = formString(form, "adapter");
  if (adapter) spec.adapter = adapter;

  const priority = numberField(form, "priority");
  if (priority !== undefined) spec.priority = priority;

  const role = formString(form, "role");
  if (role) spec.role = role;

  const genre = formString(form, "genre");
  if (genre) spec.genre = genre;

  if (form.get("aggregate") != null) spec.aggregate = true;

  const parent = formString(form, "parent");
  if (parent) spec.parent = parent;

  const dependsOn = buildDependsOn(form);
  if (dependsOn.length > 0) spec.depends_on = dependsOn;

  const maxTurns = numberField(form, "max_turns");
  if (maxTurns !== undefined) spec.max_turns = maxTurns;

  const maxWallSecs = numberField(form, "max_wall_secs");
  if (maxWallSecs !== undefined) spec.max_wall_secs = maxWallSecs;

  const maxRetries = numberField(form, "max_retries");
  if (maxRetries !== undefined) spec.max_retries = maxRetries;

  const workspace = formString(form, "workspace");
  if (workspace) spec.workspace = workspace;

  return spec;
}

export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const spec = buildNewTaskSpec(form);
  const result = await createTask(getCelerisClient(), spec, request.signal);
  if (result.ok) {
    return redirect(`/tasks/${result.task.id}`);
  }
  return data(result satisfies CreateFailure, { status: result.error.status });
}

interface CriterionRow {
  id: number;
  type: CriterionSpec["type"];
  value: string;
}

export default function NewTaskPage({ loaderData }: Route.ComponentProps) {
  const { candidates, config } = loaderData;
  // 失敗（422 等）が SSE の再検証で消えないよう fetcher に載せる（監査 H1）。成功は action の redirect で移る。
  const fetcher = useFetcher<CreateFailure>();
  const submitting = fetcher.state !== "idle";
  const error = fetcher.data && !fetcher.data.ok ? fetcher.data.error : undefined;

  const nextRowId = useRef(1);
  const [rows, setRows] = useState<CriterionRow[]>(() => [{ id: 0, type: "human", value: "" }]);

  // 分野（genre、ADR-0027 D1）を選ぶと、その分野の description と所属する role を表示する
  // （role 欄そのものは自由記述のまま。celeris 側が検証する。ADR-0005 D5）。
  const genres = config.genres ?? [];
  const [selectedGenreId, setSelectedGenreId] = useState("");
  const selectedGenre = genres.find((g) => g.id === selectedGenreId);
  const capabilities = selectedGenre?.capabilities ?? [];
  const inputArtifacts = selectedGenre?.input_artifacts ?? [];
  const outputArtifacts = selectedGenre?.output_artifacts ?? [];

  function addRow() {
    const id = nextRowId.current;
    nextRowId.current += 1;
    setRows((prev) => [...prev, { id, type: "human", value: "" }]);
  }

  function removeRow(id: number) {
    setRows((prev) => prev.filter((row) => row.id !== id));
  }

  function updateRow(id: number, patch: Partial<Omit<CriterionRow, "id">>) {
    setRows((prev) => prev.map((row) => (row.id === id ? { ...row, ...patch } : row)));
  }

  return (
    <div className="space-y-6">
      <PageHeader
        icon="plus"
        title="タスク作成"
        description="NewTaskSpec を celeris にそのまま送信します。入力の検証は celeris 側で行われます。"
      />
      <ErrorFlash error={error} />

      <fetcher.Form method="post" data-testid="new-task-form" className="space-y-6">
        <Card>
          <CardHeader icon="file" title="基本" description="タスクの名前と目的" />
          <CardBody className="space-y-4">
            <div>
              <label htmlFor="title" className={labelClass}>
                title
              </label>
              <input id="title" name="title" type="text" className={cn(inputClass, "mt-1.5")} />
              <p className={cn(hintClass, "mt-1")}>タスク一覧・詳細に表示される短い名前。</p>
              <FieldErrors error={error} field="title" />
            </div>

            <div>
              <label htmlFor="objective" className={labelClass}>
                objective
              </label>
              <textarea id="objective" name="objective" rows={4} className={cn(textareaClass, "mt-1.5 w-full")} />
              <p className={cn(hintClass, "mt-1")}>担当するエージェントに渡す目的の説明。</p>
              <FieldErrors error={error} field="objective" />
            </div>
          </CardBody>
        </Card>

        <Card>
          <CardHeader icon="checkCircle" title="受け入れ条件" description="celeris が完了を判定する条件（1 つ以上）" />
          <CardBody className="space-y-3">
            <fieldset className="space-y-2">
              <legend className="sr-only">受け入れ条件</legend>
              {rows.map((row) => (
                <div
                  key={row.id}
                  data-testid="criterion-row"
                  className="flex flex-col gap-2 rounded-lg border border-border bg-surface-2/50 p-3 sm:flex-row sm:items-center"
                >
                  <select
                    name="criterion_type"
                    aria-label="受け入れ条件の種類"
                    value={row.type}
                    onChange={(e) => updateRow(row.id, { type: e.target.value as CriterionSpec["type"] })}
                    className={cn(selectClass, "sm:w-44")}
                  >
                    {CRITERION_TYPES.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                  <input
                    name="criterion_value"
                    aria-label="受け入れ条件の内容"
                    type="text"
                    value={row.value}
                    onChange={(e) => updateRow(row.id, { value: e.target.value })}
                    className={cn(inputClass, "flex-1")}
                    placeholder={row.type === "command" ? "cargo test" : "text"}
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    data-testid="remove-criterion"
                    onClick={() => removeRow(row.id)}
                    className="self-end sm:self-auto"
                  >
                    <Icon name="x" />
                    削除
                  </Button>
                </div>
              ))}
            </fieldset>
            <Button type="button" variant="secondary" size="sm" data-testid="add-criterion" onClick={addRow}>
              <Icon name="plus" />
              条件を追加
            </Button>
            <FieldErrors error={error} field="acceptance" />
          </CardBody>
        </Card>

        <Card>
          <CardHeader icon="settings" title="分類・設定" description="種別・優先度・担当ロールなど" />
          <CardBody>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
              <div>
                <label htmlFor="kind" className={labelClass}>
                  kind
                </label>
                <select id="kind" name="kind" defaultValue="execute" className={cn(selectClass, "mt-1.5 w-full")}>
                  {KIND_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="tier" className={labelClass}>
                  tier
                </label>
                <select id="tier" name="tier" defaultValue="standard" className={cn(selectClass, "mt-1.5 w-full")}>
                  {TIER_OPTIONS.map((opt) => (
                    <option key={opt.value} value={opt.value}>
                      {opt.label}
                    </option>
                  ))}
                </select>
              </div>
              <div>
                <label htmlFor="adapter" className={labelClass}>
                  adapter
                </label>
                <input id="adapter" name="adapter" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
              </div>
              <div>
                <label htmlFor="priority" className={labelClass}>
                  priority
                </label>
                <input id="priority" name="priority" type="number" className={cn(inputClass, "mt-1.5 w-full")} />
              </div>
              <div className="col-span-2">
                <label htmlFor="role" className={labelClass}>
                  role
                </label>
                <input
                  id="role"
                  name="role"
                  type="text"
                  list="role-options"
                  data-testid="role-input"
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
                <datalist id="role-options">
                  {(config.roles ?? []).map((r) => (
                    <option key={r.id} value={r.id} />
                  ))}
                </datalist>
              </div>
              <div className="col-span-2">
                <label htmlFor="genre" className={labelClass}>
                  genre
                </label>
                {genres.length > 0 ? (
                  <select
                    id="genre"
                    name="genre"
                    data-testid="genre-select"
                    defaultValue=""
                    onChange={(e) => setSelectedGenreId(e.target.value)}
                    className={cn(selectClass, "mt-1.5 w-full")}
                  >
                    <option value="">(なし)</option>
                    {genres.map((g) => (
                      <option key={g.id} value={g.id}>
                        {g.id}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    id="genre"
                    name="genre"
                    type="text"
                    data-testid="genre-input"
                    className={cn(inputClass, "mt-1.5 w-full")}
                  />
                )}
                <div className={cn(hintClass, "mt-1 space-y-1")} data-testid="genre-hint">
                  {selectedGenre ? (
                    <>
                      <p>{`${selectedGenre.description}（roles: ${selectedGenre.roles.join(", ") || "-"}）`}</p>
                      {capabilities.length > 0 && (
                        <p data-testid="genre-capabilities">できること: {capabilities.join(" / ")}</p>
                      )}
                      {(inputArtifacts.length > 0 || outputArtifacts.length > 0) && (
                        <p className="flex flex-wrap items-center gap-x-1">
                          {inputArtifacts.length > 0 && (
                            <span data-testid="genre-input-artifacts">渡すもの: {inputArtifacts.join(", ")}</span>
                          )}
                          {inputArtifacts.length > 0 && outputArtifacts.length > 0 && <span aria-hidden="true">→</span>}
                          {outputArtifacts.length > 0 && (
                            <span data-testid="genre-output-artifacts">返るもの: {outputArtifacts.join(", ")}</span>
                          )}
                        </p>
                      )}
                    </>
                  ) : (
                    <p>分野ごとのハーネス・役割の入口（ADR-0027 D1）。role はこの一覧に関わらず自由記述で送れます。</p>
                  )}
                </div>
                <FieldErrors error={error} field="genre" />
              </div>
              <div className="col-span-2 sm:col-span-4">
                <label htmlFor="aggregate" className={chipLabelClass}>
                  <input
                    id="aggregate"
                    name="aggregate"
                    type="checkbox"
                    data-testid="aggregate-checkbox"
                    className={checkboxClass}
                  />
                  aggregate（委譲した子が全て終わったら集約 run を 1 回行う）
                </label>
              </div>
            </div>
          </CardBody>
        </Card>

        <Card>
          <CardHeader
            icon="gitBranch"
            title="依存・ワークスペース"
            description="親タスク・依存タスク・作業ディレクトリ"
          />
          <CardBody className="space-y-4">
            <div>
              <label htmlFor="parent" className={labelClass}>
                parent（id）
              </label>
              <input id="parent" name="parent" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
              <FieldErrors error={error} field="parent" />
            </div>

            <fieldset>
              <legend className={labelClass}>depends_on</legend>
              {candidates.items.length === 0 ? (
                <EmptyState className="mt-2 py-4" title="候補はありません。" />
              ) : (
                <div className="mt-1.5 max-h-48 space-y-1 overflow-y-auto rounded-lg border border-border p-2">
                  {candidates.items.map((item) => (
                    <label
                      key={item.id}
                      className="flex items-center gap-2 rounded-md px-1.5 py-1 text-sm text-fg hover:bg-surface-2/60"
                    >
                      <input type="checkbox" name="depends_on" value={item.id} className={checkboxClass} />
                      <span className="min-w-0 flex-1 truncate">{item.title}</span>
                      <StatusBadge status={item.status} />
                    </label>
                  ))}
                </div>
              )}
              <label htmlFor="depends_on_extra" className={cn(hintClass, "mt-2 block")}>
                追加の依存 id（空白またはカンマ区切り）
              </label>
              <input
                id="depends_on_extra"
                name="depends_on_extra"
                type="text"
                className={cn(inputClass, "mt-1.5 w-full")}
              />
              <FieldErrors error={error} field="depends_on" />
            </fieldset>

            <div>
              <label htmlFor="workspace" className={labelClass}>
                workspace
              </label>
              <input id="workspace" name="workspace" type="text" className={cn(inputClass, "mt-1.5 w-full")} />
              <p className={cn(hintClass, "mt-1")}>
                workspace は <code className="font-mono">{config.workspace_root}</code> からの相対パス（空ならタスク
                id）。
              </p>
            </div>
          </CardBody>
        </Card>

        <Card>
          <CardHeader icon="clock" title="予算" description="実行のターン数・時間・再試行の上限" />
          <CardBody>
            <div className="grid grid-cols-2 gap-4 sm:grid-cols-3">
              <div>
                <label htmlFor="max_turns" className={labelClass}>
                  max_turns
                </label>
                <input id="max_turns" name="max_turns" type="number" className={cn(inputClass, "mt-1.5 w-full")} />
              </div>
              <div>
                <label htmlFor="max_wall_secs" className={labelClass}>
                  max_wall_secs
                </label>
                <input
                  id="max_wall_secs"
                  name="max_wall_secs"
                  type="number"
                  className={cn(inputClass, "mt-1.5 w-full")}
                />
              </div>
              <div>
                <label htmlFor="max_retries" className={labelClass}>
                  max_retries
                </label>
                <input id="max_retries" name="max_retries" type="number" className={cn(inputClass, "mt-1.5 w-full")} />
              </div>
            </div>
          </CardBody>
        </Card>

        <div className="flex justify-end">
          <Button type="submit" variant="primary" size="md" data-testid="submit" disabled={submitting}>
            <Icon name="send" />
            作成
          </Button>
        </div>
      </fetcher.Form>
    </div>
  );
}
