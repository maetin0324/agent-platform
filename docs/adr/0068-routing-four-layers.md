# ADR-0068: routing を 4 層（Ownership / Harness / Model / Review）に分け、CoS から人選とモデル選択を外す（Phase 114、Model/Org routing 再設計 Phase 1）

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
- reasoning effort の CLI への受け渡し（D4）、`LowQuality` の検出源（reviewer の品質スコア）。
