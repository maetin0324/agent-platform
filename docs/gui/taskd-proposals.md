# GUI のために taskd 側で必要になる変更（提案一覧と採否）

- 日付: 2026-09-14（初版）→ 同日、人間の決定（H1〜H9）と `docs/adr/0013-taskd-api-and-gui-foundations.md` に合わせて状態を更新
- 状態の語彙: **採用**（ADR-0013 で決定、Phase 9 で実装）/ **置換**（別の決定で不要になった）/ **後回し**（採らない。必要になれば再提案）/ **新規**（本改訂で追加した提案。採否は人間）
- 番号は `P-G<n>`（PROGRESS.md の P-n と区別）

## 一覧

| # | 状態 | 提案 | ADR-0013 での扱い・備考 |
|---|---|---|---|
| P-G1 | **採用** | `task-ops` ライブラリ crate（approve / reject / answer / cancel / add / plan / replay の判断と検証、`TaskDetail` 等の派生ビュー、ディスパッチャの派生関数の移動） | D7。新 crate `crates/task-ops`（H3）。依存は `task-core` のみ。ビュー型は `JsonSchema`。`taskctl` / `task-api` / ディスパッチャが共用。派生値の規則は `api.md` §5 |
| P-G2 | **採用** | SQLite の PRAGMA（WAL / `busy_timeout` 5000 ms / `synchronous=NORMAL`）とスキーマ版数 | D5。3 接続（ディスパッチャ / API / taskctl）。DB はローカルディスク（WAL は NFS 不可） |
| P-G3 | **採用** | `events` のグローバル単調 id（作り直し、`UNIQUE(task_id, seq)`）、`events_since(after_id, limit)` | D6（H4）。`EventRow{id, task_id, seq, ts, event}`。SSE のカーソル |
| P-G4 | **置換** | デーモン状態の DB スナップショット表 `daemon_status` | 不採用（H5: 毎 tick の DB 書き込みは非効率）。D4 の `tokio::sync::watch` による `DaemonSnapshot` のメモリ公開に置換。`taskctl status` も作らない（`curl /api/v1/daemon` で代替） |
| P-G5 | **採用** | `Event::ProviderThrottled` を記録する | D9。`Requeue` の遷移と**同じトランザクション**で追記。`reason: Option<throttled \| auth_failed \| exhausted \| spawn>` を任意フィールドで追加 |
| P-G6 | **採用（実装済み）** | `Event` に `JsonSchema` を derive | D8。`until` は `#[schemars(with = "String")]`。Phase 9a で `docs/api/v1/event.schema.json`（ルート `EventRow`）を生成・一致テスト済み。9b の `api-v1.schema.json` の `$defs` にも同じ定義が入り、GUI はそちらだけを読む |
| P-G7 | **採用** | ストアのページング・検索（`list_page` / `count_by_status`、`tasks` に `title` / `updated_at` 列） | D10。マイグレーション 0003。keyset の cursor は `api.md` §3.3。`children_of` / `dependents_of` は task-ops 内のクエリで代替してよい（実装判断） |
| P-G8 | **後回し** | Human check の Approval 子の構造化（`ApprovalRequested` に `criterion_idx` / `attempt`） | D12。title の解析関数 `task_ops::parse_human_approval_title` を 1 つ置き、書式変更時はそこだけ直す |
| P-G9 | **採用** | `taskctl show --json` を `task-ops::TaskDetail` で提供 | D12。`GET /api/v1/tasks/{id}` と**同一 JSON**。人間向けテキスト（P-39）の改善は引き続き後回し |
| P-G10 | **後回し** | `WorkerFinished` に `exit_code` / `duration_ms` | D12。所要は `events.ts` の差で代用 |
| P-G11 | **採用** | スキーマ版数の検査（`SchemaTooNew`） | D5（P-G2 に含む）。`GET /health` の `schema_version` で GUI からも見える |
| P-G12 | **置換** | 外部利用向けの crate タグ付け（`task-core-vX.Y`）、rusqlite の更新 | D12。GUI が crate に依存しないので不要（H1）。契約は HTTP API v1 と `api-v1.schema.json`。rusqlite の更新は GUI と無関係になったので taskd 側の任意 |
| P-G13 | **後回し** | `by` の記録（`"human:<name>"` / `"gui"`） | D12（H9）。当面 `"human"` 固定 |

## 本改訂で追加した提案（採否は人間。`api.md` §9 の未決に対応）

| # | 状態 | 提案 | 理由 | 代替 |
|---|---|---|---|---|
| P-G14 | **新規** | **Reviewer run の使用量をイベントに残す**: Reviewer run にも `WorkerStarted{run_id, adapter, model, provider}` / `WorkerFinished{run_id, outcome, usage}` を対象タスクに追記する（`run_id` は review run の id。`kind` の区別は `WorkerProgress` の接頭辞ではなく `WorkerStarted` に `role: worker \| reviewer` を任意フィールドで持たせる） | 現状 Reviewer run は `ReviewerSink` が進捗を対象 run に付け替えるだけで、アカウント別のトークン集計（DESIGN-GUI §1 の目的 3）から漏れる | 集計外のまま（`api.md` §5.8 の注記どおり）。GUI は「Reviewer run は含まない」と表示 |
| P-G15 | **新規** | `tasks` に `objective` の非正規化列を追加し `GET /tasks?q=` の対象にする | 一覧の検索が title だけでは不足する場合がある | title のみ（現在の決定）。全文検索（FTS5）は非目標のまま |
| P-G16 | **新規** | `task_ops::add::create_task` / `plan::create_plan` に `title` / `objective` の空白検査と `parent` の存在検査を足す（CLI も同じ関数を通るので `taskctl add --title ""` が拒否されるようになる） | API 越しに空タイトルや孤児の子を作れてしまう。GUI はフォームで防ぐが、契約として taskd 側で拒否する方が安全 | 現状のまま（Phase 9a は挙動を変えない方針）。`api.md` §9 の未決 6 |

## ADR-0013 と `api.md` の間で実装者が確認すべき細部（`api.md` を正とする提案）

| 項目 | ADR-0013 | `api.md` の決め | 備考 |
|---|---|---|---|
| `DaemonSnapshot` の置き場 | 記述なし（D3: task-api は task-dispatch に依存しない） | `task-ops`（task-dispatch と task-api の両方が依存する） | 依存方向 task-dispatch → task-ops ← task-api |
| `[api]` の設定キー | `token_file`、Host の「設定値」 | `listen` / `token_file` / `allowed_hosts`（Phase 9a の `ApiConfig` と同じ。SSE 上限は定数 16） | 推奨 `listen = "127.0.0.1:7710"`（GUI は 7700） |
| `/health` の認証 | 記述なし | 無認証（版と `journal_mode` のみ） | G0 の疎通確認用。問題があれば認証必須に |
| スキーマファイル | `docs/api/v1/*.schema.json` | `event.schema.json`（9a、済み）+ `api-v1.schema.json`（9b、ルート `ApiV1Schema`）。GUI は後者だけを読む | 型の重複生成を避けるため API 全体は 1 ファイル |
| 操作の入力・結果の型名 | 記述なし | Phase 9a の実装名をそのまま使う: `NewTaskSpec` / `CriterionSpec` / `NewPlanSpec` / `TransitionResult`（9b で `cascaded` 追加）/ `ReplayReport` / `ReviewNote` / `AnswerNote` / `OpsError` | `api.md` §6 |
| `POST /tasks` の検証 | 記述なし | task-ops と同じ 2 種だけ（条件ゼロ、`depends_on`）。追加は P-G16 | 挙動を変えない |
| `GET /events`（全体、`after_id`） | SSE のみ言及 | 追加（`events_since` そのもの。`curl` とテスト用） | 実装コストは小 |
| `GET /config` | 記述なし | 追加。`task-api` の `ConfigView` に taskd が起動時に値を詰める | env の値・トークンは出さない |
| 変更系の `Origin` 拒否 | D11 に無し | `Origin` があれば 403 | ブラウザからの直接呼び出しは設計上無い |
| `RunOutcomeKind` | 記述なし | `done \| question \| error \| requeue \| lease_expired` | `lease_expired` は `reclaim_expired_leases` の `WorkerFinished.outcome` |
| `TransitionResult.from` | `Outcome{next, attempts, reason}` に `from` は無い | task-ops が遷移前のタスクから取る | |
| プロバイダの集計 | 記述なし | `task-api` のメモリ内集計（起動時に全走査、以後増分） | 真実ではない観測値 |
| `q` の対象 | D10 は `title` 列のみ | `title` のみ | P-G15 参照 |

## 優先順位（Phase 9 の実装順。ADR-0013「実装の順序」と同じ）

1. **9a 基盤**: P-G2 / P-G11（PRAGMA・版数）→ P-G3（events の id）→ P-G1 / P-G9（task-ops、`show --json`）→ P-G6（`Event` の JsonSchema）→ P-G5（ProviderThrottled）→ P-G7（ページング）
2. **9b API**: `task-api`（`api.md` の 25 エンドポイント、Bearer / Host / パス検査、SSE、`DaemonSnapshot` の `watch`）、`api-v1.schema.json` と一致テスト、`[api]` 設定
3. その後に GUI の G0 を始める

P-G1〜P-G7 は fake ワーカーとローカル SQLite だけでテストできる（ネットワーク不要）。`task-api` のテストも loopback だけで済む（`api.md` §8）。
