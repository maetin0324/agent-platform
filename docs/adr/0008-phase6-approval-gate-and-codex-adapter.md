# ADR-0008: Phase 6 — 承認ゲート（`Approval` 親子ブロック / `Human` check）と `codex` アダプタ

- 日付: 2026-09-14
- 状態: Accepted（Phase 6）
- 関連: `docs/DESIGN.md` §4.2, §5.7, §5.4, §6 Phase 6 / [ADR-0002](0002-state-machine.md) D1/D8 /
  [ADR-0004](0004-taskctl-cli.md) D2 / [ADR-0006](0006-phase4-claude-code-adapter.md) D2-D6 /
  [ADR-0007](0007-phase5-planner-and-reviewer.md) D5

## 文脈

Phase 6 は「`Approval` kind の親子ブロック、`Human` check、`codex` アダプタ」を実装する。受け入れ条件は
「承認前に子が `ready` にならないこと、`reject` で子が `cancelled` になることのテスト」と「`codex` アダプタで
Phase 4 と同じドッグフードタスクが通る」。調査の結果、親子ブロック自体（`TaskStore::ready_tasks` が
「親が存在し `kind=Approval` の場合は親が `Done`」を要求する）は Phase 3 の時点で既に実装済みで
`ready_tasks_excludes_incomplete_dependencies_and_pending_approval_parent` のテストもある
（PROGRESS.md の P-6 が「未解決」のまま残っていたのは記載漏れ）。Phase 6 で新たに要る決定は:

(a) `taskctl reject`（`Approval` kind, `Ready → Failed`）で子タスクをどう `cancelled` にするか、
(b) `Check::Human` をどう解決するか（DESIGN §5.7: 「`Approval` 子タスクを生成して待つ」）、
(c) `codex exec --json` の出力をどうワーカープロトコルの `Terminal` に写すか、(d) taskd の設定配線。

## 決定

### D1. Reject の子カスケードは `apply_transition_tx` に実装し、直接の子のみを対象にする

`SqliteStore::apply_transition_tx` で、トリガーが `Reject`（`kind == Approval` でのみ有効）の場合、
遷移とイベント追記を行った**同一トランザクション内**で `parent_id == task_id` の子のうち
**終端状態（`done`/`failed`/`cancelled`）でないもの**それぞれに `apply_transition_tx(..., Trigger::Cancel, vec![])`
を再帰的に適用する（新規メソッド `cascade_cancel_children_tx`）。

理由・代替案:

- カスケードの深さは**直接の子のみ**にする。`ready_tasks` の親ブロックも直接の親しか見ておらず
  （孫の扱いは元々 DESIGN に規定が無い）、粒度を揃える方が一貫している。孫以降の扱いは未解決事項として残す。
- 既に `done`/`failed`/`cancelled` の子は対象外にする。人間が承認を後から reject しても、既に完了した
  子タスクの成果を遡って取り消すのは原則 4「完了はレビューが決める」に反する（reject は「承認しない」であって
  「なかったことにする」ではない）。
- `Trigger::Cancel` を再利用する（新しい SQL を書かない）ことで、`running` から出る場合のリース解放や
  `Event::Transitioned{reason:"cancel"}` の記録など既存の不変条件をそのまま流用できる。
- `apply_transition` 経由（`taskctl reject`）だけでなく、将来 `Approval` タスクが他経路で `Reject` される
  場合も同じ関数を通るので自動的にカスケードされる。

### D2. `Human` check は `Approval` 子タスクを都度生成し、未解決の間はレビュー全体を延期する

`review_task`（`review.rs`）は純粋な async 関数のままにし、ストアへの副作用（`Approval` 子の生成・照会）は
**ディスパッチャの `spawn_review` 内で、`review_task` を spawn する前に同期的に**行う（`pick_reviewer` が
`Reviewer` run の可否を事前に決めているのと同じ位置）。

1. `task.acceptance` の `Check::Human` の各 `idx` について、`store.list(None)` を `parent_id == task.id &&
   kind == Approval` でフィルタし、`title` が `format!("Approval needed: {} — criterion {idx}", task.title)`
   と**完全一致**する既存の子を探す（実装当初は前方一致を想定していたが、`criterion 1` が `criterion 10`
   を誤って拾う事故を避けるため完全一致にした。監査で指摘され本文を訂正）。無ければ
   `Task{kind: Approval, status: Ready, parent_id: Some(task.id),
   title: <上記>, objective: criterion.text, acceptance: vec![], ...}` を `store.insert` し、
   `Event::ApprovalRequested` を追記する（Approval 子は `taskctl approve/reject` の既存経路にそのまま乗る。
   CLI 変更は不要）。
2. いずれかの `Human` 子が `Ready`/`Draft`（未決）なら、`spawn_review` は `Ok(false)` を返し、対象タスクは
   `reviewing` のまま次 tick に持ち越す（`Reviewer` run の枠待ちと全く同じ「延期」経路を再利用するため、
   `attempts` を消費しない。デーモン再起動後もストアに残る `Approval` 子から自然に復旧できる — メモリ上の
   状態を持つ必要がない）。
3. 全ての `Human` 子が終端に達していれば、`idx -> (pass, reason)` の `HashMap` を組み立てて
   `ReviewExtras::human` として `review_task` に渡す。`Done` なら pass、`Failed`（reject）なら fail
   （理由に `ApprovalDecided.note` があれば含める）、`Cancelled`（別経路での cancel。通常発生しない）は fail。
4. `review_task` の `Check::Human` 分岐は `human.get(&idx)` を引き、無ければ
   `(false, "human approval state missing")` にフォールバックする（呼び出し側が必ず解決済みで呼ぶので
   通常到達しない防御）。

理由・代替案:

- `Reviewer` run と違い `Approval` 子は**ストアに永続化された通常のタスク**なので、Phase 5 の
  `pending_subjects`（メモリ＋`runs/<run_id>/result.json` 復元）のような複雑な再起動復旧が要らない。
  `store.insert` は単発呼び出しであり（Phase 1/2 の監査どおり `Mutex<Connection>` で直列化されるため
  実害なし）、新しい `TaskStore` メソッドは追加しない。
- 「レビュー全体を延期」にしたのは、`Command`/`ArtifactExists` の再実行を毎 tick 繰り返さないため、かつ
  Phase 5 の「決定的条件が全 pass のときだけ Reviewer run」の考え方（機械的に安く判定できるものを先にやる）
  とは別に、Human だけ最優先で止める方が「人間の関与は承認ゲートとして明示する」（原則 5）に合う。
- 子の `acceptance` は空にする（`Approval` kind は `running`/`reviewing` に入らないので判定されない）。

### D3. `codex` アダプタは `claude-code` アダプタと同じ「結果ファイル規約」を使う

`codex exec --json` は `thread.started` → `item.*`（進捗）→ `turn.completed`/`turn.failed` の JSON Lines を
吐く（Phase 0 調査、ADR-0001）。`claude-code`（ADR-0006 D3/D4）と同じパターンを踏襲する:

- 起動: `codex exec --json <prompt>`（既定コマンド `"codex"`。`extra_args`/`model`/`env` は設定可能）。
  プロンプトはコマンドの最終引数として渡す（`claude -p <prompt>` と同じ位置づけ）。
- `build_prompt` は `claude_code::build_prompt` と同じ内容（`artifacts/result.json` 規約、kind 別の文面）を
  再利用する。ワーカーの自己申告ではなく `artifacts/result.json` を正とする点も同じ。
- 終端判定: `turn.completed` または `turn.failed` を一度でも観測できたら「正常終了」とみなし
  `artifacts/result.json` を読む（`turn.failed` はそのまま retryable な `Error` にする）。**どちらも一度も
  観測できずに exit した場合はクラッシュとして扱い、`artifacts/result.json` を一切信用しない**
  （ADR-0006 D4 と同じ理由: 書きかけ・前回 run の名残を誤読しない）。
- 進捗: `item.*`（`item.started`/`item.completed` 等、`type` 先頭が `item.` のもの）を `EventSink::progress`
  に変換する（内容を JSON のまま 500 文字に丸めるだけの簡易変換。`claude-code` ほど構造化しない — codex の
  `item` の型は Phase 0 調査時点で確定しておらず、過度な構造依存はしない）。
- run 開始直前に前回の `artifacts/result.json` を削除する（ADR-0006 監査の教訓をそのまま踏襲）。
- 生存監視・強制終了は `subprocess.rs` の既存関数（`read_line_limited`/`kill_now`/`reap_after_terminal`）を
  再利用する（`claude_code.rs` と同じ構成）。

### D4. taskd 設定・配線

`[adapters.codex]`（`command`・`extra_args`・`model`・`env`。既定 `command = "codex"`）を追加し、
`config.rs::validate` が `adapter = "codex"` を受理するようにする。`taskd::build_dispatcher` が
`CodexAdapter` を組み立てて `adapters` map に登録する（`claude-code` と同じパターン）。
`config/taskd.codex.example.toml` をドッグフード用に追加する。

## 影響

- `store.rs`: `apply_transition_tx` に子カスケード、`cascade_cancel_children_tx` を追加。
- `review.rs`: `ReviewExtras::human: HashMap<usize, (bool, String)>` を追加、`Check::Human` 分岐を実装。
- `dispatcher.rs`: `spawn_review` に Human 解決ステップを追加。
- `task-worker/src/codex.rs`（新規）、`lib.rs` の re-export。
- `taskd/src/config.rs`・`taskd/src/lib.rs`: `codex` アダプタの配線。
- CLI（`taskctl`）変更なし（`Approval` 子は既存の `approve`/`reject` 経路をそのまま使う）。

## 未解決事項（次 Phase 以降）

- 孫以降への cascade cancel / 親ブロックは対象外（直接の子のみ）。
- `Human` 子の `title` 完全一致によるマッチングは、`Approval` 子が一度 `Done`/`Failed` になった後にタスクが
  別条件の fail で再度 `ready`→レビューに回っても**同じ子を再利用する**（sticky）。新しい成果物に対して
  人間の再承認なしに Human 条件が pass/fail してしまう。監査で指摘。原則 5 の趣旨からは再実行のたびに
  新しい `Approval` 子を作るべきだが、Phase 6 では対応しない。
- reject された `Approval` 子は再利用され続けるため、`max_retries > 0` のタスクは
  「ワーカー再実行 → 同じ `Failed` 子を読んで再度 fail」を上限まで無駄に繰り返す（Phase 5 未解決事項 13 と
  同種の問題が reject 経路にも残る）。
- Human 条件を持つ親タスクが `cancel`/`failed` になっても、生成済みの `Ready` な `Approval` 子は誰も
  クローズしない（孤児化）。人間の承認待ちキューにゴミが残りうる。
- `ready_tasks` の探索窓（`max_concurrency * 4 + 16`）を、`acquire_lease` が常に失敗する `Approval` 子
  （kind ガードにより非ディスパッチ対象）が占有しうる。承認待ちが大量に滞留すると後続の実タスクが
  飢餓する可能性がある。
- `create_human_approval_child` の `insert` と `Event::Created` の追記は非トランザクション
  （`taskctl add` と同じ既存パターン。`complete_plan` は同一トランザクションだが今回は揃えていない）。
- `codex exec --json` の `item`/`turn` イベント形は、本セッションで実機（codex-cli 0.154.0、認証は ChatGPT
  アカウントで通ったが現在のプランでは対応モデルが無く `turn.failed` で終わる）に対して
  `codex exec --json <prompt>` を直接実行して確認した範囲では ADR の記述どおりだった
  （`thread.started` → `item.*` → `turn.completed`/`turn.failed`、`turn.failed.error` はオブジェクト
  `{"message":"..."}`。`codex.rs::describe_error` として反映済み）。ただし `taskd` 経由でワーカーとして
  起動する実機ドッグフード（DESIGN §6 Phase 6 の受け入れ条件）は本セッションのサンドボックス制約
  （ネストしたエージェント起動がブロックされる。Phase 4 と同じ制約）と、このアカウントでのモデル利用不可の
  両方により実施できなかった。`docs/PROGRESS.md` の「人間による確認待ち」を参照。
- 起動引数（`exec --json` の順序、`--model`、`extra_args`、プロンプトが最終引数であること）を検証する
  テストが無い（`stub_codex` は引数を無視するため、コマンド構築が壊れてもテストは緑のまま）。
