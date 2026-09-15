# ADR-0014: Reviewer run のイベント記録、objective の検索、作成時の検証（P-G14 / P-G15 / P-G16）

- 日付: 2026-09-14
- 状態: Accepted（人間の判断「P-G14〜16 は推奨通り」。提案は `docs/gui/taskd-proposals.md`）
- 関連: ADR-0007 D5（Reviewer run）、ADR-0010 D5（Reviewer run の延期）、ADR-0012 D1（`WorkerStarted.provider`）、ADR-0013 D4 / D10、`docs/gui/api.md`

## 文脈

Phase 9（ADR-0013）の後、GUI の設計（Fable）が taskd への追加提案を 3 件出し、人間が提案どおりの採用を決めた。

- **P-G14**: Reviewer run は `WorkerStarted` / `WorkerFinished` を残さない。進捗は `ReviewerSink` が対象 run の `WorkerProgress` に付け替えるだけなので、アカウント別のトークン集計（`GET /providers` の `stats`）から Reviewer run の分が漏れる。
- **P-G15**: 一覧の検索（`GET /tasks?q=`）が `title` しか見ないので、目的（`objective`）の文言で探せない。
- **P-G16**: `task_ops::add::create_task` は `title` / `objective` の空白と `parent` の存在を検査しない。API 越しに空タイトルや、存在しない親を指す子を作れてしまう。

## 決定

### D1. Reviewer run も `WorkerStarted` / `WorkerFinished` を対象タスクに記録する（P-G14）

- `task_core::RunRole { Worker, Reviewer }`（snake_case）を追加する。`Event::WorkerStarted` と `Event::WorkerFinished` に
  `role: Option<RunRole>` を任意フィールドとして足す（`#[serde(default, skip_serializing_if = "Option::is_none")]`）。
  - **`None` はワーカー run**。既存のイベントも、これからのワーカー run の直列化も変わらない（`Some(Worker)` は書かない）。
  - Reviewer run は `role: "reviewer"`。
- 記録の時点:
  - `WorkerStarted{run_id: <Reviewer run の id>, adapter, model: 実効モデル, provider, role: reviewer}` は、ディスパッチャが Reviewer run を
    起動する直前に追記する（プロバイダを選び、spawn を決めた後）。
  - `WorkerFinished{run_id, outcome, usage, role: reviewer}` は `on_review_finished` で追記する。判定を適用する場合は
    `ReviewVerdict` と同じトランザクション（先頭）、延期する場合は延期の `WorkerProgress` と一緒、判定を捨てる場合（もう reviewing でない）も単独で追記する。
- `outcome` の文字列はワーカー run と同じ接頭辞の規則に従う（`api.md` §5.2 の分類がそのまま使える）:
  - `done: <summary>` / `question: <text>` / `error(retryable=<b>): <message>`
  - アダプタの起動失敗など（供給側でない）: `error(retryable=false): adapter error: <e>`
  - 供給側の失敗で延期: `requeue: <message>`。連続延期が上限に達して判定する場合は `error(retryable=false): requeue limit (<n>) reached: <message>`
- `usage` は Reviewer run の `done` の `usage`（それ以外は `null`）。
- **ワーカー run を前提にする派生値は `role: reviewer` を除外する**:
  `task_ops::derive::last_run_id` / `latest_question`、受信箱の質問（`asked_at` / `run_id`）・Plan の要約・失敗の理由。
  ディスパッチャの `recover_reviews`・成果物・`prior_review` は `last_run_id` を通るので、同じく Reviewer run を見ない。
- `task_ops::view::runs` は Reviewer run も一覧に含め、`RunSummary.role`（`worker` | `reviewer`）で区別する。GUI は Reviewer run のログにも辿れる。
- プロバイダの集計（task-api の `stats`）は役割を区別せず Reviewer run も数える（P-G14 の目的）。
- `DaemonSnapshot.in_flight[]` の `kind: "reviewer"` の `run_id` は、Reviewer run 自身の id にする（ADR-0013 実装メモの「対象ワーカー run の id」を改める。
  Reviewer run を使わないレビュー（`Command` 等だけ）は従来どおり `in_flight` に現れない）。
- 真実の範囲: `replay` は `Created` / `Transitioned` だけを見るので影響しない。デーモンが Reviewer run の途中で止まった場合、
  その run の `WorkerFinished` は残らない（ワーカー run の `lease_expired` に相当する回収は行わない。一覧では未完了の run に見える）。

### D2. `tasks` に `objective` 列を追加し、`q` の対象にする（P-G15）

- マイグレーション 0004（`SCHEMA_VERSION = 4`）: `ALTER TABLE tasks ADD COLUMN objective TEXT NOT NULL DEFAULT ''`、既存行は `json` から埋める。
- `objective` は作成後に変わらないので、列を書くのは挿入時だけ（遷移の UPDATE は触らない）。
- `ListFilter.title_contains` を `text_contains` に改名し、`title LIKE ? OR objective LIKE ?`（`%` `_` はリテラル）で絞る。
  SQLite の LIKE なので **ASCII の大文字小文字は区別しない**（非 ASCII は区別する）。Phase 9 の `api.md` §3.3 は「区別する」と書いていたが、
  title だけの検索の時点から実装は区別しておらず、文書の誤りだった。検索としては区別しない方が使いやすいので、文書を実装に合わせる。
- 先頭一致でない LIKE は索引を使わないので、索引は足さない。全文検索（FTS5）は引き続き採らない。

### D3. タスク作成の検証を足す（P-G16）

- `task_ops::add::create_task` の検証の順: `title` が空白だけ → `title must not be blank`、`objective` が空白だけ → `objective must not be blank`、
  受け入れ条件が空（既存）、`parent` が存在しない → `parent <id> does not exist`、`depends_on`（既存）。いずれも `OpsError::Validation`（API は 422）。
- `taskctl add` も同じ関数を通るので、`--title ""` 等はエラーになる（exit 1。挙動の変更）。
- `create_plan` は既存の `goal must not be blank` だけ（`title` / `objective` は goal から作り、親は持たない）。
- task-api の `errors[].field` の推定に `title` / `objective` / `parent` を足す。

## 結果

- イベントの JSON Schema（`docs/api/v1/event.schema.json`）と API のスキーマ（`api-v1.schema.json`）を再生成する。
- `docs/gui/api.md`（§3.1 の版数、§3.3 `q`、§3.4 の検証、§5.2 / §5.8、§6.2、§9、§10）と `docs/gui/taskd-proposals.md` の状態を更新する。
- DESIGN.md §4.3（`Event` の定義）と §5.1（`tasks` の列）への反映は PROGRESS の提案に書く（DESIGN.md は編集しない）。
