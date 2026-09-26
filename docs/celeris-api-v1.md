# celeris HTTP API v1: 実行・計画・再実行

---
tasks: [01M3EDF3JEHRQCG6A2EJDRQMXJ]
---

共通の base path は `/api/v1`。読み取りはトークン不要、変更系は管理トークンが必要（未設定でも 401）。JSON の正本は [`api-v1.schema.json`](api/v1/api-v1.schema.json)、従来のエンドポイント一覧は [`gui/api.md`](gui/api.md)。以下の型名は同スキーマの `$defs` を指す。

### `GET /tasks/{id}/execution` → 200 `TaskExecutionView`

タスクの実行詳細。`gate` は `ExecutionGateDecision | null`、`phase` は `ExecutionPhase | null`、`plan` は `ExecutionPlanView | null`。`runs` は `RunSummary[]`、`metrics` は `ExecutionMetrics` で必須。`metrics.total_cache_read_tokens` は観測できた cached input tokens の合計で、未観測なら省略する。計画の無い atomic タスクの `plan` は `null`。不明なタスクは 404。クエリは受け付けない。

### `GET /tasks/{id}/execution-plan` → 200 `ExecutionPlanView`

有効な計画と `work_units[]` を返す。`versions[]` は `ExecutionPlanVersionView` の版履歴（`version` 昇順、superseded を含む）。有効な計画が無ければ 404 `execution_plan_not_found`。`ExecutionPlanView` は `id`、`task_id`、`version`、`origin`、`status`、`plan`、`created_at`、`work_units` が必須で、`versions` は既定 `[]`。`plan.schema` は `celeris.execution-plan/1` または `/2`（v2 は `phases` を持つ）。クエリは受け付けない。

### `POST /tasks/{id}/execution-plan` → 201 `ExecutionPlanView`（管理系）

本文は `ExecutionPlanSpec`。`schema`、`rationale`、`work_units` が必須。v2 は `phases` も必要で、`children` は現行では空配列のみ。人が提案した計画として検証・採用し、応答に `work_units` と `versions` を含む。クエリは受け付けない。管理トークンが無ければ 401、計画が無効なら検証エラー。

### `GET /metrics/execution?since=&group_by=` → 200 `ExecutionMetricsSummary`

`since` は RFC 3339 の時刻（省略可）。`group_by` は `gate_mode`（既定）、`genre`、`assignee`、`lane` のいずれか。応答は `group_by`、`total_tasks`、`groups[]` が必須で、`since` と `accounts_now[]` は省略可能。各 group は `ExecutionMetricsGroup`。不正な日時・group_by は 400。

### `POST /tasks/{id}/accept` → 200 `TransitionResult`（管理系）

`draft` を `ready` にする。本文は省略可能な `ReopenBody`（`expected_status` のみ。楽観的競合検出）。既に draft でない場合は状態競合。これは `approve`（人の承認チェック）とは異なる操作。クエリは受け付けない。

### `POST /tasks/{id}/retry` → 201 `RetryResult`（管理系）

`failed` または `cancelled` のタスクを複製し、新しいタスクの `task_id` と `rewired` を返す。本文は省略可能な `RetryBody`。`accept` の既定は **`true`** で、新しいタスクは `ready` で始まる。`false` の場合だけ `draft`。`workspace` を指定すると複製先の作業場所を差し替える。応答の `Location` は新しいタスクの URL。クエリは受け付けない。
