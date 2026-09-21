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
  NodeSessionSummary,
  OrgList,
  OrgNode,
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
 * 連続する `worker_progress` イベント（ADR-0048 D2、フェーズ 74）。`/tasks/:id?tab=timeline` の機械検査
 * （`gui/scripts/mobile-audit.mjs`）が「折り畳み → 展開すると Console と同じ step 行」を実際のデータで
 * 通ることを確認するための fixture。長い `summary`（90 字超）を混ぜて `~/components/ConsoleBlockItem.tsx::
 * ReplyStepRow` のタップ展開（U-G29-2 / P-G29-2 の解消）も一緒に検査できるようにする。
 */
export function timelineWorkerProgressItems(startSeq = 1): TimelineItem[] {
  return [
    {
      kind: "event",
      at: "2026-09-19T00:00:10Z",
      seq: startSeq,
      event: {
        type: "worker_progress",
        run_id: "r1",
        kind: "tool_use",
        tool: "Bash",
        summary: "cargo test --workspace",
        msg: "",
      },
    },
    {
      kind: "event",
      at: "2026-09-19T00:00:11Z",
      seq: startSeq + 1,
      event: {
        type: "worker_progress",
        run_id: "r1",
        kind: "tool_use",
        tool: "Bash",
        summary:
          "cargo test --workspace --all-features -- --nocapture 2>&1 | tee /tmp/very/long/path/to/the/output/of/this/particular/test/run.log",
        msg: "",
      },
    },
    {
      kind: "event",
      at: "2026-09-19T00:00:12Z",
      seq: startSeq + 2,
      event: {
        type: "worker_progress",
        run_id: "r1",
        kind: "tool_result",
        summary: "test result: ok. 42 passed; 0 failed\n(詳細はここをタップ)",
        msg: "",
      },
    },
  ];
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
// 組織（ADR-0033 D1、docs/gui/api.md §3.42〜3.45。フェーズ 73 で追加）
// ---------------------------------------------------------------------------

/** `orgList()` の 1 ノード。`gui/scripts/mobile-audit.mjs` の同名のヘルパと同じ既定値。 */
function orgNode(id: string, over: Partial<OrgNode> = {}): OrgNode {
  return {
    id,
    parent_id: null,
    name: id,
    kind: "section",
    position: 0,
    created_at: "2026-09-17T00:00:00Z",
    updated_at: "2026-09-17T00:00:01Z",
    ...over,
  };
}

/**
 * `GET /org` の既定応答（ADR-0055 D2 ラウンド 5、U12）: `/org` の開閉トグル（`OrgTreeItem`。
 * フェーズ 72 で追加）が、実際に何段か深いサブツリーを畳めることを機械検査・単体テストの両方で
 * 確かめられるように、`coding-poc`（既存の他の fixture が assignee として参照する id。変えていない）の
 * 下に 3 段の子を足した: `coding-poc` → `coding-poc-alpha` → `coding-poc-alpha-1` →
 * `coding-poc-alpha-1-x`（+ 兄弟 `coding-poc-beta`）。`research` 部・`research-survey` 課も足し、
 * 部門長の継続セッション（`lead_sessions`）を `coding`・`research` の 2 部に付ける。
 */
export function orgList(overrides: Partial<OrgList> = {}): OrgList {
  return {
    items: [
      orgNode("cos", { kind: "secretary", name: "CoS" }),
      orgNode("coding", { kind: "department", parent_id: "cos", name: "Coding", position: 0 }),
      orgNode("coding-poc", { parent_id: "coding", genre: "coding", name: "PoC", position: 0 }),
      orgNode("coding-poc-alpha", { parent_id: "coding-poc", name: "PoC・alpha 班", position: 0 }),
      orgNode("coding-poc-alpha-1", { parent_id: "coding-poc-alpha", name: "alpha・第 1 陣", position: 0 }),
      orgNode("coding-poc-alpha-1-x", { parent_id: "coding-poc-alpha-1", name: "alpha・第 1 陣・x", position: 0 }),
      orgNode("coding-poc-beta", { parent_id: "coding-poc", name: "PoC・beta 班", position: 1 }),
      orgNode("research", { kind: "department", parent_id: "cos", name: "Research", position: 1 }),
      orgNode("research-survey", { parent_id: "research", genre: "research", name: "調査課", position: 0 }),
    ],
    lead_sessions: orgLeadSessions(),
    ...overrides,
  };
}

/** `orgList()` の `lead_sessions`（部門長 2 名分。単独で上書きしたいテストのために切り出す）。 */
export function orgLeadSessions(): NodeSessionSummary[] {
  return [
    { node_id: "coding", turns: 3, approx_tokens: 123456, last_used_at: "2026-09-21T01:00:00Z" },
    { node_id: "research", turns: 1, approx_tokens: 9800, last_used_at: "2026-09-20T23:00:00Z" },
  ];
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

/**
 * `GET /console` / `GET /console/stream` の 1 本の流れ（4 ブロック）。
 *
 * ADR-0054 D2（Phase 68）の育つ返事（`reply`、`state: "streaming"`）は、ここには**加えない**:
 * `~/scripts/mobile-audit.mjs` の `checkFixedOverlays`（D1-5）は文書全体だけをスクロールし、
 * `console-stream`（`overflow-y-auto` の内側のボックス）自身のスクロールは行わないため、この既定の
 * 流れにブロックを足すと（実際にはスクロールで届く）末尾の要素が「固定の入力欄より下」という
 * 偽陽性を生む。育つ返事の見た目・積み上げの検証は `consoleReplyBlock()` /
 * `consoleGrowingReplySteps()`（このすぐ下）を使う単体テスト（`~/test/unit/console.test.ts`）で行う。
 */
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

/**
 * 確定した `reply` ブロック（`state: "done"`、既定）。CoS の返事。ADR-0048 D3 の `actions_result` は
 * 含めない既定（Phase 60b のテストは別に用意されている）。
 */
export function consoleReplyBlock(overrides: Partial<Extract<ConsoleBlock, { kind: "reply" }>> = {}): ConsoleBlock {
  return {
    kind: "reply",
    at: "2026-09-21T01:00:10Z",
    cursor: "00001789000010000000.2.m01REPLY",
    message_id: "01REPLY",
    node_id: "cos",
    text: "承知しました。関連研究の調査から始めます。",
    ...overrides,
  };
}

/**
 * ADR-0054 D2（Phase 68）: CoS の対話 run が育っていく様子（1 本の run の 4 段階）。celeris の
 * `GET /console/stream` はこの並びで `reply` ブロックを流す（`state: "streaming"` の 3 件 → 確定した
 * `state: "done"` の 1 件）。`run_id`/`task_id` は 4 件とも同じで、GUI 側は `appendConsoleBlock`
 * （`~/lib/console.ts`）がこれを 1 つの育つ吹き出しにまとめる。SSE は**増分だけ**を送るので、
 * `text`/`steps` は各段階の増分（積み上げは GUI 側）。
 */
export function consoleGrowingReplySteps(): ConsoleBlock[] {
  const run_id = "01RUNCOS0000000000000001";
  const task_id = "01TASKCOS0000000000000001";
  return [
    {
      kind: "reply",
      at: "2026-09-21T01:00:00Z",
      cursor: "00001789000000000000.1.p01TASKCOS0000000000000001:01RUNCOS0000000000000001",
      message_id: `streaming:${task_id}:${run_id}`,
      node_id: "cos",
      project_id: null,
      task_id,
      run_id,
      text: "",
      state: "streaming",
      thinking: "考え中…",
      steps: [],
    },
    {
      kind: "reply",
      at: "2026-09-21T01:00:01Z",
      cursor: "00001789000001000000.2.p01TASKCOS0000000000000001:01RUNCOS0000000000000001",
      message_id: `streaming:${task_id}:${run_id}`,
      node_id: "cos",
      project_id: null,
      task_id,
      run_id,
      text: "",
      state: "streaming",
      thinking: null,
      steps: [{ kind: "tool_use", tool: "celerisctl", text: "knowledge search 降水予測" }],
    },
    {
      kind: "reply",
      at: "2026-09-21T01:00:02Z",
      cursor: "00001789000002000000.3.p01TASKCOS0000000000000001:01RUNCOS0000000000000001",
      message_id: `streaming:${task_id}:${run_id}`,
      node_id: "cos",
      project_id: null,
      task_id,
      run_id,
      text: "承知しました。",
      state: "streaming",
      thinking: null,
      steps: [{ kind: "tool_result", text: "3 件" }],
    },
    {
      kind: "reply",
      at: "2026-09-21T01:00:00Z",
      cursor: "00001789000003000000.4.m01REPLYCOS",
      message_id: "01REPLYCOS",
      node_id: "cos",
      project_id: null,
      task_id,
      run_id,
      text: "承知しました。関連研究の調査から始めます。",
      state: "done",
    },
  ];
}

/**
 * `consoleGrowingReplySteps()` の 4 件（celeris が SSE で送る**増分**）を、`~/lib/console.ts::
 * appendConsoleBlock` と同じ規則でその場で積み上げた 1 件（フェーズ 73、U-G28-2 / P-G28-1）。
 * `GET /console`（履歴の初期表示。`since` 無し）は celeris 側で 1 回に組んで返す（Phase 68 追記 2）ので、
 * GUI から見ると SSE の積み上げ結果と同じ形の 1 件が最初から乗っている。`gui/scripts/mobile-audit.mjs`
 * が「育つ返事」の見た目（考え中の帯・`tool_use`/`tool_result` の `steps`）を機械検査に通すために使う
 * （`appendConsoleBlock` 自体のテストは `test/unit/console.test.ts` に別途ある）。
 */
export function consoleGrowingReplySnapshot(
  overrides: Partial<Extract<ConsoleBlock, { kind: "reply" }>> = {},
): ConsoleBlock {
  const run_id = "01RUNCOS0000000000000001";
  const task_id = "01TASKCOS0000000000000001";
  return {
    kind: "reply",
    at: "2026-09-21T01:00:01Z",
    cursor: "00001789000009000000.9.p01TASKCOS0000000000000001:01RUNCOS0000000000000001",
    message_id: `streaming:${task_id}:${run_id}`,
    node_id: "cos",
    project_id: null,
    task_id,
    run_id,
    text: "承知しました。",
    state: "streaming",
    thinking: "考え中…",
    steps: [
      {
        // フェーズ 74（ADR-0055 D2 ラウンド 6、U-G29-2 / P-G29-2 の解消）: 90 字を超える要約にして、
        // `~/components/ConsoleBlockItem.tsx::ReplyStepRow` のタップ展開（`toolSummaryTruncated`）が
        // 機械検査（`gui/scripts/mobile-audit.mjs`）を実データで通ることを確認する。
        kind: "tool_use",
        tool: "celerisctl",
        text: "knowledge search 降水予測の長期トレンドと気候変動の関係について、2010 年から 2024 年までの主要な論文と観測データを対象に検索し、関連する引用文献も合わせて収集する",
      },
      {
        kind: "tool_result",
        text: "3 件\n- 降水予測の手法比較（2024）\n- 長期トレンド分析（2023）\n- 気候変動と降水（2022）",
      },
    ],
    ...overrides,
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
