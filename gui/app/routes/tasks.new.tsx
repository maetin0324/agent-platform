import { useRef, useState } from "react";
import { data, Form, redirect, useNavigation } from "react-router";
import { ErrorFlash, FieldErrors } from "~/components/Flash";
import type { CreateFailure } from "~/taskd/action-types";
import type { TaskdClient } from "~/taskd/client.server";
import { getTaskdClient } from "~/taskd/client.server";
import { taskdErrorResponse } from "~/taskd/errors";
import { formString } from "~/taskd/forms";
import { createTask } from "~/taskd/route-actions.server";
import type { ConfigView, CriterionSpec, NewTaskSpec, TaskList } from "~/taskd/types";
import type { Route } from "./+types/tasks.new";

/**
 * `/tasks/new`（タスク作成、docs/DESIGN.md §4.4）。フォームは `NewTaskSpec` と 1:1。
 * 検証は taskd（task-ops）が行い、422 の `errors[]` をそのままフィールドの下に出す
 * （docs/taskd-api-v1.md §1.5, §3.4）。GUI 側の検証はしない。
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
 * `GET /config` で `workspace_root` 等の表示用の設定を取る（並列）。`TaskdClient` を引数に取ることで
 * テスト可能にする（`app/routes/tasks.tsx` の `loadTasks` と同じ形）。
 */
export async function loadNewTask(client: TaskdClient, request: Request): Promise<NewTaskData> {
  const [candidates, config] = await Promise.all([
    client.get<TaskList>("/tasks", { query: { limit: 500, order: "created_desc" }, signal: request.signal }),
    client.get<ConfigView>("/config", { signal: request.signal }),
  ]);
  return { candidates, config };
}

export async function loader({ request }: Route.LoaderArgs): Promise<NewTaskData> {
  try {
    return await loadNewTask(getTaskdClient(), request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "タスク作成 - taskd-gui" }];
}

/** `form` の文字列値（無ければ `undefined`）。空文字は `""` のまま返す（title/objective 用）。 */
function rawString(form: FormData, name: string): string {
  const v = form.get(name);
  return typeof v === "string" ? v : "";
}

/** 数値欄。空・非数値なら `undefined`（省略。taskd の既定を使う）。 */
function numberField(form: FormData, name: string): number | undefined {
  const v = formString(form, name);
  if (v === null) return undefined;
  const n = Number(v);
  return Number.isNaN(n) ? undefined : n;
}

/**
 * 受け入れ条件ビルダーの行を `criterion_type[i]` / `criterion_value[i]`（`form.getAll` で index を揃える）
 * から組み立てる。値が空白だけの行は送らない（全行空なら `acceptance: []` になり taskd が 422 を返す）。
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
 * （taskd の既定を使う。docs/taskd-api-v1.md §3.4）。`title` / `objective` は空でも必須フィールドとして送り、
 * taskd の 422 文言をそのまま出す（`required` 属性は付けない）。
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
  const result = await createTask(getTaskdClient(), spec, request.signal);
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

export default function NewTaskPage({ loaderData, actionData }: Route.ComponentProps) {
  const { candidates, config } = loaderData;
  const navigation = useNavigation();
  const submitting = navigation.state !== "idle";
  const error = actionData && !actionData.ok ? actionData.error : undefined;

  const nextRowId = useRef(1);
  const [rows, setRows] = useState<CriterionRow[]>(() => [{ id: 0, type: "human", value: "" }]);

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
      <h1 className="text-xl font-semibold">タスク作成</h1>
      <ErrorFlash error={error} />

      <Form method="post" data-testid="new-task-form" className="space-y-6">
        <div>
          <label htmlFor="title" className="block text-sm font-medium">
            title
          </label>
          <input id="title" name="title" type="text" className="mt-1 w-full rounded border px-2 py-1 text-sm" />
          <FieldErrors error={error} field="title" />
        </div>

        <div>
          <label htmlFor="objective" className="block text-sm font-medium">
            objective
          </label>
          <textarea id="objective" name="objective" rows={4} className="mt-1 w-full rounded border px-2 py-1 text-sm" />
          <FieldErrors error={error} field="objective" />
        </div>

        <fieldset>
          <legend className="text-sm font-medium">受け入れ条件</legend>
          <div className="mt-2 space-y-2">
            {rows.map((row) => (
              <div key={row.id} data-testid="criterion-row" className="flex items-center gap-2">
                <select
                  name="criterion_type"
                  aria-label="受け入れ条件の種類"
                  value={row.type}
                  onChange={(e) => updateRow(row.id, { type: e.target.value as CriterionSpec["type"] })}
                  className="rounded border px-2 py-1 text-sm"
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
                  className="flex-1 rounded border px-2 py-1 text-sm"
                  placeholder={row.type === "command" ? "cargo test" : "text"}
                />
                <button
                  type="button"
                  data-testid="remove-criterion"
                  onClick={() => removeRow(row.id)}
                  className="rounded border px-2 py-1 text-sm text-gray-600"
                >
                  削除
                </button>
              </div>
            ))}
          </div>
          <button
            type="button"
            data-testid="add-criterion"
            onClick={addRow}
            className="mt-2 rounded border px-2 py-1 text-sm"
          >
            条件を追加
          </button>
          <FieldErrors error={error} field="acceptance" />
        </fieldset>

        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <div>
            <label htmlFor="kind" className="block text-sm font-medium">
              kind
            </label>
            <select
              id="kind"
              name="kind"
              defaultValue="execute"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            >
              {KIND_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label htmlFor="tier" className="block text-sm font-medium">
              tier
            </label>
            <select
              id="tier"
              name="tier"
              defaultValue="standard"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            >
              {TIER_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label htmlFor="adapter" className="block text-sm font-medium">
              adapter
            </label>
            <input id="adapter" name="adapter" type="text" className="mt-1 w-full rounded border px-2 py-1 text-sm" />
          </div>
          <div>
            <label htmlFor="priority" className="block text-sm font-medium">
              priority
            </label>
            <input
              id="priority"
              name="priority"
              type="number"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>
          <div>
            <label htmlFor="role" className="block text-sm font-medium">
              role
            </label>
            <input
              id="role"
              name="role"
              type="text"
              list="role-options"
              data-testid="role-input"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
            <datalist id="role-options">
              {(config.roles ?? []).map((r) => (
                <option key={r.id} value={r.id} />
              ))}
            </datalist>
          </div>
          <div className="flex items-center gap-2 sm:col-span-1">
            <input id="aggregate" name="aggregate" type="checkbox" data-testid="aggregate-checkbox" />
            <label htmlFor="aggregate" className="text-sm font-medium">
              aggregate（委譲した子が全て終わったら集約 run を 1 回行う）
            </label>
          </div>
        </div>

        <div>
          <label htmlFor="parent" className="block text-sm font-medium">
            parent（id）
          </label>
          <input id="parent" name="parent" type="text" className="mt-1 w-full rounded border px-2 py-1 text-sm" />
          <FieldErrors error={error} field="parent" />
        </div>

        <fieldset>
          <legend className="text-sm font-medium">depends_on</legend>
          {candidates.items.length === 0 ? (
            <p className="mt-1 text-sm text-gray-500">候補はありません。</p>
          ) : (
            <div className="mt-1 max-h-48 space-y-1 overflow-y-auto rounded border p-2 text-sm">
              {candidates.items.map((item) => (
                <label key={item.id} className="flex items-center gap-2">
                  <input type="checkbox" name="depends_on" value={item.id} />
                  {item.title}（{item.status}）
                </label>
              ))}
            </div>
          )}
          <label htmlFor="depends_on_extra" className="mt-2 block text-xs text-gray-500">
            追加の依存 id（空白またはカンマ区切り）
          </label>
          <input
            id="depends_on_extra"
            name="depends_on_extra"
            type="text"
            className="mt-1 w-full rounded border px-2 py-1 text-sm"
          />
          <FieldErrors error={error} field="depends_on" />
        </fieldset>

        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <div>
            <label htmlFor="max_turns" className="block text-sm font-medium">
              max_turns
            </label>
            <input
              id="max_turns"
              name="max_turns"
              type="number"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>
          <div>
            <label htmlFor="max_wall_secs" className="block text-sm font-medium">
              max_wall_secs
            </label>
            <input
              id="max_wall_secs"
              name="max_wall_secs"
              type="number"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>
          <div>
            <label htmlFor="max_retries" className="block text-sm font-medium">
              max_retries
            </label>
            <input
              id="max_retries"
              name="max_retries"
              type="number"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>
          <div>
            <label htmlFor="workspace" className="block text-sm font-medium">
              workspace
            </label>
            <input
              id="workspace"
              name="workspace"
              type="text"
              className="mt-1 w-full rounded border px-2 py-1 text-sm"
            />
          </div>
        </div>
        <p className="text-xs text-gray-500">
          workspace は <code>{config.workspace_root}</code> からの相対パス（空ならタスク id）。
        </p>

        <button
          type="submit"
          data-testid="submit"
          disabled={submitting}
          className="rounded border bg-gray-900 px-4 py-2 text-sm text-white disabled:opacity-50"
        >
          作成
        </button>
      </Form>
    </div>
  );
}
