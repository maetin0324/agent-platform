import type { EventsPage, Inbox, TaskDetail, TaskList } from "~/taskd/types";
import inboxFixture from "./api/inbox.json";
import taskDetailFixture from "./api/task-detail.json";
import taskEventsFixture from "./api/task-events.json";
import tasksFixture from "./api/tasks.json";

/**
 * `scripts/capture-fixtures.sh` が実 taskd から採取した JSON が生成型（`app/taskd/types.ts`）の形と
 * 一致することを `pnpm typecheck` で検証するだけのファイル（docs/DESIGN.md §10 Phase G1）。実行はしない。
 *
 * `resolveJsonModule` は JSON の文字列値を `string`（列挙のリテラル型ではなく）に広げて型推論するため、
 * `Widen<T>` で生成型側も同じ規則（文字列系は `string` に広げる）で緩め、フィールド名・入れ子構造の一致だけを
 * 検証する（`docs/taskd-api-v1.md` の列挙値そのものの正しさまでは検証しない。フィールド抜け・typo の検出が目的）。
 */
type Widen<T> = T extends string
  ? string
  : T extends (infer U)[]
    ? Widen<U>[]
    : T extends object
      ? { [K in keyof T]: Widen<T[K]> }
      : T;

export const _inboxFixture: Widen<Inbox> = inboxFixture;
export const _tasksFixture: Widen<TaskList> = tasksFixture;
export const _taskDetailFixture: Widen<TaskDetail> = taskDetailFixture;
export const _taskEventsFixture: Widen<EventsPage> = taskEventsFixture;
