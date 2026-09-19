import type {
  Health,
  ProjectRepo,
  ReleaseChanges,
  ReleaseItem,
  ReleasePromoteAccepted,
  Releases,
  RepoList,
  TreeFileView,
  TreeView,
} from "~/taskd/types";

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
    promoted_at: null,
    on_main: true,
    changes: null,
    is_current: false,
    is_previous: false,
    promoting: false,
    ...overrides,
  };
}

/**
 * `ReleaseItem.changes`（ADR-0041 D4、Phase G15）。既定は**安全に関わる変更が無い**差分。
 * 赤いバッジと sha12 入力を試すテストは `releaseChanges({sensitive: [...]})` で上書きする。
 */
export function releaseChanges(overrides: Partial<ReleaseChanges> = {}): ReleaseChanges {
  return {
    base: "aaaaaaaaaaaa",
    stale: false,
    commit_count: 2,
    file_count: 3,
    sensitive: [],
    commits: [
      { sha: "1111111111111111111111111111111111111111", subject: "phase 50: 検証の直列化" },
      { sha: "2222222222222222222222222222222222222222", subject: "adr-0041" },
    ],
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
    releaseItem({
      sha12: "bbbbbbbbbbbb",
      built_at: "2026-09-19T08:00:00Z",
      verify: null,
      on_main: false,
      changes: releaseChanges(),
    }),
    releaseItem({ is_current: true, promoted_at: "2026-09-19T02:00:00Z" }),
  ],
};

/** `POST /releases/{sha12}/promote` の既定応答（202）。 */
export const defaultReleasePromoteAccepted: ReleasePromoteAccepted = {
  sha12: "bbbbbbbbbbbb",
  log: "/home/mock/taskd/releases/bbbbbbbbbbbb/promote.log",
  started_at: "2026-09-19T10:00:00Z",
  // ADR-0041 D4: 既定は「いま動いている版に同梱の promote.sh で昇格した」。
  script_from: "current",
};

/**
 * 案件のリポジトリ 1 件（ADR-0043 D1、docs/taskd-api-v1.md §3.68〜3.71。Phase 52 / G16）。
 * 既定は手元の git で主なリポジトリ。テストは `projectRepo({...})` で上書きする。
 */
export function projectRepo(overrides: Partial<ProjectRepo> = {}): ProjectRepo {
  return {
    id: "01MOCKREPO0000000000000001",
    project_id: "p1",
    name: "benchfs",
    kind: "git",
    location: { kind: "local", path: "/home/mock/workspace/rust/benchfs" },
    default_branch: "main",
    run: "auto",
    is_primary: true,
    created_at: "2026-09-19T00:00:00Z",
    ...overrides,
  };
}

/** `GET /projects/{id}/repos` の既定応答（primary が先頭、あとは作った順）。 */
export const defaultRepoList: RepoList = {
  items: [
    projectRepo(),
    projectRepo({
      id: "01MOCKREPO0000000000000002",
      name: "benchfs-paper",
      kind: "dir",
      location: { kind: "local", path: "/home/mock/workspace/papers/benchfs" },
      default_branch: null,
      is_primary: false,
    }),
  ],
};

/**
 * `GET /tasks/{id}/tree` の既定応答（ADR-0043 D6、§3.72）。リポジトリ 2 つ、根に
 * ディレクトリ 1 つ + ファイル 2 つ（並びは taskd が決めたもの: ディレクトリが先、あとは名前順）。
 */
export function treeView(overrides: Partial<TreeView> = {}): TreeView {
  return {
    repo: "benchfs",
    path: "",
    repos: [
      {
        name: "benchfs",
        kind: "git",
        dir: "/home/mock/.local/celeris/workspaces/01TASK/repos/benchfs",
        branch: "celeris/01TASK",
        base: "9602b596826c",
      },
      {
        name: "benchfs-paper",
        kind: "dir",
        dir: "/home/mock/.local/celeris/workspaces/01TASK/repos/benchfs-paper",
      },
    ],
    entries: [
      { name: "src", path: "src", kind: "dir" },
      { name: "Cargo.toml", path: "Cargo.toml", kind: "file", size: 512 },
      { name: "README.md", path: "README.md", kind: "file", size: 1234 },
    ],
    ...overrides,
  };
}

/** `GET /tasks/{id}/tree/file` の既定応答（§3.73）。テキストで 512 KiB 以下なので `text` が付く。 */
export function treeFileView(overrides: Partial<TreeFileView> = {}): TreeFileView {
  return {
    repo: "benchfs",
    path: "README.md",
    size: 1234,
    binary: false,
    too_large: false,
    text: "# benchfs\n\nワークスペースの読み取り専用の表示。\n",
    ...overrides,
  };
}
