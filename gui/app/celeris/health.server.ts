import type { CelerisClient } from "./client.server";
import { CelerisError, isCelerisUnavailable } from "./errors";
import type { Health } from "./types";

export interface HealthState {
  health: Health | null;
  /** celeris に届かなかった（接続拒否・タイムアウト） */
  unavailable: boolean;
  /** 届いたが問題応答だった場合の要約（例: `401 unauthorized`）。トークンや本文は含めない */
  problem: string | null;
}

/** root loader 用。CelerisUnavailable / CelerisError は例外にせず状態として返す（docs/DESIGN.md §6.5: 500 にしない）。 */
export async function loadHealth(client: CelerisClient, signal?: AbortSignal): Promise<HealthState> {
  try {
    return { health: await client.health({ signal }), unavailable: false, problem: null };
  } catch (e) {
    if (isCelerisUnavailable(e)) return { health: null, unavailable: true, problem: null };
    if (e instanceof CelerisError) return { health: null, unavailable: false, problem: `${e.status} ${e.code}` };
    throw e;
  }
}
