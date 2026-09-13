# ADR-0004: `taskctl` CLI と `replay` の実装方針

- 日付: 2026-09-13
- 状態: Accepted（Phase 2）
- 関連: `docs/DESIGN.md` §5.9, §4.3 / [ADR-0002](0002-state-machine.md) D2, D4, D8, 「結果」節

## 文脈

Phase 2 は `taskctl`（`add / ls / show / approve / reject / answer / log / replay`）を実装する。
ADR-0002 D4 は `approve`/`reject` の遷移写像を既に決めているが、次の点が未確定:

(a) `taskctl` から状態を変更する際、`acquire_lease` 以外に「任意のトリガーを検証して同一トランザクションで
`Event::Transitioned`（と関連イベント）を追記する」汎用の書き込み口が `TaskStore` に無い。
(b) `taskctl add` の CLI 引数から `Task` を構築する際のデフォルト値・`--accept` の `Check` 種別。
(c) SQLite ファイルの場所の規約。
(d) `taskctl replay` が何を再構築し、どう「差分」を報告するか。

## 決定

### D1. `TaskStore::apply_transition` を追加する

```rust
fn apply_transition(
    &self,
    task_id: TaskId,
    trigger: Trigger,
    extra_event: Option<Event>,
) -> Result<Outcome, StoreError>;
```

`acquire_lease` と同じトランザクション構造（ADR-0002 D2 の不変条件）を汎用化する:

1. トランザクション内でタスクを取得。無ければ `StoreError::Invalid("task not found")`。
2. `StateView{kind, status, attempts, max_retries: task.budget.max_retries}` を組み立て、
   `transition::transition(&view, &trigger)` を呼ぶ。`Err` ならロールバックして
   `StoreError::InvalidTransition`（新しいバリアント、`#[from] InvalidTransition`）を返す。
3. `Ok(outcome)` なら `task.status = outcome.next`, `task.attempts = outcome.attempts`,
   `task.updated_at = now` を書いて `tasks` 行を更新。
4. 同一トランザクションで `Event::Transitioned{from: view.status, to: outcome.next, reason: outcome.reason}`
   を追記する。`reason` は必ず `Trigger` の機械可読名（ADR-0002 D2/「結果」節）を使い、
   呼び出し側が上書きしない。
5. `extra_event`（`Approve`/`Reject` に伴う `Event::ApprovalDecided{by,approved,note}` など）が
   あれば同一トランザクションで追記する。
6. commit。

`taskctl` の `approve` / `reject` / `answer` はこの1メソッドだけで状態変更を行い、`get`+手動 `UPDATE` は行わない。
`Cancel` は Phase 2 では CLI から露出しない（DESIGN §5.9 に `cancel` サブコマンドの記載が無いため。将来
`taskctl cancel` を追加する際もこのメソッドを再利用できる）。

### D2. `taskctl approve` / `reject` の実装は ADR-0002 D4 のとおり

- `status = draft`（kind 不問）→ `Trigger::Accept`。
- `kind = Approval` かつ `status = ready` → `approve` は `Trigger::Approve`、`reject` は `Trigger::Reject`。
  このとき `extra_event = Some(Event::ApprovalDecided{ by: "human".into(), approved, note })` を渡す。
  `note` は `--note` フラグ（省略可）。`by` は固定文字列 `"human"`（認証機構が無いため。Phase 6 で拡張余地）。
- それ以外の `(status, kind)` → `apply_transition` が返す `StoreError::InvalidTransition` をそのまま
  ユーザー向けエラーとして表示し、終了コード 1。
- **P-5 は採用しない。** `draft` タスクへの `reject` はエラーのまま（ADR-0002 D4 の「それ以外→エラー」)。
  P-5（`draft → cancelled`）は Phase 0/1 から未採否のまま `PROGRESS.md` に残す。

### D3. `taskctl answer <id> <text>` は状態遷移のみ行い、回答テキストは永続化しない

`Trigger::Answer`（`blocked → ready`）を `apply_transition` で適用する。回答テキストを `context.answers` として
ワーカーに渡す仕組みは P-10（未決定、Phase 3 までに要決定）であり、`Event` enum にも保存先が無い。
Phase 2 では回答テキストを標準出力に反映するだけに留め、`Event::Transitioned.reason` を `"answer"` から
変更しない（D1 の不変条件を壊さないため）。この制約は `PROGRESS.md` に引き継ぐ。

### D4. `taskctl add` の CLI→`Task` 写像

| CLI | 既定値 | 備考 |
|---|---|---|
| `--title` | 必須 | |
| `--objective` | 必須 | |
| `--accept <text>`（複数可） | 最低1つ必須 | `Criterion{ text, check: Check::Human }`。Phase 2 は自動検証を実装しないため、既定の `check` は常に `Human`。機械可読な `Check::Command` 等を CLI から指定する構文は Phase 3 以降で `--check-cmd` 等として追加を検討（提案 P-17）。 |
| `--kind` | `execute` | `plan\|execute\|review\|approval` |
| `--tier` | `standard` | `frontier\|standard\|cheap` |
| `--priority` | `0` | |
| `--parent <id>` | なし | |
| `--depends-on <id>`（複数可） | なし | |
| `--max-turns` | `10` |
| `--max-wall-secs` | `600` |
| `--max-retries` | `2` |
| `--workspace <path>` | カレントディレクトリ | `WorkspaceSpec::Local{path}` |

初期状態は ADR-0002 D4 のとおり: `kind = Approval` は `ready`、それ以外は `draft`。
`worker_hint.adapter` は常に `None`（Phase 2 はディスパッチしないため未使用）。
挿入は `store.insert(&task)` の後、同一操作として `store.append_event(task.id, Event::Created{task})` を呼ぶ
（`insert` 自体はトランザクションを持たないが、Phase 2 では単一プロセス・単一呼び出しのため実害はない。
複数プロセスからの `add` 競合が問題になる場合は Phase 3 で `insert` 自体のトランザクション化を検討する）。

### D5. DB ファイルの場所

グローバルフラグ `--db <path>`（既定値 `./taskd.sqlite3`）。優先順位: `--db` フラグ > 環境変数 `TASKD_DB` >
既定値。`config/taskd.toml` によるパス解決は Phase 3 以降（DESIGN §2）に譲る。

### D6. `taskctl replay` の再構築と差分report

ADR-0002「結果」節のとおり、`events_for(task_id)` を `seq` 昇順に畳み込んで次を再構築する:

- `status`: `Event::Created{task}` の `task.status` を初期値とし、以降の `Event::Transitioned{to,..}` で
  `to` に更新する。
- `attempts`: 0 を初期値とし、`Event::Transitioned{reason,..}` の `reason` が
  `"worker_error" | "lease_expired" | "review_fail"` のとき +1 する。

各タスクについて、再構築した `(status, attempts)` と `store.get(task_id)` の現在値を比較する。
不一致があれば `task_id`, フィールド名, 期待値, 実際値を1行ずつ報告し、末尾に `replay: N mismatches across M tasks`
を出す。`N = 0` なら終了コード 0、そうでなければ終了コード 1（自動検証用）。

## 結果

- `task-core::store` に `apply_transition` と `StoreError::InvalidTransition` を追加する（`transition` モジュールへの
  依存が `store.rs` に生じる。ADR-0002 D2 は元々「同一トランザクションで追記」を要求しており、疎結合の原則
  （store.rs 冒頭コメント）は「`transition()` の"結果"をストアに書き戻す」経路を想定済みなので矛盾しない）。
- `crates/taskctl` を新設し、workspace member に追加する。

## DESIGN.md 修正提案（本 ADR のスコープ分）

- **P-17（§5.9 add --accept）** `--accept` に機械可読な検証方法（`--check-cmd "cargo test"` 等）を指定できる
  構文を Phase 3 で追加することを提案。採用まで `--accept` は常に `Check::Human` を生成する。
