# Task execution decomposition（ADR-0072）E6 dogfood: 分析と最終報告

- 日付: 2026-09-25
- 対象: ADR-0072（`docs/adr/0072-task-execution-decomposition.md`）Phase E0〜E6
- このレポートはコードを変更していない（`docs/` 配下のみ）。数値はすべて
  `/tmp/claude-1001/-home-rmaeda-workspace-agent-platform/a4058fdb-50a6-4f32-a9f3-589e4694447f/scratchpad/e6/`
  にある JSON（本番 API から取得済みの読み取り専用コピー）から数えたもので、本番には触れていない。
  推測は本文に「推測」と明記する。

---

## 1. 何が変わったか（実際にどこまで達成できたか）

人の依頼の前提は「Task ≒ 1 巨大 LLM execution」から「Task = user-visible goal /
ExecutionPlan = internal plan / WorkUnit = context-isolated work / Run = one harness attempt」への
変更。E0〜E6 の実装を、コードの根拠つきで**達成 / 部分達成 / 未達**に分類する。

### 達成

1. **4 層のデータモデルと決定的 scheduler**（D1、D5、D15）。
   - migration 0026（`crates/task-core/migrations/0026_execution.sql:49` `execution_plans`、
     以下 `work_units`、`runs` の 3 表。`SCHEMA_VERSION = 26`、`crates/task-core/src/store.rs:83`）。
   - 検証つきの計画採用: `crates/task-core/src/execution_plan.rs:290 pub fn validate(...)`。
   - replanning: `crates/task-ops/src/execution.rs:130 pub fn replan(...)`（done の WU を保持し、
     旧版を supersede）。
   - dogfood タスク 01M3C33KW8YH336QDD0QAV45H8 で実測: `execution-plan.json` の `work_units` は
     8 件全て `status: done`、各 `runs: 1 / continuations: 0 / retries: 0`（WU 単位で正しく独立した
     カウンタを持つ）。3 つの計画版（v1 6 WU → v2 7 WU → v3 8 WU）が `ExecutionPlanned.supersedes`
     で監査できる形で残っている（§3 参照）。
2. **budget 切れを Task の失敗にしない**（D7、D9、D11、D12）。E1 で実装、コードは本番稼働中
   （`docs/PROGRESS.md` Phase E1「本番反映」節、release `93076d0f4c76`）。dogfood では
   `budget_exhausted_by_kind: {}`、`continuations: 0`、`retries: 0`（8 WU とも 1 run で完了したため
   この仕組み自体は今回発火していないが、比較対象の「before」タスク 01M38J4X53P1Y684FS42Z6R0VZ が
   まさにこの種の予算切れ・喪失で失敗しており、その対策として E1 の効果が期待どおりに働く条件は
   コードで確認できる）。
3. **Complexity Gate の決定的な判定と記録**（D13）。`crates/task-core/src/execution_gate.rs:179
   pub fn decide(...)`。`out_of_scope_rule` → 人の明示（`human_execution`）→ 規則表、の優先順位は
   コードどおり（`execution_gate.rs:187-210`）。dogfood の `gate.rule_id == "human/explicit"`、
   `gate.score == 0` はこの優先順位の帰結（人の明示があると `Scorer` 自体を作らず即座に返すため、
   `score: 0` は「規則表が 0 点と判定した」ではなく「規則表を評価していない」ことを意味する。
   §5 で詳述）。
4. **Planner（task-local、部署 lead の profile、node_sessions を resume しない）**（D3、D14）。
   dogfood の 3 回の planner run はいずれも `role: planner`、`model: claude-fable-5-1`、
   `lane: frontier`（`rule_id: planner/system-frontier`）で走り、`session_id` を持たない
   （`crates/task-dispatch/src/dispatcher.rs` の `dispatch_ready`／`RunContext.session.is_none()`。
   Phase E3 PROGRESS 節の単体テスト `gate_on_compound_task_runs_a_planner_then_the_planned_work_units_in_order`
   で確認済み）。
5. **reviewer repair（D16）と replanning（D17）**。E4/E4b/E6 で実装。dogfood で replan が
   2 回、実際に発火した（§3）。ただし D16 の「reviewer の不合格を局所修復する」経路そのものは
   dogfood では発火していない（最終レビューの不合格は 2 回とも `substantive`〈環境要因とマージ衝突〉
   で、D16 の `format`/`lint`/`test_small`/`reviewer_local` のどれにも当たらなかった。§3・§4）。
6. **metrics（D19）**。`crates/task-core/src/execution_metrics.rs:84 pub fn summarize(task: &Task,
   events: &[Event]) -> ExecutionMetrics`。`GET /tasks/{id}/execution` と `GET /metrics/execution`
   は実装され、dogfood・比較対象の両方から実際に値が取れている（本レポート §2 の比較表はこの
   API の出力そのもの）。
7. **GUI の Execution 節、ExecutionPhase**（D20）。E5 で実装、`mobile-audit` 違反 0。

### 部分達成

1. **配送の repair（D16 の `merge_base`、E4 の任意項目 (h)）**: E6 で実装されたが、dogfood の
   実際の repair（`merge-main` WU、§3）は D16 の `classify_review_failure` 経路ではなく、
   **replan で新しく足された `kind: repair` の WU**として発生した。`repairs_by_class: {"unknown": 1}`
   がその証拠（§3 で詳述。E5 の PROGRESS 申し送り「既に計画のある Task への repair 追加時に
   class を events だけから復元できない」がそのまま dogfood で再現した）。D16 の本来の経路
   （最終レビュー不合格 → `classify_review_failure` → repair WU）は今回 1 回も通っていない。
2. **`GET /metrics/execution` の性能**（E6 の受け入れ条件どおり索引ベースに変更）。ただし
   `metrics-execution.json`（`group_by=gate_mode`、`since=2026-09-24`）を見る限り、gate が
   `shadow`/`on` で実際に判定を記録したタスクは 46 件中 dogfood の 1 件だけで（下記「未達」参照）、
   索引の実運用でのスケール（数千件規模）は未確認のまま（E6 の PROGRESS 節が書く「2,000 タスク ×
   20 events の手元計測」は合成データでの性能測定であり、実データでの検証ではない）。
3. **ExecutionPhase の「done 後」の扱い**（D6 の表どおりの実装だが、実際の API 出力で気づきにくい形）。
   `crates/task-ops/src/view.rs:1161-1184 fn execution_phase(...)` は `Status::Reviewing` と
   `Status::Running` 以外は常に `None` を返す（`:1183 _ => None`）。dogfood の
   `GET /tasks/{id}/execution` 相当（`01M3C33KW8YH336QDD0QAV45H8.execution.json`）にも
   `phase` キー自体が存在しない（`gate`/`plan`/`runs`/`metrics` の 4 キーのみ）。これは D6 の表
   （「done / failed / cancelled | 同じ名前の Status」）どおりの**仕様どおりの挙動**であり、
   バグではないが、GUI で「done になった瞬間に Execution 節の phase バッジが消える」という
   体験になる。E5 のときの申し送り（「空タスクは running でも None」）の裏返しの形で、E6 で
   実機確認できたという意味では部分達成にとどめる。

### 未達

1. **shadow モードでの閾値調整（D13、U10）**: `docs/PROGRESS.md` Phase E3〜E5 は繰り返し
   「S4/S6 の実データ配線は shadow の記録を見てから」「gate の閾値は E6 の結果で調整」と書いているが、
   `metrics-execution.json` の実測は `gate_mode=compound: 1 件（dogfood 自身、人の明示）` /
   `gate_mode=none: 45 件` のみで、**規則表が実際に compound/atomic を判定した組織内の実例が
   1 件もこの期間に記録されていない**（本番設定は Phase E4/E4b/E5 反映まで `gate=shadow` の
   はずだが、`shadow` でも `ExecutionGated` は記録されるはずの信号が今回のデータでは確認できない）。
   閾値・重み（F1〜F5、S1〜S6）は E0 時点の推測値のまま、実質未検証。
2. **WU 並列実行（U3）、部署をまたぐ WU の扱い（U5）、`idle timeout` の分類（U7）、
   remote worktree の mechanical checkpoint（U9）**: 明示的に対象外のまま（ADR §7 のまま）。
3. **S4（複数の実行環境）・S6（過去の類似タスクの budget_exhausted 率）の実データ配線**:
   Phase E3 の逸脱節どおり既定値（`false`/`None`）のまま。dogfood でも `gate.signals: []`
   （human/explicit のため計算されていない）なので、この 2 信号が実際に動く様子はまだ一度も
   観測できていない。
4. **`gate` の本番既定を `on` にするかの判断材料**: ADR は「E6 の dogfood の結果を見て、人が
   `on` に切り替える」としているが、上記のとおり実質 N=1（それも人の強制）のサンプルしかなく、
   判断材料としては不十分（§5 で詳述）。

---

## 2. before / after 比較表

3 件のタスクについて、`events`/`execution` の実測値を並べる。算出方法は各列の脚注のとおり。

| 指標 | dogfood（after）<br>01M3C33KW8YH336QDD0QAV45H8 | 巨大 session（before）<br>01M38J4X53P1Y684FS42Z6R0VZ | 同タスクの retry 複製<br>01M39FGDAE9XQA3FGMCP5MCW0B |
|---|---|---|---|
| 実行方式 | compound（gate=human, 8 WU, 3 計画版） | atomic（1 Task = 1 session の繰り返し、ExecutionPlan なし） | atomic（同上、Phase 116 後） |
| 期間 | 2026-09-25 10:52:18Z 〜 14:58:22Z | 2026-09-24 01:58:11Z 〜 03:46:30Z | 2026-09-24 10:31:18Z 〜 10:55:55Z |
| 壁時計（`ExecutionMetrics.wall_ms`） | 14,764,351 ms（4h6m4s） | 6,498,999 ms（1h48m19s） | 1,478,846 ms（24m39s） |
| 最終状態（`final_status`） | done | **failed** | done |
| max-turn failure 回数（注1） | 0 | 0（`error_max_turns` 自体は起きていない。失敗要因は lease 失効） | 0 |
| budget_exhausted（種類別） | `{}`（0 件。E1 の仕組みは今回発火せず） | 概念自体が存在しない（pre-E1。当時は `Terminal::Error{retryable}` のみ） | 同左 |
| continuation 数 | 0（8 WU とも 1 run で完了） | 0（存在しない仕組み） | 0 |
| retry 数（WU 単位 / attempts） | WU retries=0（全 WU）。Task の attempts は**推測 2**（注2） | attempts **推測 3**（注2、`max_retries=2` の上限で failed） | attempts 1（review-fail 1 回 → retry 1 回 → pass） |
| repair 数 | 1（`repairs_by_class: {"unknown": 1}`。D16 経路ではなく replan で追加された `kind:repair` WU。§3） | 0（仕組みなし） | 0（仕組みなし） |
| replan 数 | 2（v1→v2、v2→v3。§3） | 0（仕組みなし） | 0（仕組みなし） |
| run 数（役割別） | 14（planner 3 / worker 8 / reviewer 3） | 5（worker 3 / reviewer 2）（注3） | 4（worker 2 / reviewer 2） |
| peak context | **未計測**（`RunMetrics.peak_context_tokens` は E1 で型は追加されたが実測配線は未実装。U2 のまま） | 未計測（同上） | 未計測（同上） |
| 入力トークン | 65,118,541（うち Claude 系 3 run の合計 934、codex/gpt-6-sol 8 run の合計 65,117,589。注4） | 44（claude-sonnet-5、4 run 合計。lease 失効 run の usage は喪失） | 40（claude-sonnet-5、4 run 合計） |
| 出力トークン | 252,271 | 14,725 | 12,895 |
| cache_read トークン | 5,304,617（すべて Claude 系 run。codex/gpt-6-sol は `cache_read_tokens` フィールドが無い＝キャッシュ非対応） | 1,605,878 | 2,590,568 |
| 費用（`cost_usd`、注5） | **$11.21**（実際は過小評価。§4） | $0.36 + $0.63 + $0.43 + $0.34 = $1.76（4 run、うち lease 失効 run は usage 喪失で $0 計上） | $0.55+$0.31+$0.40+$0.41 = $1.67 |
| 失敗の原因分類（注6） | N/A（done） | Infra（drain によるlease 失効・usage 喪失）→ Work（review fail ×2、`cargo fmt` 違反ほか） | Work（review fail 1 回、PROGRESS 衝突・fmt 未解決・artifacts 空） |

脚注:
1. 「max-turn failure」は `WorkerFinished.outcome` に `error_max_turns` 相当の文言、または
   `RunEnd::BudgetExhausted{kind: Turns}` を持つ run の数。3 タスクとも events/execution JSON に
   この種の outcome は現れない（0 件）。
2. `Task.attempts` の生の値は今回のオフラインコピーに含まれない（`GET /tasks/{id}` 相当の
   `*.task.json` は 3 タスクとも `404 not_found` で取得できていない）。表の値は D11/D17 の遷移規則
   （ReviewFail は `retry_or_fail` で attempts+1。replan の起点が ReviewFail のときだけ attempts+1、
   それ以外の replan は消費しない）を、観測できた ReviewFail の回数に適用した**推測**。
   - dogfood: reviewer run が 2 回とも実質不合格（1 回目は criterion 1 のタイムアウト、2 回目は
     criterion 5 のマージ衝突）で replan を誘発しているため、ReviewFail 2 回 → attempts 推測 2。
   - before（01M38J4X53P1Y684FS42Z6R0VZ）: reviewer run が 2 回とも `差し戻し`（不合格）で、
     `max_retries` の既定値 2 に対し 3 回目の試行機会がなく failed になっている。lease 失効
     （1 回目の worker run）も、ADR-0072 D12/調査 §6 の「死んでいれば `InfraRequeue`」ではなく
     旧経路で attempts を消費した可能性がある（Phase 116 以前の挙動。調査時点でも
     `InfraRequeue` は Phase 116 導入）ため、ReviewFail 2 + lease 失効 1 の合計で attempts
     推測 3（`max_retries=2` を超えて failed、という筋と整合する）。
   - retry 複製（01M39FGDAE9XQA3FGMCP5MCW0B）: reviewer run が 1 回不合格、2 回目で合格。
     attempts 推測 1。
3. before タスクの run 数は worker 3（1 回目 `lease_expired`、2 回目 `done`、3 回目 `done`）+
   reviewer 2（両方とも不合格）。`ExecutionMetrics.runs_by_role` は `{}`（空）だが、これは実際に
   run が無いのではなく、この task の `WorkerFinished` イベントが Phase E3（`role` フィールド追加）
   より前のもので `role` を持たないため、`execution_metrics::summarize` が数えられない
   （§7 で詳述する後方互換の穴）。表の値は `execution.json` の `runs` 配列（`role` フィールドは
   `view.rs` 側の表示ロジックが別の手がかりから埋めている）を直接数えたもの。
4. dogfood の入力トークンの大半（65,117,589 / 65,118,541 = 99.9%）は 8 件の codex（`gpt-6-sol`）
   worker run で、これらは `cache_read_tokens`/`cache_creation_tokens` フィールド自体を持たない
   （usage が `{"input_tokens": N, "output_tokens": M}` の 2 欄のみ）。「入力トークンの大半は
   cache read」という前提は、少なくとも `Usage` の生データからは確認できない。Claude 系（planner
   ×3、reviewer ×3）だけを見れば input 934 token・cache_read 5,304,617 token で、確かに
   ほぼ全てが cache read だが、これは総入力トークン 65.1M のうち 0.001% にすぎない。§4 で詳述。
5. `cost_usd` は `task_core::pricing::estimate_cost_usd`（`crates/task-core/src/pricing.rs`）の
   `PRICE_TABLE` に基づく。`gpt-6-sol`/`gpt-6-astra`/`gpt-6-luna` は 4 欄とも `None`
   （`pricing.rs:70-77`）なので、これらのモデルを使った run は費用計算から**まるごと除外**される
   （0 円ではなく「計上されない」。関数の契約どおりだが、集計結果を読む側には $0 に見えてしまう）。
   dogfood の 14 run のうち 8 run（全 worker run）が `gpt-6-sol` で、費用が反映されているのは
   Claude 系の 6 run（planner 3 + reviewer 3 のうち usage を持つ 5 run）だけ。
6. 「失敗の原因分類」は `ReviewVerdict.reason`／`WorkerFinished.outcome` の文面から人が読み取った
   もの（`classify_task_failure` の `FailureClass::{Infra,Work}` の生の値は今回のコピーに
   含まれない）。

---

## 3. dogfood の実行の流れ（時系列）

すべて `01M3C33KW8YH336QDD0QAV45H8.execution.json` の `runs` 配列、
`01M3C33KW8YH336QDD0QAV45H8.execution-plan.json` の `versions`、
`execution-plan.v1.json`/`v2.json`/`execution-plan.json`（= v3）の `rationale` から。

| 時刻（UTC） | 出来事 |
|---|---|
| 10:52:18 | Task 作成、`ExecutionGated{mode: compound, source: human, rule_id: human/explicit}`。gate は人の明示による強制で、規則表は評価されていない |
| 10:52:18–11:06:05 | planner run #1（`claude-fable-5-1`, frontier, 13m46s）。v1 採用: 6 WU（`delivery-repair-impl`→`delivery-repair-e2e`、`pricing`、`metrics-store`→`metrics-api`、`finish`） |
| 11:06:05–13:58:51 | worker run ×6（すべて `codex`/`gpt-6-sol`, standard lane）が v1 の 6 WU を順に実行、全て 1 回で done（`delivery-repair-impl` 44m43s、`delivery-repair-e2e` 19m20s、`metrics-store` 17m33s、`metrics-api` 40m39s、`pricing` 22m31s、`finish` 28m0s） |
| 13:58:51– | reviewer run #1（`claude-opus-5-5`）開始。**`runs` 索引にこの run の `finished_at`/`usage`/`end` が記録されていない**（§4 の問題1） |
| 14:11:32–14:21:19 | planner run #2（replan）が v2 を採用: v1 の 6 WU を不変で保持し、`review-warm`（test、`finish` に依存）を追加。**理由（v2 の rationale より）**: 最終レビューの criterion 1（`cargo test --workspace`）が 600 秒でタイムアウト。原因はコードではなく検査環境 — レビューの deterministic check が worktree の `target/debug` を使うが、`finish` WU は sandbox の制約で別の `CARGO_TARGET_DIR`（`target/e6-metrics-api`）にビルドしていたため、レビュー時に cold compile が発生した |
| 14:21:29–14:31:23 | worker run（`review-warm`、9m54s）: レビューと同じ条件（`env -u CARGO_TARGET_DIR`、sandbox 外）で HEAD の test binary を温める。コードは変えない（test 種別の WU） |
| 14:31:23–14:34:10 | reviewer run #2（2m47s、`outcome: done`）。verdict は不合格（実質的な不合格。criterion 0〜4 pass、criterion 5 fail） |
| 14:34:32–14:37:52 | planner run #3（replan）が v3 を採用: v1+v2 の 7 WU を不変で保持し、`merge-main`（**kind: repair**、`review-warm` に依存）を追加。**理由（v3 の rationale より）**: ブランチ基点 `a119458` に対し main が `f479ca5` に進んでおり、`git merge-tree` で `docs/PROGRESS.md` が衝突する（`git merge-base --is-ancestor main HEAD` が exit 1）。コードの衝突は無く、両方の PROGRESS 節を残す形で解消可能と判定 |
| 14:38:13–14:55:49 | worker run（`merge-main`、17m36s）: main を merge し `docs/PROGRESS.md` の衝突を解消、fmt/test/clippy を再実行、release/verify を merge 後の sha（`5cc1610938f5`）で再実行 |
| 14:55:49–14:58:23 | reviewer run #3（2m34s、`outcome: done`）。**合格**、Task `done` |

### v1 → v2 → v3 の差分（`rationale` の要旨）

- **v1（6 WU）**: 3 つの独立した成果（配送の局所修復・単価表・metrics 集計性能）を、それぞれの
  実装／テストの枝に分け、最後に `finish`（fmt・test・clippy・report・commit を束ねる）を置いた。
  「工程ごとに 1 context」という ADR の狙いどおりの分解。
- **v2（7 WU、+1）**: v1 の 6 WU は**すべて done のまま変更なし**。追加した `review-warm` は
  「コードは変えず、レビューが見る target を温めておく」という**検査環境の不整合への対処**で、
  goal そのものの分解ではない。
- **v3（8 WU、+1）**: v1+v2 の 7 WU は**すべて done のまま変更なし**。追加した `merge-main` は
  `kind: repair` で、**main の先行による merge 衝突の解消**という、実装内容とは無関係な
  「配送直前のリベース作業」。D17 の「done の WU を保持する」という replan の設計は 2 回とも
  正しく機能した（既存の実装をやり直していない）。

### attempts が 2 になった理由

ADR-0072 D11 の表:「replan | review 不合格が起点なら +1（`ReviewFail` のまま）」。D17 4.
「計画のある Task の最終レビューで、実質的な不合格が出た（`ReviewFail` の後の最初の dispatch）」が
replan のトリガーの 1 つ。

dogfood では、reviewer run #1・#2 がいずれも**修復できない種類の不合格**（環境要因のタイムアウト、
main 進行によるマージ衝突）で、D16 の `classify_review_failure` の 5 分類
（`format`/`lint`/`test_small`/`reviewer_local`/`merge_base`）のどれにも当たらず、`substantive`
に分類された（`merge_base` は D16 の表では「配送の `[delivery-repair]` が扱う技術的失敗」専用で、
Task 内部の最終レビュー段階のマージ衝突はこの分類の対象外）。`substantive` な不合格は
`Trigger::ReviewFail`（Reviewing → Ready、attempts+1）が先に発生し、そのあと
`wu_dispatch_gate` が「計画の WU は全部 done なのに Ready に戻った」状態
（`NextStep::AllDone`）を検出して `replan_gate` に倒す（`dispatcher.rs`。Phase E4 の PROGRESS 節
「`AllDone`…も `replan_gate` に倒す（repair 枯渇後の `ReviewFail` や実質的な review 不合格の後の
再 dispatch）」）。つまり **1 回の「実質的な review 不合格」ごとに `ReviewFail`（attempts+1）→
`Continue{Replan}`（attempts 不変）の 2 段階を経る**。2 回発生したので attempts は推測 2
（生の `Task.attempts` は今回のオフラインコピーに含まれず直接確認はできていない。§2 注2）。

### `repairs_by_class: {"unknown": 1}` の意味

`execution-plan.json`（v3）の `merge-main` WU は `kind: "repair"` を持つが、これは D16 の
`classify_review_failure` → `try_review_repair`（`dispatcher.rs:5027`）経路で作られたものでは
**ない**。v3 の rationale にあるとおり、これは **planner が replan の一環として `kind: repair`
の WU を計画に書いた**もの（D17 の replan は「新しい版の全体」を出すだけで、D16 の repair 判定
ロジックを通らない）。`ExecutionMetrics.repairs_by_class` は `WorkUnitTransitioned` の title
接頭辞（`"repair (<bucket>): …"`）から class を復元する実装（Phase E4「per-class カウンタは
WU の title の接頭辞から復元する」）だが、replan で足された WU の title
（`"配送の main 取り込みと再検査（repair）"` 相当。正確な文字列は `execution-plan.json` の
`merge-main.title` 参照）は `try_review_repair` が付ける `"repair (merge_base|format|...): "` の
形式に従っていないため、class を復元できず `"unknown"` に落ちる。これは Phase E5 の PROGRESS
申し送り「既に計画のある Task への repair 追加時に class を events だけから復元できない」が
そのまま実例として再現したもので、**バグではなく既知の設計限界の実証**である。

---

## 4. 観測された問題と改善提案

### 問題 1: 最終レビューの不合格が「repair」ではなく「replan + ReviewFail」に分類される

D16 は「不合格が全部修復できる種類のときだけ repair にする」と定義しているが、dogfood で実際に
起きた 2 つの不合格（検査環境のタイムアウト、main 進行によるマージ衝突）はどちらも
D16 のどの class にも該当しない。結果として、**本来なら「レビュー環境を直すだけ」「リベースする
だけ」で済む軽微な不合格が、attempts を消費する `ReviewFail` + 重い replan（planner run を
1 回丸ごと起こす）という重い経路を通った**。特に v1→v2 の原因（cold compile によるタイムアウト）
は、コードにもレビュー観点にも問題が無く、純粋に「レビューとワーカーで `CARGO_TARGET_DIR` が
食い違っていた」という環境要因であり、D16 の `test_small`（テスト失敗 1〜3 件）とも性質が違う
（テストは失敗しておらず、時間切れになっただけ）。
**提案**: D16 に「review timeout（決定的検査がタイムアウトで終わった）」という新しい class を足し、
`review-warm` のような「コードを変えず検査環境を温めるだけの WU」を repair 扱い（attempts を
消費しない）にする。また `merge_base` の repair は、現状「配送段階の `[delivery-repair]` 専用」
という限定を外し、**Task 内部の最終レビュー段階でも同じ分類を使えるようにする**
（`classify_review_failure` の入力に `git merge-base --is-ancestor` の結果を足す）。

### 問題 2: `GET /tasks/{id}/artifacts` が空

`01M3C33KW8YH336QDD0QAV45H8.artifacts.json` は `{"items":[]}`。一方
`dogfood-artifacts-ls.txt`（worker の artifacts ディレクトリの `ls`）には `report.md`、
`gate.json`、`verify.json`、`review.json` など 27 ファイルが実在する。原因は
`crates/task-api/src/files.rs:246 pub(crate) fn artifact_views(...)` が
`Event::ArtifactProduced` イベント（`files.rs:234-239 fn produced(...)`）だけを数える実装で、
ディスク上の `artifacts/` ディレクトリを走査しないため。dogfood の 8 worker run・3 planner run は
1 件も `ArtifactProduced` を発行していない（result.json に `report`/`evidence` を書いても、この
イベントは自動生成されない設計）。
**提案**: (a) 短期には、worker のプロンプト指示（D10 の graceful yield と同じ層）に
「主要な成果物（report.md など）は `result.json` の `evidence` か、`ArtifactProduced` を発行する
明示的な手段で登録せよ」を足す。(b) 中期には `GET /tasks/{id}/artifacts` に「登録されていない
既知のファイル（`report.md`、`gate.json`、`verify.json`、`checkpoint.json`）をベストエフォートで
一覧に混ぜる」フォールバックを検討する（ADR や既存の規約に反しない範囲で）。

### 問題 3: reviewer run の `runs` 索引が不完全（finished_at/usage/end が欠落する経路がある）

dogfood の reviewer run #1（`01M3CDS6K0JPYT0GRDJ97Z986T`、13:58:51 開始）は、`execution.json`
の `runs` 配列で `finished_at: null`、`outcome: null`、`usage: null`、`end` キー自体が無いまま
残っている。この run は実際には（v2 への replan が起きているので）不合格の verdict まで完了して
いるはずで、単なる「まだ実行中」ではない。E2b/E3 の PROGRESS 節が「reviewer run にも
`run_index_start`/`run_index_finish` を配線した」と記録しているにもかかわらず、この 1 件だけ
索引が更新されていない（他の 2 件の reviewer run は正しく `finished_at`/`usage`/`outcome` を
持っている）。
**提案**: reviewer run が「不合格 → replan」という、通常の pass/fail 以外の第 3 の終わり方を
するときに、`run_index_finish` の呼び出し漏れが無いか（`on_review_finished` の replan 分岐）を
コードレビューで確認する。`celerisctl replay --check` を dogfood タスクに対して実行し、
`RUN_MISMATCH` が出るかどうかを実機で確かめるのが次の一手。

### 問題 4: 入力トークン 65.1M の内訳は「cache read」ではなく「codex の非キャッシュ入力」

dogfood の `total_input_tokens: 65,118,541` のうち 65,117,589（99.9%）は 8 件の `codex`
（`gpt-6-sol`）worker run によるもので、これらの `Usage` は `cache_read_tokens` フィールド
自体を持たない（`input_tokens`/`output_tokens` の 2 欄のみ）。Claude 系（planner・reviewer、
計 6 run）の `cache_read_tokens` 合計は 5,304,617 で、これは総入力トークンの 8% にすぎない。
「入力トークンの大半は cache read」という前提は、少なくとも今回の JSON からは確認できない
**（本レポートの依頼文にあった記述は、この点で実測と食い違う。訂正として記録する）**。真の理由は
2 つ考えられる: (a) codex アダプタが OpenAI 互換の prompt caching を利用・報告していない
（`RunMetrics`/`Usage` の型自体は cache フィールドを持つが、codex アダプタがこれを埋めていない
可能性。U2 の「codex/acp の turn ごとの usage は未確認」と同根）。(b) 8 WU が直列実行される
ため、各 WU の worker run が「そのつどゼロから」道具（ファイル読み込み等）を積み直しており、
実際に非キャッシュの入力が大きい。どちらであるかはコード（`crates/task-worker/src/codex.rs`）の
追加調査が必要（本レポートの範囲外。次の一手として記録）。
**提案**: codex アダプタが usage に `cache_read_tokens`/`cache_creation_tokens` 相当を報告できるか
確認し、できないなら「codex は現状キャッシュ非対応」という事実を `docs/llm-source.md` か
ADR-0072 の逸脱節に明記する。

### 問題 5: `cost_usd` が実質的な作業の大部分を除外している

`gpt-6-sol`（8 worker run すべて、総入力トークンの 99.9%）は `pricing.rs` の `PRICE_TABLE` で
4 欄とも `None`（`pricing.rs:70-77`）のため、`estimate_cost_usd` の合計 `cost_usd` から
まるごと除外される。dogfood の `cost_usd: $11.21` は実質的に **Claude 系 6 run（planner 3 +
reviewer 3 のうち usage を持つ 5 run）だけの費用**で、実装作業そのもの（8 worker run）の費用は
$0 として（正確には「計上されない」ものが見かけ上 $0 として）扱われている。これは ADR §7 の
U6・PROGRESS の P-118-1 の残課題（「GPT-6 3 モデルの単価は一次情報が揃うまで不明」）が
そのまま実害になった例。**単価が不明なことは仕方ないが、集計 API・GUI の表示が「不明」と
「$0」を区別していない**（`ExecutionMetrics.cost_usd` は単一の `f64` で、欠損の有無を示すフィールドが
無い）。
**提案**: `ExecutionMetrics` に `cost_usd_complete: bool`（単価不明のモデルを含む run が
1 件でもあれば `false`）を足し、GUI・報告で「$11.21（一部のモデルの単価が不明なため過小）」
のように表示する。

### 問題 6: planner run が 1 回あたり 10〜14 分かかる

3 回の planner run（13m46s、9m47s、3m20s）は、いずれも実装作業（各 WU 17〜45 分）と比べて
決して短くない。8 WU 中 3 回の合計 planner 時間は約 27 分で、壁時計 4h6m の約 11%。
**提案**: replan（v2、v3）の入力コンテキストを絞る余地がないか確認する
（`ExecutionPlannerContext.work_unit_summaries` は既に要約されているはずだが、実際の
プロンプト長を計測していない）。

### 問題 7: WU の直列実行が壁時計を押し上げている

D6 の設計どおり 1 Task 内の WU は直列実行（worktree 共有のため）。dogfood の 3 つの成果
（配送の局所修復・単価表・metrics 集計性能）は計画の依存グラフ上は互いに独立
（`delivery-repair-impl`/`pricing`/`metrics-store` はいずれも `depends_on: []`）だが、実行は
`delivery-repair-impl` → `delivery-repair-e2e` → `metrics-store` → `metrics-api` → `pricing` →
`finish` と完全に直列だった（11:06:05〜13:30:51 の 2h24m46s のうち、並列化できれば
理論上 `max(44m43s+19m20s, 17m33s+40m39s, 22m31s)` ≈ 64 分程度まで縮む可能性がある）。
これは ADR が明示的に「初期版は直列」と決めた設計（D6、U3）どおりであり不具合ではないが、
壁時計の観点では最大のボトルネックになっている。**提案**: U3（WU の並列実行、別 ADR）の
優先度を上げることを検討する。

### 問題 8: シャドウ計測が実質機能していない

§1「未達」・§5で詳述。

---

## 5. gate の既定を `on` にするかの提案

**提案: 現時点では既定を `on` にしない。`shadow` を維持し、実データを集めてから再検討する。**

根拠:

1. **サンプルが 1 件しかない**。`metrics-execution.json`（`since=2026-09-24`、46 タスク）を見ると
   `gate_mode=compound` は dogfood の 1 件だけで、しかも `source: human`（規則表を評価せずに
   強制した）。`gate_mode=none` が 45 件で、規則表が実際に compound/atomic を判定した記録が
   この期間に 1 件も確認できない。これでは D13 の重み（F1〜F5、S1〜S6）が現実のタスク分布に
   対してどの程度の精度・再現率を持つか、まったく検証できていない。
2. **今回の compound 判定は規則表を経由していない**。`execution_gate.rs:199-210` のとおり、
   `human_execution` が `Some` のときは `Scorer` を作らずに即座に返るため、dogfood の
   `gate.score: 0` は「このタスクが規則表で 0 点」を意味しない（§1）。つまり、
   「E6 の dogfood が compound と正しく判定された」という事実は、**規則表の性能について
   何も語らない**。
3. **decomposition 自体の効果は確認できたが、コストも見えている**。8 WU 全てが 1 run で完了し、
   replan も「done の WU を保持する」設計どおりに機能し、Task 全体としては done になった
   （§1・§3）。一方で、壁時計 4h6m のうち約 27 分（11%）が planner run、最終レビューの
   不合格 2 回がいずれも D16 ではカバーされない種類で `ReviewFail`（attempts 消費）+ 重い
   replan を経由した（§3・問題1）。「compound と判定して分解すれば必ず速く安く終わる」とは
   まだ言えない。
4. **費用の実態が見えていない**（問題5）。`cost_usd: $11.21` は実装作業の費用をほぼ含んでおらず、
   compound と atomic の費用比較ができる状態にない。

**段階案**（次にやること）:

- (a) `shadow` のまま最低 2〜4 週間、`ExecutionGated` の記録を溜める（人の明示に頼らない、
  規則表由来の判定を複数件集める）。
- (b) `GET /metrics/execution?group_by=rule_id` 相当（現状は `gate_mode`/`genre`/`assignee`/`lane`
  のみ。`rule_id` の追加を検討）で、どの規則がどれくらいの頻度で当たっているかを見る。
- (c) 問題1（review 不合格の分類の弱さ）と問題5（費用の欠損表示）を先に直してから、`on` への
  切り替えを検討する（さもないと `on` にした途端に「repair のはずが replan で attempts を消費する」
  「費用が見えない」という同じ問題が組織全体のタスクで頻発する）。
- (d) 全面的な `on` より先に、部署・genre 単位の段階導入（例: `software-engineering` の
  `implement` 系タスクだけ `on`）を検討する。CoS のヒント（`execution: compound`）は
  人が明示的に判断したときのバイパス経路として現状のまま残す。

---

## 6. 残る制限と backlog

### ADR §7 の未解決事項（U1〜U10）の現状

| # | 内容 | 現状 |
|---|---|---|
| U1 | context 超過の実機文言（claude-code/codex/ACP） | 未確認のまま。dogfood でも `budget_exhausted` は 1 件も発生しておらず検証機会が無かった |
| U2 | codex/acp の turn ごとの usage（peak context） | 未確認。dogfood でも `peak_context_tokens` は全 run で欠損（§2） |
| U3 | 1 Task 内の WU 並列 | 未着手。問題7で優先度を上げるよう提案 |
| U4 | WU ごとの WIP commit | 未着手（mechanical の `repo_state.uncommitted` のみ） |
| U5 | 部署をまたぐ WU | 「別の Task にする」の既定のまま、変更なし |
| U6 | `gpt-6-*` 系の単価 | 未解決のまま（`claude-fable-5-1` のみ E6 で解決。問題5） |
| U7 | idle timeout の分類 | 遷移未変更のまま |
| U8 | 上限到達の質問への回答のボタン化 | 未着手 |
| U9 | remote worktree の mechanical checkpoint | 対象外のまま |
| U10 | gate の閾値調整 | §5 のとおり、実質未検証 |

### 各 Phase の未解決事項（抜粋、ADR/PROGRESS からの持ち越し）

- **E1b（任意）**: wrap-up run、ACP の真の yield。未着手。
- **D16 の (h) 以外の未実装**: 配送の repair 自体は E6 で実装されたが、問題1で指摘した
  「review timeout」class は未実装。
- **`[execution.planner].permission_mode` の実行時配線**: E4b で実装済み（claude-code のみ、
  codex は対応するフックが無い）。
- **repair lane（cheap/standard）の強制**: budget だけ反映、lane は policy 任せのまま
  （Phase E4 の逸脱2）。
- **`ExecutionPlanVersionSummary` の差分件数（added/changed/removed）**: 未実装（Phase E5 の
  申し送り）。
- **`GET /metrics/execution` の `group_by=lane`**: WU 単位ではなくタスクの直近 run の lane を
  代表値にする簡略実装のまま。
- **Selective Lead Activation との統合**: D3 で「ADR-0069 §5 が予約した部門リードの選択的起動を
  この仕組みで実現する」としているが、実際に確認できたのは dogfood の
  `assignee=software-engineering` → planner が `software-engineering` department の
  profile で走った、という 1 例のみ（`routing.json` の `org_node: "software-engineering"`）。
  他部署・複数部署にまたがるケースの検証は未実施。
- **WU 並列**: U3 のまま（問題7で優先度提案）。
- **metrics-aware routing（過去の budget_exhausted 率を lane 決定に使う等）**: S6 が未配線
  （§1「未達」3）なので、この統合も手つかず。
- **shadow classifier（ML による gate 判定）**: R11 のとおり不採用のまま。データが
  溜まっていない（§5）ので当面その必要も無い。

---

## 7. migration / 互換の確認

- **schema**: `SCHEMA_VERSION = 26`（`crates/task-core/src/store.rs:83`）。dogfood の release
  `5cc1610938f5` の `verify.json` 相当（`releases.json` の該当 item）は
  `checks[0].detail: "health 200, schema_version=26, mode=verify, release=5cc1610938f5"`、
  `checks[4]（n-1-compat）.detail: "old celeris (a1194588b417) reads the migrated snapshot:
  schema_version=26, counts match ..."` で、**旧バイナリ（E6 より前の release）が schema 26 の
  DB を読めることを実機で確認済み**（migration 0026 は `CREATE TABLE IF NOT EXISTS` のみで
  既存の表に触れないという設計どおり）。
- **既存タスクの挙動**: 01M38J4X53P1Y684FS42Z6R0VZ（E1 より前に実行された atomic タスク）を
  `GET /tasks/{id}/execution` 相当で読むと、`runs` 配列自体は正しく 5 件表示される一方
  （`view.rs` 側の表示ロジックは events から直接組み立てるため後方互換）、
  `ExecutionMetrics.runs_by_role` は `{}`（空）になる。原因は `WorkerFinished.role`
  フィールドが Phase E3 で追加されたもので、それ以前の `WorkerFinished` イベントには存在せず
  （`serde(default)` で `None` に落ちる）、`execution_metrics::summarize` の `runs_by_role`
  集計がこれを数えられないため（**§2 注3 で述べた具体例**）。これは「壊れている」わけではない
  （古いイベントを読んでも panic やエラーにはならない）が、**E3 より前のタスクの
  `runs_by_role`／関連する metrics 集計は実態より少なく出る**という後方互換上の制約として
  記録しておく。
- **GUI の旧タスク**: `TaskDetail.execution: Option<ExecutionView>` は `serde(default)` で、
  Execution 節自体が無いタスクは `None`（Phase E5 のテスト
  `task_detail_execution_is_none_for_a_task_with_no_execution_activity` で確認済み）。今回の
  3 タスクのうち 01M38J4X53P1Y684FS42Z6R0VZ・01M39FGDAE9XQA3FGMCP5MCW0B は `has_plan: false`
  だが `gate`/`metrics` 自体は返る（`execution.json` に `metrics` キーが存在する）ため、
  「Execution 節が完全に非表示になる」わけではなく「計画が無い簡易表示になる」という設計どおり
  の挙動（D20「計画を持たない Task は『直接実行（Run n 回、continuation m 回）』の 1 行だけ」）
  になっていると考えられる（GUI 本体の実機確認はこのレポートの範囲外）。

---

## まとめ

ADR-0072 の 4 層モデル・決定的 scheduler・budget 切れの非失敗化・reviewer repair・replanning・
metrics・GUI は、E0〜E6 を通じてコードとして実装され、dogfood の 1 件では**実際に動いて
Task を done まで導いた**（before の巨大 session は failed で終わっている）。一方で、
(1) 実際に発火した repair・replan は D16/D17 が想定した「典型的な不合格」ではなく環境要因と
マージ衝突で、分類の弱さが露呈した、(2) gate の規則表は今回ほとんど検証されておらず
`on` への切り替えの根拠にはまだ足りない、(3) 費用・トークンの集計にモデル単価の欠損という
盲点がある、という 3 点が主な持ち越し課題である。
