import type { Health, ReleaseItem, ReleasePromoteAccepted, Releases } from "~/taskd/types";

/**
 * `GET /health` の既定応答（docs/taskd-api-v1.md §3.1）。`Health` 型で宣言することで形を検証する。
 */
export const defaultHealth: Health = {
  api_version: "1",
  schema_version: 4,
  taskd_version: "0.1.0",
  instance_id: "01MOCKTASKDINSTANCE00001",
  started_at: "2026-09-15T00:00:00Z",
  now: "2026-09-15T00:00:01Z",
  release: "dev",
  mode: "normal",
  role: "active",
  db: {
    journal_mode: "wal",
    busy_timeout_ms: 5000,
  },
};

/**
 * `GET /releases` の 1 件（ADR-0040 D6、docs/taskd-api-v1.md §3.66）。検証済み・ライブ引き継ぎ可・
 * current ではない（＝昇格できる）状態が既定。テストは `releaseItem({...})` で上書きする。
 */
export function releaseItem(overrides: Partial<ReleaseItem> = {}): ReleaseItem {
  return {
    sha12: "aaaaaaaaaaaa",
    ref: "main",
    built_at: "2026-09-19T00:00:00Z",
    schema_version: 11,
    gate_ok: true,
    verify: { ok: true, live_ok: true, at: "2026-09-19T01:00:00Z" },
    is_current: false,
    is_previous: false,
    promoting: false,
    ...overrides,
  };
}

/** `GET /releases` の既定応答（current が 1 つ、引き継ぎは走っていない）。 */
export const defaultReleases: Releases = {
  current: "aaaaaaaaaaaa",
  previous: null,
  running: { release: "aaaaaaaaaaaa", role: "active", instance_id: "01MOCKTASKDINSTANCE00001" },
  instances: [
    {
      instance_id: "01MOCKTASKDINSTANCE00001",
      release: "aaaaaaaaaaaa",
      pid: 111,
      role: "active",
      started_at: "2026-09-19T00:00:00Z",
      heartbeat_at: "2026-09-19T02:00:00Z",
    },
  ],
  items: [
    releaseItem({ sha12: "bbbbbbbbbbbb", built_at: "2026-09-19T08:00:00Z", verify: null }),
    releaseItem({ is_current: true }),
  ],
};

/** `POST /releases/{sha12}/promote` の既定応答（202）。 */
export const defaultReleasePromoteAccepted: ReleasePromoteAccepted = {
  sha12: "bbbbbbbbbbbb",
  log: "/home/mock/taskd/releases/bbbbbbbbbbbb/promote.log",
  started_at: "2026-09-19T10:00:00Z",
};
