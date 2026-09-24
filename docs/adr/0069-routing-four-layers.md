# ADR-0069: routing を 4 層（Ownership / Harness / Model / Review）に分け、CoS から人選とモデル選択を外す（Phase 114、Model/Org routing 再設計 Phase 1）

- 日付: 2026-09-24
- 状態: **Accepted**（人間の依頼「Model/Org routing redesign Phase 1」。CoS の責務を減らし、組織を継承の名前空間として
  扱い、モデル選択を決定的な policy と監査記録に置き換える。Phase 2 の項目は §9 に境界だけ予約する）
- 関連: ADR-0046（組織 = Agent Profile の継承木、D5 の決定的 matching）、ADR-0061（harness routing 基盤と
  Phase 104 のメトリクス）、ADR-0024 / ADR-0049（アカウント・プロバイダの残量による選択）、ADR-0048（Console の
  `actions`）、ADR-0010（リトライと供給側失敗）

## 1. 文脈

ADR-0046 D5 で「担当は決定的な matching が決める」と決めたが、実装には **LLM が決めた担当・tier がそのまま
タスクに入る抜け道**が 3 本残っていた（調査結果。Phase 114 で全て塞いだ）:

1. `task_ops::actions::create_task_action`（CoS の Console `actions`）が `ConsoleAction::CreateTask.assignee` /
   `tier` をそのまま `NewTaskSpec` に写していた。`assignee` が入ったタスクはディスパッチャの
   `assign_if_needed`（matching は `assignee == None` のときだけ走る）を素通りする。
2. `task_core::plan::materialize`（計画 run の `plan.json`）が `NewTask.assignee` を
   `resolve_child_defaults` 経由で子に記録していた（プロンプトは「書くな」と言うが、書けば通った）。
3. `task_core::delegate::materialize_delegated`（実行中の `delegate.json`）も同じ `resolve_child_defaults` を
   通って `DelegateTask.assignee` を子に記録していた（委譲のプロンプトは「組織図を見て assignee を書け」と
   指示していた）。

モデルについては、`tier` を LLM（CoS・計画・委譲）が「難易度の自己申告」として書き、ディスパッチャは
残量（`model_routing::select_tier`）で下げるだけだった。`ModelPrefs.allowed_tiers` は継承（交わり）まで
計算されていたが、どこでも強制されていなかった。

## 2. 決定: routing は 4 層

| 層 | 決めるもの | 決め方 | 入力 | 記録 |
|---|---|---|---|---|
| **Ownership** | TaskSpec → OrgNode（担当） | ADR-0046 D5 の matching（決定的）。人の明示だけが勝つ | harness / skills / tools / repos | `Event::Assigned` |
| **Harness** | 実行契約（`genre` = harness id）と adapter | タスクの harness → 役割・分野の既定（従来どおり） | harness / role | `Event::WorkerStarted.adapter` |
| **Model** | lane → provider / account / model_id / reasoning effort | lane は本 ADR の `ModelPolicy`（D3）。lane → model は既存の `model_routing` + `TieredAdapter`、残量は `select_tier` | TaskFeatures・組織の天井・残量 | `Event::RoutingDecided`（D6） |
| **Review** | 合否・やり直し・エスカレーション | 既存のレビュー（ADR-0007/0051）＋本 ADR の `EscalationPolicy`（D7） | 受け入れ条件・履歴 | `ReviewVerdict` / `RoutingDecided.record.escalation` |

層の間は**値だけ**を受け渡す（上の層は下の層の実装を知らない）。どの層も LLM を呼ばない（DESIGN 原則 1）。

### 組織木は「継承の名前空間」であって「命令の中継」ではない

組織ノードは knowledge / tools / policy / **budget** / **model の既定と天井** / **review policy** を子へ継承させる
名前空間（Kubernetes の Namespace + policy）である。上位ノードがタスクを「受け取って下へ回す」ことはしない
（CoS も同じ。CoS は対話・分解・報告の control plane で、担当とモデルを選ばない）。

## 3. 決定の詳細

### D1. CoS / 計画 / 委譲は担当とモデルを選ばない（人の明示だけが勝つ）

- **担当**: LLM が書いた `assignee` は**捨てる**。捨てた値はタスクの `routing.dropped_assignee` に残し、
  経路ごとに人が読める記録を出す（CoS: `ExecutedAction.summary` に「担当の指定 X は使わず、celeris が決定的に
  選ぶ」、計画: 計画 run の `fix_plan_for_harness` と同じ tracing の注記、委譲: run の進行 `note`）。
  担当は D5 の matching が決める。
- **人の明示**（出自＝provenance が人であること）だけは従う:
  - API `POST /tasks` / `celerisctl add --assignee`: `NewTaskSpec` の既定の出自は `SpecOrigin::Human`（`serde(skip)`
    なので API の JSON からは偽装できない。LLM 経路はコードが `SpecOrigin::Agent` を立てる）。
  - Console（人の発言 → CoS → `actions`）: **その対話タスクのきっかけになった人の発言本文に `@<node-id>` が
    書かれている**ときだけ、CoS の `assignee` をその値として採る（決定的な字句判定。CoS の自己申告は信じない）。
    `tier` も同様に、人の発言に `tier:<lane>` / `tier=<lane>` があるときだけ人の明示として扱う。
- **tier（lane）**: LLM が書いた `tier` は**ヒント**（`TierSource::Hint`）として記録するだけで、lane は D3 の
  policy が決める。人の明示（`TierSource::Human`）とコードが固定した値（`TierSource::System`: 計画 run の
  frontier 等）はそのまま使う。
- **CoS / 計画のプロンプト**: 「goal / harness / skills / mode / repos / constraints（features）を定義する。
  担当とモデルは選ばない」に書き換えた（`preamble.rs::actions_instructions`、
  `claude_code.rs::assignee_instructions_for_plan` / `assignee_instructions_for_delegation` /
  `delegation_instructions` / 計画の JSON 例）。
- スキーマの互換: `ConsoleAction::CreateTask.assignee/tier`、`NewTask.assignee/tier`、`DelegateTask.assignee/tier` は
  **フィールドとしては残す**（`deny_unknown_fields` の計画が落ちないため、既存の `plan-output.schema.json` を
  変えないため）。値の扱いだけが変わる。

### D2. 組織 = 継承の名前空間の拡張（budget / review policy / 天井の強制）

`Profile` に追加（全て `serde(default)`、空なら JSON に出ない。migration 不要。`org_nodes.profile_json` の中だけ）:

```toml
budget = { max_lane = "standard", max_attempts = 3 }   # 両方とも「最も厳しい値が勝つ」（根→葉の min）
review = { harness = "reviewer", tier = "cheap", escalate_on_fail = false }   # escalate_on_fail は子が勝つ
```

`EffectiveProfile` に `max_lane` / `max_attempts` / `review_escalate_on_fail` を足し、`lane_ceiling()` で
`LaneCeiling { allowed: allowed_tiers, max_lane }` を返す。**`allowed_tiers` はここで初めて強制される**:
policy が決めた lane が許可集合に無い／`max_lane` を超えるときは、許可された lane のうち**下側で最も近いもの**
（無ければ上側で最も近いもの）に丸め、`clamped_by` に理由を残す。resolve の規則:

| 項目 | 規則 |
|---|---|
| `model.allowed_tiers` | 交わり（従来どおり。空は制限なし） |
| `budget.max_lane` / `budget.max_attempts` | 最小（最も厳しい値が勝つ。天井なので子は緩められない） |
| `review.escalate_on_fail` | 子が勝つ |

人の明示 tier（`TierSource::Human`）は天井で丸めない（人の指示は組織の既定より強い。丸めない旨を記録する）。

### D3. `ModelPolicy`: TaskFeatures → lane（決定的な規則表。単一スコアにしない）

`crates/task-core/src/model_policy.rs`。`Tier` の直列化名（`frontier` / `standard` / `cheap`）は変えず、意味を
**品質／予算の lane** として読み替える（名前の変更はしない）。

- `TaskFeatures { judgment, ambiguity, verifiability, reversibility, consequence, context_size, tool_intensity,
  expected_length, cross_cutting }`、各軸 `Level::{Low, Medium, High}`。
- `TaskFeatures::infer(&Task)`: タスクの kind / harness / mode / category / 受け入れ条件の種類 / 目的の長さ /
  repos / skills / workspace（remote か）/ budget / priority から決定的に作る（LLM は呼ばない）。
- 明示の上書き: `Task.routing.features: Option<TaskFeatureHints>`（各軸 `Option<Level>`）。API の
  `NewTaskSpec.features` と CoS の `create_task.features` から入る（features は「仕事の性質の記述」であって
  モデルの選択ではないので CoS が書いてよい）。
- 規則（上から評価し、最初に当たったもの。`rule_id` を記録）:

| rule_id | 条件 | lane |
|---|---|---|
| `frontier/judgment-under-uncertainty` | judgment = High かつ（ambiguity = High または verifiability = Low） | frontier |
| `frontier/costly-and-unverifiable` | consequence = High かつ verifiability = Low | frontier |
| `frontier/broad-judgment` | judgment = High かつ cross_cutting = High | frontier |
| `cheap/mechanical-verifiable-reversible` | judgment = Low かつ ambiguity = Low かつ verifiability = High かつ reversibility = High かつ consequence ≠ High | cheap |
| `standard/default` | それ以外 | standard |

- 出力 `LaneDecision { lane, proposed, source, rule_id, policy_version = "lane-policy/1", features, reasons,
  clamped_by, hint, escalation, shadow }`。`reasons` は当たった条件の軸と値の文面。
- 優先順位: **人の明示 tier > policy**。policy の後に**別の層として**残量の `select_tier` が効く（従来どおり、
  下げるだけ）。
- 適用範囲: `kind = execute` かつ `Task.routing` を持つタスク（Phase 114 以降に `task_ops::add` / 計画 / 委譲で
  作られたもの）だけ。`routing` の無い既存タスク・対話・計画 run・合成 Review は従来どおり `worker_hint.tier`。

### D4. lane → provider / model / reasoning effort は別の層（既存の account/provider routing）

lane を決めた後の解決は従来の `select_provider`（ADR-0024/0049）→ `TieredAdapter::model_for_tier`
（`model_routing::resolve`）→ `select_tier`（残量）のまま。`ModelBinding` に任意の `reasoning_effort` を
足し（`serde(default)`）、`WorkerAdapter::reasoning_effort_for_tier` で読めるようにした。Phase 1 では
**記録するだけ**で CLI には渡していない（claude-code に相当の引数が無く、codex の `-c model_reasoning_effort`
の配線は Phase 2）。解決結果は `LaneResolution { lane, provider, account, adapter, model_id, reasoning_effort }`。

### D5. 監査記録: `Event::RoutingDecided`

dispatch ごと（`WorkerStarted` の直後、同じ `run_id`）に `Event::RoutingDecided { run_id, record: RoutingRecord }`
を 1 件残す（`routing` を持つ execute タスクだけ）。`RoutingRecord = { org_node, harness, decision: LaneDecision,
resolution: LaneResolution, quota_reason }`。状態は変えない（`replay` は無視）。

Phase 104 のメトリクス（`WorkerFinished.usage` の cost / tokens、`metrics.wall_ms` / `retries`）とレビュー結果
（`ReviewVerdict` と `Transitioned{review_pass|review_fail}`）を run 単位で結合する純粋関数
`task_core::routing_audit::routing_audit(&Task, &[Event]) -> Vec<RoutingAudit>` と、ストアから読む
`task_ops::routing_audit::task_routing_audit(store, task_id)` を置いた（GUI/API の表示は別 Phase）。

### D6. fallback / retry / escalation policy（`retry_policy.rs`）

- `EscalationPolicy { max_attempts_per_lane = 2, max_total_attempts = 4, escalate_on = [ReviewFailed,
  VerificationFailed, LowQuality], never_escalate_on = [SupplySide, BudgetExhausted], max_lane }`。
  `max_total_attempts` は `min(4, task.budget.max_retries + 1, profile.max_attempts)`（タスクの
  `max_retries` の意味を超えない。最終的に failed にするのは従来どおり状態機械）。
- `decide(history, base_lane, ceiling, budget) -> Retry{lane} | Escalate{from, to, reason} | Stop{reason}`。
  エスカレーションは 1 回に 1 段（cheap → standard → frontier）、天井を超えない、供給側失敗（`requeue`）・
  予算切れ（wall-clock / max_turns / budget の失敗）では上げない、`budget` が `Defer`/`Exhausted` なら止める。
- 履歴 `attempt_history(&Task, &[Event])` はイベントから決定的に作る（`RoutingDecided` の lane、
  `Transitioned` の reason、`ReviewVerdict` の失敗した条件の種類: command/artifact → VerificationFailed、
  reviewer/human → ReviewFailed、`reopen` で履歴をリセット）。
- 配線: ディスパッチャが `attempts > 0` のタスクを再 dispatch するとき、policy の lane に対して
  `EscalationPolicy` を当て、結果を `LaneDecision.escalation` に残す。`Stop` は「これ以上上げない」
  （lane は直前のまま）という意味で、タスクを失敗させるのは状態機械の `max_retries`。人の明示・System の
  tier は上げない。

### D7. 別の関心事との切り分け

本 ADR は**タスクの実行経路**（誰が・どの harness で・どの lane/model で・どうやり直すか）だけを扱う。
Knowledge GC（知識ベースのページの鮮度・統廃合）と Repository Docs Maintenance（リポジトリの `docs/` の
保守 run）の ADR とは**別の関心事**で、互いに依存しない（それらが作るタスクも、本 ADR の上では普通の
execute タスクとして同じ 4 層を通るだけ）。

## 4. 移行・互換

- DB migration なし（`SCHEMA_VERSION` は 25 のまま）。`Task.routing` は `tasks.json`（正本）の中だけ、
  `Profile` の追加項目は `profile_json` の中だけ。どちらも `serde(default)` で、無いものは従来どおり読める。
- `routing` の無い既存タスクは lane policy・エスカレーションの対象外（`worker_hint.tier` のまま）。
- 既存テストのうち旧い抜け道を前提にしたもの（計画・委譲の `assignee` が子に残る、Console の `assignee` が
  そのまま入る）は、新しい規則（捨てて `routing.dropped_assignee` に残る）の期待値に書き換えた。人の経路
  （`NewTaskSpec` 直接、API、`celerisctl add --assignee`）の `assignee` を検証するテストはそのまま通る。
  書き換えたもの: `task_core::plan::tests::materialize_drops_the_plan_supplied_assignee_and_records_it`、
  `plan_child_inherited_remote_workspace_stays_remote_because_the_plan_assignee_is_dropped`、
  `task_core::delegate::tests::materialize_drops_the_delegated_assignee_and_records_it`、
  `inherited_remote_workspace_stays_remote_because_the_delegated_assignee_is_dropped`、計画の
  harness 補正・調査系警告の 5 本（担当ではなく `genre` で harness を指定する形へ）、
  `task_ops::actions` の cluster 道具検証（人の発言に `@web-research` を入れる形へ）。
- **委譲の「部をまたぐ認可」（ADR-0033 D4 / SPEC §3.1）はこの経路では発火しなくなった**: 委譲が担当を
  名指ししない（捨てる）ので、`split_delegation` が見る `assignee` が常に空になる。dispatcher の 4 本の
  テスト（部またぎの質問・`once`/`standing`/`denied` の再実行・バッチ分割・同じ部）は、1 本
  （`a_delegation_naming_other_departments_creates_children_and_drops_the_assignees`）と同じ部の 1 本に
  まとめて新しい規則を確かめる形にした。matching が別の部のノードを選んだときの認可をどう扱うかは
  Phase 2 の「部門横断の調整」で決める（`split_delegation` と認可の仕組み自体は残してある）。
- ADR-0062 B2 の「継いだ Remote を担当の道具不足で Local に落とす」は、計画・委譲では担当が常に未定に
  なるので「担当未定なら Remote のまま（matching が `cluster:<id>` を持つノードだけを候補にする）」の側に
  倒れる。降格の関数と規則は残した（単体テスト `downgrade_rule_still_applies_when_an_assignee_is_known`）。
- API スキーマ（`api-v1.schema.json` / `event.schema.json`）と GUI の生成型は再生成した（追加のみ）。

## 5. Phase 2 に残すこと（境界だけ予約）

- **shadow mode の軽量分類器**（例: Jev）: `trait ShadowClassifier { fn classify(&Task, &TaskFeatures) ->
  Option<ShadowDecision> }` と `LaneDecision.shadow: Option<ShadowDecision { classifier, lane, confidence }>` を
  予約した（**実装なし**。heuristic と並べて記録し、lane は heuristic のまま）。
- **metrics-aware routing**: `routing_audit` の集計（lane × harness ごとの成功率・cost per success）を
  `ModelPolicy` の入力にする（ADR-0061 の `MetricsAwareRoutingPolicy` と同じ形）。
- **lead + sidekick**（Devin-Fusion 型）: 1 タスク = 1 lead model + 任意の sidekick。`LaneResolution` に
  sidekick を足す想定。
- **部門リードのセッションを選択的に起こす**: 複数サブタスク・複数 skill / repo / 環境・長時間・部門横断の調整・
  レビュー失敗のエスカレーションのときだけ（それ以外は命令の中継をしない）。
- `LowQuality` の検出源（reviewer の品質スコア）。

## Phase 118 追記（2026-09-24。「tier を実際に効かせる」。本番の `[[providers]]` に `tier_models` が
無く、lane に関わらず claude-pool が `claude-sonnet-5`、codex-pool が codex の既定モデルのままだった
不具合の是正。人の指示「tier を適切に運用できるように」）

実測（2026-09-24、CLI を実際に実行して確認。Claude Code / Codex、ChatGPT アカウント）。この値は
セッション中に 2 回、人の追加の実測で訂正されている（下の「本番の最終表」が最終値。他は経緯として
残す）:

- 最初の実測（Codex 0.156.1）: Claude Code は `claude-opus-5-5` / `claude-sonnet-5` /
  `claude-haiku-4-5-20251001` が成功。Codex は `gpt-5.6-terra` / `gpt-5.6-sol` / `gpt-5.6-luna` /
  `gpt-5.5` が成功、`gpt-5-codex` / `gpt-5` / `gpt-5-mini` / `gpt-5.5-codex` は
  「model is not supported when using Codex with a ChatGPT account」、`gpt-5.6-astra` / `gpt-5.6` /
  `gpt-5.4*` は「Model metadata for … not found」。
- 訂正 1（`codex exec -m <id>`）: Codex は **gpt-6 系**（`gpt-6-astra` / `gpt-6-sol` / `gpt-6-luna`）
  の 3 つとも成功。`gpt-5.6-*` は使わない。
- 訂正 2（`claude --model <id>`）: Claude Code は **claude-fable-5-1**（別名 `fable`）も成功。
  最終表は haiku を使わず frontier を 1 段引き上げる。

**本番の最終表**（人が `[[providers]]` に設定するのはこちら）: claude-pool は
frontier=`claude-fable-5-1` / standard=`claude-opus-5-5` / cheap=`claude-sonnet-5`（effort なし）、
codex-pool は frontier=`gpt-6-astra`(high) / standard=`gpt-6-sol`(medium) / cheap=`gpt-6-luna`(low)。

### D1. reasoning effort を CLI に渡す

- `trait WorkerAdapter`（`task-worker/src/adapter.rs`）に `supports_reasoning_effort() -> bool`
  （既定 `false`）と `with_reasoning_effort(&self, effort: &str) -> Option<Arc<dyn WorkerAdapter>>`
  （既定 `None`）を追加した。`with_model` と同じ「複製を返す」形。
- **codex**: `supports_reasoning_effort() = true`。`CodexConfig.reasoning_effort: Option<String>` を
  持ち、`run_codex_once` が `config.model` の直後（fresh/`exec resume` のどちらの形でも）に
  `-c model_reasoning_effort="<value>"` を足す。`exec resume` のホワイトリスト（Phase 68c）は
  `-c key=value` を任意個数許すので、resume でも同じ形で通る（Phase 112 の `-c` 翻訳と同じ理屈）。
- **claude-code**: `claude --help` に effort 相当のフラグ・環境変数が無い（このワークツリーは本物の
  `claude` を実行しないので、これは Phase 114 までの調査と本 Phase の人の実測メモに基づく判断であり、
  新たに CLI を実行して確認してはいない）。`supports_reasoning_effort()` は既定の `false` のまま
  （override しない）。設定で `reasoning_effort` を書いても CLI には**渡らない**（監査記録には残る。
  下記）。本番表は claude-pool に effort を設定しない運用なので、実害はない。
- **`TieredAdapter::run`**: `model_for_tier` の後、`reasoning_effort_for_tier(tier)` が `Some` かつ
  `base.with_reasoning_effort(...)` が `Some` を返すときだけ、その複製で実行する（対応しないアダプタ
  では黙って素通しし、`base` は変わらない）。`TieredAdapter::supports_reasoning_effort()` は
  `self.base.supports_reasoning_effort()` に委譲する。
- **監査記録**: `Event::RoutingDecided`（run の `WorkerStarted` の直後）の
  `RoutingRecord.resolution.reasoning_effort` は、**実際に CLI へ渡った値**を残す
  （`adapter.reasoning_effort_for_tier(tier).filter(|_| adapter.supports_reasoning_effort())`）。
  設定はあるが対応しないアダプタ（例: claude-code に effort を設定した場合）では `None` になる —
  「設定した」ことと「実際に渡した」ことを区別するため（`GET /tasks/{id}/routing` は
  `routing_audit` 経由でこの値をそのまま見せる。追加の配線は不要だった）。
- `ModelBinding.reasoning_effort` の doc コメント（Phase 114 の「Phase 1 では記録するだけ」）を
  「対応するアダプタ（codex）には実際に渡す」に更新した（スキーマの description のみ。型は不変）。

### D2. プロキシ既定表・example・docs を実測 ID に更新

- `llm-proxy::config::default_claude_models` / `default_gpt_models` を上記の実測 ID に変更した
  （`default_qwen_models` は変更なし。`qwen3.8-27b`）。
- `config/celeris.model-tiers.example.toml`: `unavailable_reason`（「実行モデルID未確認」）を外し、
  `model_id` に実測 ID を入れた。codex 側は `reasoning_effort`（high/medium/low）も添えた。
- `docs/llm-source.md` §1: 「既定値は未確認」の注記を実測済みの表に更新した。

### D3. 起動時の検証と可視化

- `model_routing::resolve` の現状（変更なし。動作を確認しただけ）: lane に対応する `ModelBinding` が
  無い、または `unavailable_reason` が付いているとき、`resolve` は `Err` を返す。呼び出し側
  （`dispatch_ready`）はこれを **`Trigger::Unroutable`**（タスクは `blocked` になり、人に聞く経路に
  乗る。ADR-0021）として扱う。**隣接 tier へ自動で倒すことはしない**（`select_tier` による残量調整は
  「解決できた lane」をさらに下げるだけの別の層であり、「解決できない lane」を別の tier で代替する
  機構ではない）。起動時（`Config::validate`）には `tier_models` の中身までは検査しない（`name` と
  `unavailable_reason`/`model_id` はどちらも自由記述であり、実行できるかは実行してみるまで分からない
  ため）。この設計は変えない。
- `celerisctl routing show [--config <path>]`（新規、読み取り専用）。`Config::load` するだけ（DB を
  開かない。`config to-harnesses` と同じ扱いで `main` がストアを開く前に分岐する）。2 つの表を出す:
  1. provider ごとの `tier → name / model_id（または unavailable の理由）/ reasoning_effort`
     （`[[providers]] tier_models`）。
  2. `[llm_proxy.models]` の `tier → claude/gpt/qwen ごとの model`。`[llm_proxy]` が無効でも表だけは
     出す（設定ファイル上の値をそのまま見せるだけで、到達性は見ない。到達性・cooldown は既存の
     `GET /llm/sources` の仕事）。
- GUI `/accounts`: 新しい API は足さない。`GET /providers` の `ProviderView.tier_models` に
  `reasoning_effort` を含む `ModelBinding` が既にあった（Phase 114）ので、`/accounts` の loader が
  `GET /providers` も読み、「プロバイダのモデル階層」節（読み取り専用の表。編集は従来どおり
  `/providers` 画面で行う）を追加した。

### D4. reviewer の lane

- `[reviewer] tier` を `Option<Tier>`（既定 `None` = 未設定）に変えた。`DispatchConfig` に
  `reviewer_tier_override: Option<Tier>`（`self.reviewer.tier` をそのまま運ぶ）を追加。既存の
  `DispatchConfig.reviewer_hint.tier`（`Option` ではない `Tier`。`self.reviewer.tier.unwrap_or(Standard)`）
  は「他に何も分からないときの既定値」として意味そのままに残した（後方互換。既存のテストが
  この値に依存している）。
- `Dispatcher::pick_reviewer` の lane 決定の優先順位（上ほど強い。ADR-0069 D2 の「組織の継承」を
  尊重しつつ、今回の既定を追加した）:
  1. **`profile.review_tier`**（部署の `[profile] review.tier`。既存 ADR-0069 D2、最も具体的な
     指定なのでそのまま最優先を維持）。
  2. **`[reviewer] tier` が明示されているとき**（`reviewer_tier_override.is_some()`）。
  3. **既定（Phase 118 で新設）**: そのタスクの worker run の lane（`task.worker_hint.tier`。直近の
     `decide_lane`/`select_tier` を経た値）と同じにし、部署の `lane_ceiling()`（`allowed_tiers` /
     `budget.max_lane`）で丸める。
  - 1./2. のときは丸めない（人・運用が明示した値は組織の既定より強い。D2 の「人の明示 tier は天井で
    丸めない」と同じ考え方を運用の明示にも適用した）。
- **監査記録**: reviewer run にも `Event::RoutingDecided`（`run_id` は review run の id）を 1 件足した
  （Phase 114 は worker run にしか出していなかった）。`RoutingRecord.decision` は規則表を通らない
  （reviewer は `TaskFeatures` 規則表の対象外）ので、`rule_id` は
  `reviewer/department-review-tier` / `reviewer/explicit-config` / `reviewer/matches-worker-lane` の
  いずれか、`source = TierSource::System`、`features` は監査の一貫性のため
  `TaskFeatures::infer(task)`（既存の純粋関数。LLM は呼ばない）をそのまま使う。`proposed` は
  worker lane、`clamped_by` は既定（3.）で天井に丸めたときだけ入る。`harness` は `Some("reviewer")`
  固定（レビュー run はタスクの `genre` を実行しない）。ストア書き込み失敗はレビューそのものを
  止めない（既存の `warned_unroutable` の通知と同じ「ベストエフォート」の扱い）。
- `Config::validate` の「reviewer を満たせるプロバイダが無い」チェックは、`[reviewer] tier` が
  明示されていればそのまま単一 tier で検査し、未設定なら「（`adapter` 制約を満たす）プロバイダが
  1 つ以上の tier を提供しているか」に緩めた（既定の lane はタスクごとに動的なので、特定の 1 tier に
  固定した検査は意味を持たない）。

### D5. テスト（ゲートは §末尾の完了報告を参照）

(a) codex argv（fresh・`exec resume` 双方）に tier ごとの `-m`/`--model` と
`-c model_reasoning_effort="…"` が乗ること。(b) claude-code argv に tier ごとの `--model` が乗り、
`reasoning_effort` を設定しても CLI 引数・環境変数には現れないこと。(c) `celerisctl routing show`
が一時 config から 2 つの表を出すこと（`tier_models` あり/なし、`unavailable_reason` あり、
`[llm_proxy.models]` の既定）。(d) reviewer lane: 既定（worker lane 一致・天井で丸め）、
`[reviewer] tier` 明示が既定に勝つこと、`profile.review_tier` が明示にも勝つこと、
`RoutingDecided` が review run にも出ること。(e) `default_claude_models`/`default_gpt_models` の
実測 ID への更新。
