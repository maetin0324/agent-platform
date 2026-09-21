import type {
  ChangeDiffView,
  ChangesView,
  CommentResult,
  ConsoleBlock,
  ConsolePage,
  ConsoleProgress,
  DocPage,
  DocPageResult,
  DocsInitResult,
  DocsTree,
  EditResult,
  Health,
  IntegrateResult,
  KnowledgeCandidate,
  KnowledgeInbox,
  KnowledgePage,
  KnowledgePageResult,
  KnowledgeRejectResult,
  KnowledgeTree,
  Milestone,
  MilestoneLifecycle,
  Project,
  ProjectIntegrationItem,
  ProjectIntegrations,
  ProjectLifecycle,
  ProjectRepo,
  ReleaseChanges,
  ReleaseItem,
  ReleasePromoteAccepted,
  Releases,
  RepoChangesView,
  RepoList,
  Task,
  TaskComment,
  TaskIntegration,
  TaskRef,
  TaskSummary,
  Timeline,
  TimelineItem,
  TreeFileView,
  TreeView,
} from "~/celeris/types";

/**
 * `GET /health` の既定応答（docs/celeris-api-v1.md §3.1）。`Health` 型で宣言することで形を検証する。
 */
export const defaultHealth: Health = {
  api_version: "1",
  schema_version: 4,
  celeris_version: "0.1.0",
  instance_id: "01MOCKCELERISINSTANCE00001",
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
 * `GET /releases` の 1 件（ADR-0040 D6、docs/celeris-api-v1.md §3.66）。検証済み・ライブ引き継ぎ可・
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
    promote_failed: null,
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
  running: { release: "aaaaaaaaaaaa", role: "active", instance_id: "01MOCKCELERISINSTANCE00001" },
  instances: [
    {
      instance_id: "01MOCKCELERISINSTANCE00001",
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

/**
 * ADR-0044（Phase 53）のタスク管理の fixture。ボード・タイムライン・コメント・編集で使う。
 * どれも celeris が返す形（`app/celeris/types.ts`）で宣言してあるので、型が変わればここで気づく。
 */

/** ボードのカード 1 枚（`GET /tasks` の 1 行）。 */
export function taskSummary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "01BOARDTASK00000000000001",
    parent_id: null,
    kind: "execute",
    status: "ready",
    title: "関連研究を調べる",
    priority: 10,
    priority_label: "P2",
    tier: "standard",
    adapter: null,
    attempts: 0,
    max_retries: 2,
    depends_on: [],
    created_at: "2026-09-19T00:00:00Z",
    updated_at: "2026-09-19T00:00:01Z",
    lease_expires_at: null,
    backoff_until: null,
    children: 0,
    pending_children: 0,
    conversation: false,
    actions: ["cancel", "edit"],
    labels: [],
    category: "other",
    ...overrides,
  };
}

/** `PATCH /tasks/{id}` が写す先の `Task`（ADR-0044 D1）。 */
export function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "01BOARDTASK00000000000001",
    kind: "execute",
    status: "ready",
    title: "関連研究を調べる",
    objective: "3 本読む",
    priority: 10,
    attempts: 0,
    created_at: "2026-09-19T00:00:00Z",
    updated_at: "2026-09-19T00:00:02Z",
    acceptance: [],
    depends_on: [],
    inputs: [],
    worker_hint: { tier: "standard" },
    budget: { max_retries: 2, max_turns: 10, max_wall_secs: 600 },
    workspace: { kind: "local", path: "." },
    labels: [],
    category: "other",
    ...overrides,
  };
}

/** `PATCH /tasks/{id}` の既定応答（`fields` は**実際に変わった項目**）。 */
export function editResult(fields: string[] = ["priority"], overrides: Partial<Task> = {}): EditResult {
  return { task: task(overrides), fields };
}

/** `task_comments` の 1 行（ADR-0044 D2）。 */
export function taskComment(overrides: Partial<TaskComment> = {}): TaskComment {
  return {
    id: "01COMMENT0000000000000001",
    task_id: "01BOARDTASK00000000000001",
    author_kind: "human",
    body: "先に関連研究を 3 本だけ読んでください",
    created_at: "2026-09-19T00:01:00Z",
    ...overrides,
  };
}

/**
 * `POST /tasks/{id}/comments` の応答。`effect` ごとに `transition` / `can_reopen` の付き方が変わる
 * （ADR-0044 D2 の表）。
 */
export function commentResult(overrides: Partial<CommentResult> = {}): CommentResult {
  return { comment: taskComment(), effect: "stored", can_reopen: false, ...overrides };
}

/** `GET /tasks/{id}/timeline` の既定応答（ADR-0044 D5。時刻の昇順）。 */
export function timeline(items: TimelineItem[] = [], taskId = "01BOARDTASK00000000000001"): Timeline {
  return {
    task_id: taskId,
    items:
      items.length > 0
        ? items
        : [
            {
              kind: "event",
              at: "2026-09-19T00:00:00Z",
              seq: 0,
              event: { type: "transitioned", from: "draft", to: "ready", reason: "accepted" },
            },
            { kind: "comment", at: "2026-09-19T00:01:00Z", comment: taskComment() },
            {
              kind: "release",
              at: "2026-09-19T00:02:00Z",
              sha12: "aaaaaaaaaaaa",
              commits: ["1111111111111111111111111111111111111111"],
            },
            knowledgeTimelineItem(),
          ],
  };
}

/**
 * タイムラインの `knowledge` 項目（ADR-0047 D4/D5、Phase 62）。既定は `applied`
 * （取り込み 1 / 候補 2 / 破棄 0）。`state: "scheduled"` のときは `ingested`/`inbox`/`discarded` は
 * 付かない（celeris が適用前は出さない）。
 */
export function knowledgeTimelineItem(
  overrides: Partial<Extract<TimelineItem, { kind: "knowledge" }>> = {},
): TimelineItem {
  return {
    kind: "knowledge",
    at: "2026-09-19T00:03:00Z",
    run_task_id: "01BOARDTASK00000000000099",
    state: "applied",
    ingested: 1,
    inbox: 2,
    discarded: 0,
    ...overrides,
  };
}

/** `POST /releases/{sha12}/promote` の既定応答（202）。 */
export const defaultReleasePromoteAccepted: ReleasePromoteAccepted = {
  sha12: "bbbbbbbbbbbb",
  log: "/home/mock/celeris/releases/bbbbbbbbbbbb/promote.log",
  started_at: "2026-09-19T10:00:00Z",
  // ADR-0041 D4: 既定は「いま動いている版に同梱の promote.sh で昇格した」。
  script_from: "current",
};

/**
 * 案件のリポジトリ 1 件（ADR-0043 D1、docs/celeris-api-v1.md §3.68〜3.71。Phase 52 / G16）。
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
 * ディレクトリ 1 つ + ファイル 2 つ（並びは celeris が決めたもの: ディレクトリが先、あとは名前順）。
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

/**
 * 変更の取り込み（ADR-0043 D5、celeris Phase 54 / G18）。`GET /tasks/{id}/changes` の 1 リポジトリぶん。
 * 既定は「2 コミット進んでいて 3 ファイル変わった、きれいな worktree」。テストは `repoChangesView({...})` で上書きする。
 */
export function repoChangesView(overrides: Partial<RepoChangesView> = {}): RepoChangesView {
  return {
    repo: "benchfs",
    branch: "celeris/01TASK",
    default_branch: "main",
    base: "9602b596826c9f0f3b1c",
    head: "1f2e3d4c5b6a79887766",
    ahead: 2,
    files: [
      { path: "src/lib.rs", status: "M", additions: 12, deletions: 4 },
      { path: "src/new.rs", status: "A", additions: 30, deletions: 0 },
      { path: "docs/old.md", status: "D", additions: 0, deletions: 8 },
    ],
    stat: { files: 3, additions: 42, deletions: 12 },
    dirty: false,
    missing: false,
    origin: true,
    integration: null,
    ...overrides,
  };
}

/** `GET /tasks/{id}/changes` の既定応答（git のリポジトリ 1 つ、`gh` は使える）。 */
export function changesView(overrides: Partial<ChangesView> = {}): ChangesView {
  return {
    task_id: "01TASK",
    repos: [repoChangesView()],
    gh: true,
    merge_method: "merge",
    ...overrides,
  };
}

/** `GET /tasks/{id}/changes/{repo}/diff?path=` の既定応答（1 ファイル、切られていない）。 */
export function changeDiffView(overrides: Partial<ChangeDiffView> = {}): ChangeDiffView {
  return {
    repo: "benchfs",
    path: "src/lib.rs",
    diff: [
      "diff --git a/src/lib.rs b/src/lib.rs",
      "index 1111111..2222222 100644",
      "--- a/src/lib.rs",
      "+++ b/src/lib.rs",
      "@@ -1,3 +1,3 @@",
      " fn main() {",
      "-    old();",
      "+    new();",
      " }",
      "",
    ].join("\n"),
    truncated: false,
    ...overrides,
  };
}

/** `task_integrations` の 1 行（既定は merge が通ったあと）。 */
export function taskIntegration(overrides: Partial<TaskIntegration> = {}): TaskIntegration {
  return {
    id: "01MOCKINTEGRATION000000001",
    task_id: "01TASK",
    repo_id: "01MOCKREPO0000000000000001",
    repo: "benchfs",
    method: "merge",
    state: "done",
    pr_number: null,
    pr_url: null,
    merged_at: null,
    detail: null,
    created_at: "2026-09-19T10:00:00Z",
    updated_at: "2026-09-19T10:00:01Z",
    ...overrides,
  };
}

/** `POST .../integrate` と `POST .../pr/merge` の既定応答（衝突していないので `child_task_id` は無し）。 */
export function integrateResult(overrides: Partial<IntegrateResult> = {}): IntegrateResult {
  return { integration: taskIntegration(), child_task_id: null, ...overrides };
}

/** `GET /projects/{id}/integrations` の既定応答（新しい順。開いている PR 1 件と取り込み 1 件）。 */
export const defaultProjectIntegrations: ProjectIntegrations = {
  items: [
    {
      integration: taskIntegration({
        id: "01MOCKINTEGRATION000000002",
        task_id: "01TASK2",
        method: "pr",
        state: "open",
        pr_number: 42,
        pr_url: "https://github.test/example/benchfs/pull/42",
        updated_at: "2026-09-19T11:00:00Z",
      }),
      task_title: "ベンチマークの並列化",
      task_status: "reviewing",
    },
    {
      integration: taskIntegration(),
      task_title: "読み取りの高速化",
      task_status: "done",
    },
  ] satisfies ProjectIntegrationItem[],
};

/**
 * 中止・一時停止・アーカイブ（ADR-0044 D6、docs/celeris-api-v1.md §3.84〜3.91。Phase 55 / G19）。
 * `ProjectLifecycle` / `MilestoneLifecycle` は `cancel` のときだけ `cancelled_*` に中身が入る。
 */

/** `GET /projects` / `ProjectLifecycle.project` の 1 件（既定は進行中・アーカイブなし）。 */
export function project(overrides: Partial<Project> = {}): Project {
  return {
    id: "p1",
    title: "Pluvio の新テーマ",
    request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証",
    status: "active",
    created_at: "2026-09-19T00:00:00Z",
    updated_at: "2026-09-19T00:00:01Z",
    ...overrides,
  };
}

/** `MilestoneLifecycle.milestone` の 1 件（既定は進行中）。 */
export function milestone(overrides: Partial<Milestone> = {}): Milestone {
  return {
    id: "m1",
    project_id: "p1",
    seq: 1,
    title: "統合・選定",
    description: "",
    status: "in_progress",
    created_at: "2026-09-19T00:00:00Z",
    updated_at: "2026-09-19T00:00:01Z",
    ...overrides,
  };
}

/** 連鎖で中止されたタスク 1 件（`cancelled_tasks[]` の要素）。 */
export function taskRef(overrides: Partial<TaskRef> = {}): TaskRef {
  return {
    id: "01BOARDTASK00000000000001",
    title: "関連研究を調べる",
    kind: "execute",
    status: "cancelled",
    actions: [],
    ...overrides,
  };
}

/** `POST /projects/{id}/{cancel|pause|resume|archive|unarchive}` の応答（既定は連鎖なし）。 */
export function projectLifecycle(overrides: Partial<ProjectLifecycle> = {}): ProjectLifecycle {
  return { project: project(), cancelled_tasks: [], cancelled_milestones: [], ...overrides };
}

/** `POST /milestones/{id}/{cancel|pause|resume}` の応答（既定は連鎖なし）。 */
export function milestoneLifecycle(overrides: Partial<MilestoneLifecycle> = {}): MilestoneLifecycle {
  return { milestone: milestone(), cancelled_tasks: [], ...overrides };
}

// ---------------------------------------------------------------------------
// 文書（ADR-0044 D7、docs/celeris-api-v1.md §3.92〜3.97。Phase 57 / G20）
// ---------------------------------------------------------------------------

/** `GET /projects/{id}/docs` の既定応答（2 ページ。1 つはフォルダの中）。 */
export function docsTree(overrides: Partial<DocsTree> = {}): DocsTree {
  return {
    project_id: "01PROJECT",
    repo: "benchfs",
    root: "docs",
    default_branch: "main",
    truncated: false,
    items: [
      {
        path: "docs/README.md",
        title: "案件のあらまし",
        updated_at: "2026-09-19T10:00:00Z",
        last_commit: {
          sha: "1111111111111111111111111111111111111111",
          at: "2026-09-19T10:00:00Z",
          author: "Celeris (human)",
          subject: "docs: docs/README.md",
        },
      },
      {
        path: "docs/research/fs.md",
        title: "調べたこと",
        updated_at: "2026-09-19T11:00:00Z",
        last_commit: {
          sha: "2222222222222222222222222222222222222222",
          at: "2026-09-19T11:00:00Z",
          author: "Celeris (human)",
          subject: "docs: docs/research/fs.md",
        },
      },
    ],
    ...overrides,
  };
}

/** `GET /projects/{id}/docs/page?path=` の既定応答（front matter 付き）。 */
export function docPage(overrides: Partial<DocPage> = {}): DocPage {
  const raw =
    "---\ntitle: 調べたこと\ntags: [research]\ntasks: [01TASK]\n---\n\n# 調べたこと\n\n本文と [[../README.md]]\n";
  return {
    project_id: "01PROJECT",
    repo: "benchfs",
    root: "docs",
    default_branch: "main",
    path: "docs/research/fs.md",
    title: "調べたこと",
    raw,
    html: "<h1>調べたこと</h1>",
    tags: ["research"],
    tasks: ["01TASK"],
    history: [
      {
        sha: "2222222222222222222222222222222222222222",
        at: "2026-09-19T11:00:00Z",
        author: "Celeris (human)",
        subject: "docs: docs/research/fs.md",
      },
    ],
    etag: "3333333333333333333333333333333333333333",
    too_large: false,
    ...overrides,
  };
}

/** `PUT`/`DELETE /projects/{id}/docs/page` と `POST /tasks/{id}/artifacts/promote` の既定応答。 */
export function docPageResult(overrides: Partial<DocPageResult> = {}): DocPageResult {
  return {
    project_id: "01PROJECT",
    repo: "benchfs",
    path: "docs/research/fs.md",
    etag: "4444444444444444444444444444444444444444",
    sha: "5555555555555555555555555555555555555555",
    deleted: false,
    unchanged: false,
    ...overrides,
  };
}

/** `POST /projects/{id}/docs/init` の既定応答（新しく作った）。 */
export function docsInitResult(overrides: Partial<DocsInitResult> = {}): DocsInitResult {
  return {
    project_id: "01PROJECT",
    repo: "pluvio-poc",
    root: "docs",
    default_branch: "main",
    created: true,
    path: "/home/celeris/workspace/pluvio-poc",
    ...overrides,
  };
}

/**
 * `GET /console` の既定応答（docs/celeris-api-v1.md §3.98、ADR-0048 D1）。
 * 偽アダプタの 1 run と同じ形: `task`（開始）→ `progress`（run ごとに 1 件）→ `task`（終了）→ `report`。
 */
export function consolePage(overrides: Partial<ConsolePage> = {}): ConsolePage {
  return {
    items: consoleBlocks(),
    next_cursor: "00001789000000000000.4.r01REPORT",
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// 知識ベース（ADR-0047 D5、docs/celeris-api-v1.md §3.101〜3.106。Phase 61 / G21）
// ---------------------------------------------------------------------------

/** `GET /knowledge/tree` の既定応答（用意済み。2 つの置き場に 1 ページずつ、候補が 2 件）。 */
export function knowledgeTree(overrides: Partial<KnowledgeTree> = {}): KnowledgeTree {
  return {
    root: "/home/celeris/knowledge",
    initialized: true,
    generated_at: "2026-09-20T01:00:00Z",
    inbox_count: 2,
    truncated: false,
    scopes: ["environment/clusters", "user"],
    items: [
      {
        path: "environment/clusters/pegasus.md",
        title: "pegasus の使い方",
        tags: ["environment", "cluster", "pegasus"],
        scope: "environment",
        sources: ["human", "task:01TASKPEGASUS0000000000001"],
        updated: "2026-09-20",
        confidence: "low",
      },
      {
        path: "user/profile.md",
        title: "人のこと",
        tags: ["user"],
        scope: "user",
        sources: ["human"],
        updated: "2026-09-19",
        confidence: "high",
      },
    ],
    ...overrides,
  };
}

/** `GET /console` / `GET /console/stream` の 1 本の流れ（4 ブロック）。 */
export function consoleBlocks(): ConsoleBlock[] {
  return [
    {
      kind: "task",
      at: "2026-09-20T01:00:00Z",
      cursor: "00001789000000000000.1.e1",
      task: {
        task_id: "01BOARDTASK00000000000001",
        title: "関連研究を調べる",
        from: "ready",
        to: "running",
        reason: "dispatch",
        assignee: "research",
        harness: "fake",
        tier: "standard",
        mode: "worktree",
        project_id: "p1",
        elapsed_secs: 3,
      },
    },
    consoleProgressBlock(),
    {
      kind: "task",
      at: "2026-09-20T01:00:30Z",
      cursor: "00001789000000030000.3.e3",
      task: {
        task_id: "01BOARDTASK00000000000001",
        title: "関連研究を調べる",
        from: "running",
        to: "done",
        reason: "worker_done",
        assignee: "research",
        harness: "fake",
        tier: "standard",
        mode: "worktree",
        project_id: "p1",
        elapsed_secs: 33,
      },
    },
    {
      kind: "report",
      at: "2026-09-20T01:00:40Z",
      cursor: "00001789000000040000.4.r01REPORT",
      report: {
        id: "01REPORT",
        project_id: "p1",
        node_id: "research",
        kind: "result",
        level: 0,
        headline: "関連研究を 12 件集めた",
        body: "本文",
        sources: [],
        created_at: "2026-09-20T01:00:40Z",
      },
    },
    consoleKnowledgeBlock(),
  ];
}

/**
 * `knowledge` ブロック（ADR-0047 D4/D5、Phase 62）: 「この仕事から知識 N 件」。既定は `applied`
 * （取り込み 1 / 候補 2 / 破棄 0）。`state: "failed"` のときも `ingested`/`inbox`/`discarded` は
 * 0 のまま返る（celeris 側の既定）。
 */
export function consoleKnowledgeBlock(
  overrides: Partial<Extract<ConsoleBlock, { kind: "knowledge" }>> = {},
): ConsoleBlock {
  return {
    kind: "knowledge",
    at: "2026-09-20T01:00:50Z",
    cursor: "00001789000000050000.5.k01BOARDTASK00000000000001",
    project_id: "p1",
    task_id: "01BOARDTASK00000000000001",
    task_title: "関連研究を調べる",
    run_task_id: "01BOARDTASK00000000000099",
    state: "applied",
    ingested: 1,
    inbox: 2,
    discarded: 0,
    ...overrides,
  };
}

/** 折り畳んだ `progress` ブロック 1 件（SSE の `event: console.block` でも同じ形）。 */
export function consoleProgressBlock(overrides: Partial<ConsoleProgress> = {}): ConsoleBlock {
  return {
    kind: "progress",
    at: "2026-09-20T01:00:05Z",
    cursor: "00001789000000050000.2.p01BOARDTASK00000000000001:01RUN",
    title: "関連研究を調べる",
    assignee: "research",
    harness: "fake",
    tier: "standard",
    project_id: "p1",
    progress: {
      task_id: "01BOARDTASK00000000000001",
      run_id: "01RUN",
      count: 12,
      tool_count: 9,
      last_status: "searching the web",
      started_at: "2026-09-20T01:00:05Z",
      updated_at: "2026-09-20T01:00:29Z",
      first: [
        { at: "2026-09-20T01:00:05Z", seq: 2, kind: "status", text: "searching the web" },
        { at: "2026-09-20T01:00:06Z", seq: 3, kind: "tool_use", tool: "Bash", text: "cargo test --workspace" },
        { at: "2026-09-20T01:00:07Z", seq: 4, kind: "tool_result", tool: "Bash", text: "test result: ok" },
      ],
      last: [{ at: "2026-09-20T01:00:29Z", seq: 13, kind: "text", text: "まとめました。" }],
      truncated: true,
      ...overrides,
    },
  };
}

/** `GET /knowledge/tree` の応答（`celerisctl knowledge init` がまだ）。 */
export function knowledgeTreeUninitialized(overrides: Partial<KnowledgeTree> = {}): KnowledgeTree {
  return knowledgeTree({
    initialized: false,
    generated_at: null,
    inbox_count: 0,
    scopes: [],
    items: [],
    ...overrides,
  });
}

/** `GET /knowledge/page?path=` の既定応答（front matter + 履歴 + etag）。 */
export function knowledgePage(overrides: Partial<KnowledgePage> = {}): KnowledgePage {
  const raw = [
    "---",
    "title: pegasus の使い方",
    "tags: [environment, cluster, pegasus]",
    "scope: environment",
    "sources: [human, 'task:01TASKPEGASUS0000000000001']",
    "confidence: low",
    "updated: 2026-09-20",
    "---",
    "",
    "# pegasus の使い方",
    "",
    "ログインは踏み台から。詳しくは [[../../user/profile.md]] と [担当](celeris:task/01TASKPEGASUS0000000000001)。",
    "",
  ].join("\n");
  return {
    root: "/home/celeris/knowledge",
    path: "environment/clusters/pegasus.md",
    title: "pegasus の使い方",
    raw,
    html: "<h1>pegasus の使い方</h1>",
    tags: ["environment", "cluster", "pegasus"],
    scope: "environment",
    sources: ["human", "task:01TASKPEGASUS0000000000001"],
    confidence: "low",
    updated: "2026-09-20",
    history: [
      {
        sha: "6666666666666666666666666666666666666666",
        at: "2026-09-20T01:00:00Z",
        author: "Celeris (human)",
        subject: "knowledge: environment/clusters/pegasus.md",
      },
    ],
    etag: "7777777777777777777777777777777777777777777777777777777777777777",
    too_large: false,
    ...overrides,
  };
}

/** `PUT /knowledge/page` と `POST /knowledge/inbox/{id}/accept` の既定応答。 */
export function knowledgePageResult(overrides: Partial<KnowledgePageResult> = {}): KnowledgePageResult {
  return {
    path: "environment/clusters/pegasus.md",
    etag: "8888888888888888888888888888888888888888888888888888888888888888",
    sha: "9999999999999999999999999999999999999999",
    unchanged: false,
    ...overrides,
  };
}

/** `POST /knowledge/inbox/{id}/reject` の既定応答。 */
export function knowledgeRejectResult(overrides: Partial<KnowledgeRejectResult> = {}): KnowledgeRejectResult {
  return {
    id: "20260920T010000-pegasus",
    sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ...overrides,
  };
}

/**
 * `GET /knowledge/inbox` の既定応答（候補 2 件。新しい順）。
 * 1 件目は**取り込み先に既にページがある**（`target_exists: true` = accept に `overwrite` が要る）。
 */
export function knowledgeInbox(overrides: Partial<KnowledgeInbox> = {}): KnowledgeInbox {
  return {
    root: "/home/celeris/knowledge",
    initialized: true,
    items: [
      knowledgeCandidate(),
      knowledgeCandidate({
        id: "20260919T220000-ldr-tunnel",
        path: "_inbox/20260919T220000-ldr-tunnel.md",
        title: "LDR のトンネルは 900 秒で切れる",
        tags: ["environment", "ldr"],
        scope: "environment",
        sources: ["message:01MESSAGE0000000000000001", "url:https://example.invalid/ldr"],
        confidence: "medium",
        created: "2026-09-19T22:00:00Z",
        body: "アイドル 900 秒でトンネルが落ちる。落ちたら張り直す。\n",
        html: "<p>アイドル 900 秒でトンネルが落ちる。落ちたら張り直す。</p>",
        target: "environment/ldr.md",
        target_exists: false,
      }),
    ],
    ...overrides,
  };
}

/** `_inbox/` の候補 1 件（既定は取り込み先が既にある = `target_exists: true`）。 */
export function knowledgeCandidate(overrides: Partial<KnowledgeCandidate> = {}): KnowledgeCandidate {
  return {
    id: "20260920T010000-pegasus",
    path: "_inbox/20260920T010000-pegasus.md",
    title: "pegasus は踏み台を通す",
    tags: ["environment", "cluster", "pegasus"],
    scope: "environment",
    sources: ["task:01TASKPEGASUS0000000000001", "human"],
    confidence: "low",
    created: "2026-09-20T01:00:00Z",
    body: "pegasus には踏み台（jump host）を通してつなぐ。\n",
    html: "<p>pegasus には踏み台（jump host）を通してつなぐ。</p>",
    target: "environment/clusters/pegasus.md",
    target_exists: true,
    ...overrides,
  };
}
