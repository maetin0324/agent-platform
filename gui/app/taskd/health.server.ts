import type { TaskdClient } from "./client.server";
import { isTaskdUnavailable, TaskdError } from "./errors";
import type { Health } from "./types";

export interface HealthState {
  health: Health | null;
  /** taskd に届かなかった（接続拒否・タイムアウト） */
  unavailable: boolean;
  /** 届いたが問題応答だった場合の要約（例: `401 unauthorized`）。トークンや本文は含めない */
  problem: string | null;
}

/** root loader 用。TaskdUnavailable / TaskdError は例外にせず状態として返す（docs/DESIGN.md §6.5: 500 にしない）。 */
export async function loadHealth(client: TaskdClient, signal?: AbortSignal): Promise<HealthState> {
  try {
    return { health: await client.health({ signal }), unavailable: false, problem: null };
  } catch (e) {
    if (isTaskdUnavailable(e)) return { health: null, unavailable: true, problem: null };
    if (e instanceof TaskdError) return { health: null, unavailable: false, problem: `${e.status} ${e.code}` };
    throw e;
  }
}
