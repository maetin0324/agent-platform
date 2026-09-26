# ADR-0074: WorkUnit の並列実行・工程ごとの途中確認・案件レベルの計画（マイルストーン Task の DAG）・quota を主にした費用指標

- 日付: 2026-09-26
- 状態: **Accepted**（Phase F0 = 設計。Phase F1（WU ごとの lane、planner の lane とサイズ、replan の差分、
  repair の分類、成果物の登録）着手・完了、2026-09-26。F2 以降は未着手）
- 関連:
  - ADR-0072（Task / ExecutionPlan / WorkUnit / Run。本 ADR はその D6 の直列規則・D13・D14・D16・D17・D18・D19・D21・D22 と §7 U3 / U4 を改める）
  - `docs/execution-decomposition-report-2026-09-25.md`（E6 dogfood の分析。以下「E6 報告」）
  - ADR-0069（routing の 4 層。lane は policy が決め、LLM は features だけ書く）
  - ADR-0053（LLM source。アカウントの残量スコア、`GET /llm/sources`）、ADR-0024 / 0025（アカウントプールと rate limit の観測）
  - ADR-0046（組織 = profile の継承木。budget は最も厳しい値が勝つ）
  - ADR-0043（worktree。`<workspace_root>/<task_id>/repos/<name>/`、ブランチ `celeris/<task_id>`）、ADR-0066（build cache の共有）
  - ADR-0033（案件・途中目標 `milestones`・報告）、ADR-0038（途中目標の判定 ok / 議論 / ng）、ADR-0044 D6（途中目標の一時停止・中止）
  - ADR-0016（委譲）、ADR-0067（成果物の置き場と未申告の成果物）、ADR-0070（失敗の可視化と通知）
- 人の決定: 2026-09-25（本 ADR の §1.2。以下「人の決定 1〜4」）

## 1. 文脈

### 1.1 E6 で分かった事実（E6 報告と本番の JSON コピーから。コードの位置は本 ADR の執筆時に確かめた）

| # | 事実 | 根拠 |
|---|---|---|
| E6-1 | dogfood の worker 8 run はすべて `standard/default`（gpt-6-sol）。どの frontier / cheap 規則にも当たらない | E6 報告 §2・§3。規則表は `crates/task-core/src/model_policy.rs:553-593` |
| E6-2 | planner は WU ごとの `features` を書かない。型としては既に `WorkUnitSpec.features: Option<serde_json::Value>` があり（`execution_plan.rs:118-120`）、`decide_for_work_unit` も読む（`model_policy.rs:728-745`）。しかし planner プロンプトの JSON 例に `features` が無く（`claude_code.rs:747-758`）、読めない値は `.ok()` で黙って捨てる。結果として WU の lane は Task の hints の焼き直しになる | 同左 |
| E6-3 | WU の view は `done_when` を `Check::Human` に写す（`model_policy.rs:684-693`）。`checks` の無い WU は verifiability が上がらず、機械的な `pricing` も cheap に落ちない | 同左 |
| E6-4 | planner は frontier 固定（`dispatcher.rs:6359`、`rule_id = planner/system-frontier`、`dispatcher.rs:6513-6530`）。3 run の出力は 44k / 37k / 14k トークン、時間は 13m46s / 9m47s / 3m20s（壁時計の 11%） | E6 報告 §3・問題 6 |
| E6-5 | replan は「新しい版の全体」を出させ、done の WU を**そのまま書き写す**ことを要求する（`claude_code.rs:844-850`）。出力が大きく、写し間違いは検証で拒否される | 同左 |
| E6-6 | codex は毎ターン全 context を再送する。入力 65.1M トークン（99.9% が codex の 8 run、cache の欄を持たない）。定価換算 11.21 USD は Claude 系 6 run だけの値で、しかも ChatGPT / Max は定額なので実支出ではない | E6 報告 問題 4・5 |
| E6-7 | 最終レビューの不合格 2 回（検査のタイムアウト、main の先行による merge-base のずれ）はどちらも D16 の 5 分類に当たらず、`ReviewFail`（attempts+1）+ replan を通った。replan が作った repair WU は class を復元できず `repairs_by_class: unknown` | E6 報告 問題 1、`execution_metrics.rs:79-81`、`execution.rs:716` |
| E6-8 | `GET /tasks/{id}/artifacts` が空。`artifact_views` は `ArtifactProduced` だけを見る（`crates/task-api/src/files.rs:246`）。ADR-0067 D3 の未申告成果物の走査は git worktree でない local の作業場所だけ（`undeclared_artifacts.rs:1-11`） | E6 報告 問題 2 |
| E6-9 | 3 つの成果は依存グラフ上独立（`depends_on: []`）なのに直列で、壁時計 4h06m。並列なら WU の区間は理論上 64 分程度 | E6 報告 問題 7 |
| E6-10 | reviewer run の `runs` 索引に `finished_at` / `usage` が欠ける経路が 1 件あった | E6 報告 問題 3 |

### 1.2 人の決定（2026-09-25）

1. **WU の並列実行**: 「WU ごとの worktree + 工程（phase）末尾の統合 WU」。計画に `phases` を持たせ、同じ工程内で依存の無い WU を並列に走らせる。
   並列数は Task ごとの `max_parallel_work_units`（既定 3）と provider の枠で抑える。統合 WU は scheduler が決定論的に `git merge` し、
   衝突は repair WU（LLM）へ。lease と running の鍵を (task, WU) にする。
2. **途中確認（phase 停止）**: 人が `pause_after`（工程）を指定したら、そこまで進んだところで Task を `blocked(awaiting_human)` にし、途中報告を
   受信箱に出す。操作は「続ける」「replan（指示つき）」「取り下げ」。通知は既存の失敗通知の経路。CoS が `pause_after` を含めることも許す。
   既定は全工程自動。
3. **案件レベルの計画**: 案件の途中目標 = 案件に直接紐づく top-level の「マイルストーン Task」。マイルストーン Task の DAG を CoS が起こし、
   人が承認する（HUMAN GATE の流儀）。案件の replan も同じ承認を通す。構造は 3 層に固定:
   案件 → マイルストーン Task（DAG、人の関門、報告の単位）→ ExecutionPlan → WU（平坦）→ Run。大きすぎる WU は planner が「子 Task への分割」を
   提案し、既存の委譲に乗せる。WU の中で再帰しない。
4. **費用の主指標は quota の消費**（定価 USD ではない）。アカウントの残量（ADR-0053）を metrics に載せ、Task / WU / run ごとに「どのアカウントの
   quota をどれだけ使ったか」を推定して記録する。定価 USD は参考値として残す（`cost_usd_complete` 付き）。

### 1.3 既にあるもの（設計の前提として確かめたこと）

- 並列の単位は今 Task だけ。`running: HashMap<TaskId, RunEntry>`（`dispatcher.rs:1514`、`RunEntry` は `dispatcher.rs:719`）、
  `dispatch_ready` は `running.contains_key(&task.id)` で同じ Task を二重に走らせない（`dispatcher.rs:6303`）。lease は `tasks` 行に 1 つで、
  `acquire_lease` が Ready → Running の遷移と同じトランザクションで取る（`store.rs:2891`）。
- WU の scheduler は `next_work_unit` が 1 つだけ返す（`execution_plan.rs:826-860`。`running` の WU があれば `Stuck`）。
- 並列度の会計は `workers_in_flight`（`dispatcher.rs:5713`）と `provider_in_use`（`dispatcher.rs:5722`）、クラスタの `concurrency`、
  アカウントの `max_concurrent_per_account`（`llm-proxy/src/selection.rs:22-28` と同じ規律を dispatcher も持つ）。
- worktree はタスクごと 1 つ（ADR-0043 D2）。build cache は同じリポジトリの worktree 間で `CARGO_TARGET_DIR=<build_cache_dir>/cargo/<repo-key>` を
  共有する（ADR-0066、`crates/celeris/src/config.rs:307-313`）。
- **途中目標は既にある**: `milestones`（ADR-0033 D2。`proposed / approved / in_progress / reached / redesigned / paused / cancelled`、
  `crates/task-core/src/org.rs:391-402, 475-492`）。判定は ADR-0038 の「秘書のまとめ → ok / 議論 / ng」（`task_ops::milestone_review`）、
  通知は `NotificationKind::MilestoneReady`（`celeris/src/notify.rs:255`）。ただし**直列の 1 本の鎖**（`ok` で次の `proposed` を 1 件 `approved` にする）で、
  DAG と前もっての全体計画は無い。分解は `POST /projects/{id}/plan` の `kind = plan` タスクが子を `draft` で作り、人が受け入れる
  （`crates/task-api/src/project_plan.rs`、受信箱の `DraftGroup`、`task-ops/src/inbox.rs:94-99`）。
- 残量の観測: `RateLimitObservation{five_hour, seven_day, observed_at}`（`task-core/src/accounts.rs:63-87`）を claude-code の
  `rate_limit_event` と codex の `token_count` / `account/rateLimits/read` から取り、`AccountBook` に置く。`GET /llm/sources` は
  `remaining`（厳しい方）/ `remaining_short` / `remaining_long` を返す（`llm-proxy/src/sources_view.rs:13-45, 174-207`。300 秒より古い観測は `null`）。
  `WorkerStarted.account` に run のアカウントが残る（`model.rs:889-892`）。
- 通知: 失敗は `scan_task_failed`（`notify.rs:609`）、`blocked` は `scan_question_blocked`（`notify.rs:340`。未決の認可が無い blocked を全部拾う）。
  受信箱の `AttentionItem`（`inbox.rs:103`）は `failed / requeue_limit_near / unroutable / cluster_unavailable`。
- CLAUDE.md の禁止に「予算管理の実装（別プロジェクト）」がある。**本 ADR の quota は観測と記録だけで、quota を理由に dispatch を止めたり
  選択を変えたりはしない**（D4、§5）。

---

## 2. 構造（3 層に固定）

```
Project（案件）
└─ マイルストーン Task（top-level。DAG = depends_on。人の関門・報告・途中目標の判定の単位）   ← D3
   ├─ 子 Task（任意。planner の分割提案 → 既存の委譲。見える別の deliverable）          ← D3.7
   └─ ExecutionPlan v1, v2 …（schema execution-plan/2 は phases を持つ）                 ← D1
      ├─ phase "build"
      │  ├─ WU a（worktree wu/a） ─┐  並列（max_parallel_work_units、provider の枠）
      │  ├─ WU b（worktree wu/b） ─┤
      │  └─ integrate-build（system WU。決定的な git merge + 検査。衝突は repair WU） ← D1.4
      ├─ phase "finish" …                         ← pause_after なら blocked(awaiting_human)  ← D2
      └─ Run（harness の 1 session。checkpoint / continuation は WU 単位のまま）
```

- WU は平坦（WU は計画を持たない。ADR-0072 D14 `max_planning_depth = 1` のまま）。phase は WU の**束**で、入れ子ではない。
- 子 Task は「別の deliverable」で、自分の ExecutionPlan を持てる（それは子 Task という Task の計画で、WU の再帰ではない）。子 Task の子は
  既存の委譲の深さ上限（ADR-0016 D2、深さ 5）に従う。**マイルストーン Task になるのは案件直下だけ**（D3.1）。

---

## 3. 決定

### D1. WU の並列実行（WU ごとの worktree、工程末尾の統合 WU、鍵 (task, WU)）

#### D1.1 schema `celeris.execution-plan/2`

```json
{
  "schema": "celeris.execution-plan/2",
  "rationale": "3 つの独立した成果を並列に作り、最後に仕上げる",          // ≤ 1,500 文字（D5.4）
  "phases": [
    {"key": "build",  "kind": "implement", "title": "3 成果の実装"},
    {"key": "verify", "kind": "test",      "title": "e2e と集計 API"},
    {"key": "finish", "kind": "release",   "title": "fmt・test・clippy・報告"}
  ],
  "work_units": [
    {"key": "delivery-impl", "phase": "build", "kind": "implement", "title": "…", "objective": "…",
     "depends_on": [], "done_when": ["…"], "checks": [{"cmd": "cargo test -p celeris delivery", "expect_exit": 0}],
     "features": {"judgment": "medium", "ambiguity": "low", "verifiability": "high", "reversibility": "high", "consequence": "medium"},
     "context": {"paths": ["crates/celeris/src/delivery.rs"]}, "budget": {"max_turns": 40}, "outputs": []},
    {"key": "delivery-e2e", "phase": "build", "depends_on": ["delivery-impl"], "…": "…"},
    {"key": "pricing", "phase": "build", "depends_on": [], "…": "…"},
    {"key": "metrics-api", "phase": "verify", "depends_on": ["metrics-store"], "…": "…"}
  ],
  "children": []                                                           // D3.7（F4 まで空のみ許す）
}
```

- `phases`: 1..=`max_phases`（既定 5）。`key` は `[a-z0-9-]{1,32}`、`kind` は WU と同じ語彙（investigate / design / implement / test / release /
  repair / other）。配列の順が実行順。**`pause_after` は計画には書かない**（誰が書けるかは D2.1。planner は書けない）。
- 各 WU は `phase`（必須）を持つ。`depends_on` の規則（`validate` で拒否。決定的）:
  1. 依存先は**同じ工程か前の工程**の WU。後の工程は拒否。
  2. 同じ工程の中の依存は **高々 1 つ**（鎖か木）。同じ工程で 2 つ以上に依存するなら、後の工程に置く（D1.3 の「積み上げ」の都合）。
  3. 前の工程への依存はいくつでもよい（工程の境で統合済みなので、Task ブランチに入っている）。
- `features` は `TaskFeatureHints` の形で**型として**検証する（D5.1）。`assignee` / `tier` / `model` / `lane` は持たない（`deny_unknown_fields`）。
- **後方互換**: `celeris.execution-plan/1` はそのまま受け付け、「1 工程・並列 1・WU 用 worktree なし（Task の worktree を共有）」として
  走らせる（ADR-0072 の E2〜E6 の挙動と 1 バイトも変えない）。DB に保存済みの v1 計画も同じ。planner に v2 を出させるのは
  `[execution] parallel = true` のときだけ（F2 の既定は `false`、F5 の結果で人が切り替える）。
- `ExecutionPlanSpec` は `schema` の値で v1 / v2 を分ける 1 つの型にする（`phases: Vec<PhaseSpec>` は `serde(default)`。v1 で `phases` や
  WU の `phase` があれば拒否、v2 で無ければ拒否）。`docs/protocol/execution-plan.schema.json` は v2 を含む形に再生成する（既存の
  schema 一致テストを保つ）。

#### D1.2 WU ごとの worktree（ADR-0043 の流儀）

- 置き場: `<workspace_root>/<task_id>/wu/<key>/repos/<name>/`（git の repo ごとの worktree）と `<workspace_root>/<task_id>/wu/<key>/artifacts/`
  （その WU の `checkpoint.json`・`result.json` など。並列の WU が同じ `artifacts/checkpoint.json` を上書きしないため）。`runs/` は Task の
  ものを共有する（`run_id` は一意）。
- ブランチ: **`celeris-wu/<task_id>/<key>`**。人の決定の `wu/<key>` を Task の名前空間に入れた形。`celeris/<task_id>/wu/<key>` にしないのは、
  git の ref が `refs/heads/celeris/<task_id>`（ファイル）と `refs/heads/celeris/<task_id>/…`（ディレクトリ）を同時に持てないため
  （D/F 衝突）。
- 基点: 工程の最初の WU は「その工程が始まった時点の Task ブランチの HEAD」（前の工程の統合の結果）。同じ工程の依存を持つ WU は
  **その依存先の WU ブランチの HEAD**から切る（積み上げ。依存先が done になってから作る）。基点の sha を `work_units.base_commit` に残す。
- WU の完了時の commit（ADR-0072 §7 U4 を v2 で解消）: WU の run が done になったら、daemon が WU の worktree で
  `git add -A && git commit -m "wu/<key>: <title>"`（作者は celeris の固定値、未変更なら何もしない）を決定的に行い、HEAD を
  `work_units.head_commit` と `Event::WorkUnitCommitted` に残す。ワーカーのプロンプトにも「自分のブランチに commit してよい。push はしない」と書く。
- build cache: WU の worktree も ADR-0066 の共有 `CARGO_TARGET_DIR` を使う（依存 crate は再利用、同時ビルドは cargo のロックで直列になる。
  正しさは変わらない）。レビューと統合の検査は Task の worktree で走るので、E6-1 の「別 target で cold compile」は起きにくくなる。
- **並列 1 に倒す条件**（決定的。`work_units` の行に理由を残し、GUI に出す）: Task の workspace が remote（ADR-0072 §7 U9 と同じく
  mechanical checkpoint と worktree の複製が無い）、書き込み可能な `dir`（git でない、シンボリックリンクで共有）の repo を持つ、
  `workspace_mode = Shared`。このとき v2 の計画でも工程の中は直列で走り、WU は Task の worktree を共有する（統合 WU は no-op）。
- 後片付け: WU の worktree は統合が済んだら消す（ブランチは Task の終端まで残す。監査と差分の確認のため）。Task の中止・取り込みでは
  ADR-0043 D2 の規則で Task の worktree と一緒に消す。

#### D1.3 scheduler: 工程の中の並列

- `next_work_unit(&[WorkUnitRow]) -> NextStep` を `runnable_work_units(&[WorkUnitRow], in_flight: usize, limit: usize) -> Vec<id>` に一般化する
  （純粋関数）。選び方:
  - 対象は**現在の工程**（統合が済んでいない最初の工程）の WU だけ。
  - `needs_continuation` を先に、次に `ready` を `seq` 順。
  - `limit - in_flight` 件まで。v1 と「並列 1 に倒す条件」では `limit = 1`（今と同じ）。
  - 工程の中で `failed` / `blocked` の WU があれば、**新しい WU は起こさない**（走っている WU とその continuation だけは続ける）。
- `pending → ready` の依存解決は今と同じ（`newly_ready`）。ただし「前の工程の統合が done」を追加の条件にする（工程の境は障壁）。
- 並列数の上限（上から順にすべて効く。最も厳しいものが勝つ）:
  1. `max_parallel_work_units`: Task ごと。既定は `[execution] max_parallel_work_units = 3`、上限 6。`NewTaskSpec.execution.max_parallel_work_units`
     （人・CoS）で狭められる（広げるのは人だけ）。組織の profile の `budget.max_parallel_work_units`（ADR-0046 / ADR-0069 D2 の規則。最も厳しい値）
     でも狭められる。
  2. 既存の全体の `max_concurrency`、provider の `concurrency`（`select_provider` の `full`）、アカウントの `max_concurrent_per_account`、
     クラスタの `concurrency`。WU の run は 1 本ずつ `select_provider` を通るので、同じ Task の並列 WU は別のアカウントに散りうる
     （`in_use` の少ない順。ADR-0024 D3-4）。
  3. **公平性**: `dispatch_ready` は、まず従来どおり `ready_tasks` を順位順に見て各 Task の**1 本目**を起こし、その後で残りの枠を
     「Running で、現在の工程に runnable な WU を持つ Task」（新設の `store.running_tasks_with_runnable_work_units(limit)`）の 2 本目以降に
     回す（作成順）。並列の WU が Ready の別の Task の枠を奪わない。

#### D1.4 統合 WU（工程末尾。決定的な merge、衝突は repair WU、検査の再実行）

- 計画の採用時に、v2 の工程ごとに **system WU** `integrate-<phase>`（`kind = integrate`、`runs = 0`、LLM run を起こさない）を daemon が足す。
  依存は「その工程のすべての WU」。`max_work_units` には数えない。GUI の WU の表に出る（何が起きたかを見せるため）。
- 工程のすべての WU が `done` になったら、scheduler は統合を**spawn したタスクの中で**実行する（git の I/O は tick を止めない。E4 (g) の
  `spawn_work_unit_checks` → `on_work_unit_checks_finished` と同じ非同期の形）。Task は Running のまま（lease は D1.5 の工程の lease）。
  1. Task の worktree（ブランチ `celeris/<task_id>`）で、工程の**葉の WU**（同じ工程の他の WU に依存されていない WU）を `seq` 順に
     `git merge --no-ff --no-edit celeris-wu/<task_id>/<key>`（メッセージは固定 `integrate wu/<key> (phase <p>)`）。積み上げた依存先は葉に含まれる。
  2. 既に入っている WU（`git merge-base --is-ancestor`）は飛ばす（再起動後のやり直しを冪等にする）。`MERGE_HEAD` が残っていれば
     `git merge --abort` してからやり直す。
  3. **衝突**したら `git merge --abort`。その WU で止め、repair WU `merge-<phase>-<key>`（`kind = repair`、class `merge_conflict`、
     Task の worktree で走る、D16 の最小 context: 衝突したファイルの一覧・両側の WU の objective と最終 checkpoint の `decisions`・
     `git diff --stat`、目的は「`celeris-wu/…` を merge し衝突だけを解消せよ。設計は変えるな」）を作る。repair が done になったら
     統合を続きから再開する（2 の冪等性で、済んだ merge は飛ばす）。
  4. すべて入ったら**検査の再実行**: その工程の WU の `checks`（重複を除く）と、`.config/celeris/workspace.toml` の check（ADR-0043 D4）を
     Task の worktree で順に実行する（`review::run_work_unit_checks` を再利用、`command_timeout` も同じ）。失敗したら D16 の分類
     （`classify_review_failure`。format / lint / test_small / review_timeout）で repair WU を Task の worktree に作る。分類に当たらなければ
     replan（D17。起こした理由は「phase <p> の統合後の検査が失敗」）。
  5. 成功したら `Event::PhaseIntegrated{phase, merged: [{key, commit}], head, checks: [{cmd, exit, summary}]}` を残し、`integrate-<phase>` を
     `done` にし、WU の worktree を消す（D1.2）。次の工程の WU が `ready` になる。
- 上限: 工程ごとの merge の repair は 2 回まで、統合後の検査の repair は D16 の `max_repairs`（Task ごと）に数える。超えたら replan、replan の
  上限なら blocked（D12 の「失敗にしないもの」）。
- 統合は daemon の中の git 操作だけで、LLM を呼ばない（DESIGN 原則 1）。ADR-0072 D4 の「daemon がやること」に「工程の統合（決定的な merge と
  検査）」を足す。

#### D1.5 running / lease の鍵を (task, WU) にする

- `running: HashMap<RunKey, RunEntry>`、`RunKey { task: TaskId, work_unit: Option<String> }`（`None` = atomic・planner・v1 の WU）。
  `run_id → RunKey` の索引を持つ。`Completion` は今どおり `run_id` を運ぶので、終わった run の鍵はこの索引で引く。
  `workers_in_flight` / `provider_in_use` / `cluster_in_use` / アカウントの `in_use` は `values()` を数えるだけなので意味は変わらない。
  Task 単位の問い（「この Task は走っているか」）は `running_for_task(task_id)`（その Task の鍵を持つ entry の集合）にする。
  `reviewing: HashMap<TaskId, ReviewEntry>` は変えない（最終レビューは Task の単位）。
- **WU の lease**: `work_units.lease_run_id` / `lease_expires_at`（migration 0027）。`store.acquire_work_unit_lease(task_id, wu_id, run_id, ttl)` が
  1 トランザクションで「Task が Running」「WU が ready / needs_continuation」を確かめ、WU を running にし（`WorkUnitTransitioned{to: running, run_id}`）、
  WU の lease を取り、**Task の lease の期限を WU の lease の最大値まで延ばす**。
- **Task の lease（工程の lease）**: v2 の Task では、Task の lease の保持者を run ではなく `phase:<plan_id>:<phase>:<ulid>` にする。
  - 工程の最初の WU を起こすときは、従来の `acquire_lease`（Ready → Running、`Trigger::Dispatch`）を工程の保持者で取り、続けて
    `acquire_work_unit_lease`。
  - 2 本目以降は `acquire_work_unit_lease` だけ（Task は既に Running）。
  - 統合の間も工程の保持者のまま、統合の wall の上限で延ばす。
  - 期限は常に「生きている WU の lease と統合の期限の最大値」なので、**Task の lease が切れるのは全部が死んだとき（デーモンの停止）だけ**。
    そのとき既存の `reclaim_expired_leases` → `InfraRequeue` がそのまま効く。
- WU の run の結果が古いか（lease を失った run の結果を捨てる判定）は、Task の lease ではなく **WU の `lease_run_id`** と比べる。
- v1 と atomic の Task は今と同じ（Task の lease の保持者は run_id、WU の lease 列は使わない）。

#### D1.6 Task の遷移（並列のとき）

| 起きたこと | 他に走っている WU / 統合 | Task の遷移 | WU の遷移 |
|---|---|---|---|
| WU の run が done / budget_exhausted / yielded / failed(retry 可) | ある | **なし**（Running のまま） | ADR-0072 D6 の表のとおり |
| 同上 | ない、現在の工程に runnable な WU がある | `Continue{advance}`（Running → Ready。今と同じ） | 同上 |
| 同上 | ない、工程の WU がすべて done | なし（統合を始める） | `integrate-<phase>` を running |
| 統合が done | — | 次の工程があれば `Continue{advance}`。`pause_after` に当たれば D2 の `PhaseGate`。最後の工程なら `WorkerDone`（→ Reviewing） | — |
| WU の question | ある | なし | blocked（question）。この工程で新しい WU は起こさない |
| 同上 | ない | `WorkerQuestion`（→ Blocked。今と同じ） | — |
| WU が failed（retry の上限）・進捗なし・`plan_issue` | ある | なし。**兄弟は止めない**（完走させる。途中の成果は checkpoint とブランチに残る） | failed / blocked |
| 同上 | ない | 今と同じ（replan できれば `Continue{replan}`、できなければ D12） | — |
| Task の Cancel / 取り下げ | — | `Cancel` | 走っている WU の run をすべて止め（`stop_run` を鍵ごと）、未完了の WU を `cancelled`（ADR-0072 E2 で未実装だった D6 のカスケードをここで入れる） |

- replan は**工程の中の in-flight が 0 になってから**起こす（走っている WU の結果を捨てないため）。replan は done の WU と統合済みの工程を不変で持ち越す。
  未統合の工程の done の WU は「done（未統合）」として持ち越し、新しい版の同じ key の WU として統合される。
- continuation / checkpoint は WU 単位のまま（ADR-0072 D8 / D9）。checkpoint の mechanical な欄（`repo_state`・`files_changed`）は WU の worktree で、
  base を WU の `base_commit` として取る。依存する WU への引き継ぎ（`dependency_checkpoints`）も今のまま。

#### D1.7 再起動後の照合

tick の最初に、v2 の Task について次を決定的に行う（`WorkUnitTransitioned{reason: "restart_reconcile"}`）:

- WU が running で、WU の lease が切れている、または `lease_run_id` の run が `running` に無い → lease を外し、checkpoint があれば
  needs_continuation、無ければ ready。WU の worktree はそのまま（続きから再開できる）。
- `integrate-<phase>` が running で、統合を走らせている spawn が無い → pending に戻す。次の tick で D1.4 の 2 の冪等な手順でやり直す。
- Task が Running で、どの WU も running でなく、統合も走っていない → `Continue{advance}`（Ready に戻して通常の経路に乗せる）。
- ADR-0072 E2 の逸脱節が挙げた「`abort_stale_runs` / Cancel / Interrupt の後に WU が running のまま残る」穴も、この照合で塞ぐ。

#### D1.8 reviewer との関係

- **最終レビューは Task の単位で 1 回**（今のまま）。工程ごとに LLM の reviewer は起こさない。工程の品質は統合後の決定的な検査（D1.4 の 4）で見る。
- 理由: reviewer run は 1 回 2〜14 分・frontier / standard の費用がかかり（E6 報告 §3）、工程の数だけ増やすと並列で縮めた壁時計を食う。
  人が工程ごとに見たいときは D2 の途中確認を使う（人の判断は reviewer より強い）。

### D2. 途中確認（工程の後で止まる）

#### D2.1 `pause_after` を誰がどこに書くか

- `NewTaskSpec.execution.pause_after: PausePolicy`（`serde(default)`、既定 `none`）:
  - `{"mode": "none"}`（既定。全工程自動）
  - `{"mode": "each_phase"}`（最後の工程を除くすべての工程の後）
  - `{"mode": "after", "phases": ["design", "build"]}`（工程の `key` か `kind` に一致する工程の後。計画の前に書けるよう `kind` でも当てる）
- 書ける者: **人**（API・GUI の「工程ごとに確認する」、または計画採用後の `PUT /tasks/{id}/execution/pause-after`）と **CoS**（`create_task` の
  `execution.pause_after`、および D3 の案件計画のマイルストーンの `pause_after`）。出自は `SpecOrigin`（ADR-0069 D1）で記録する。
  CoS の値も採用する（止まる方向は安全側で、担当やモデルの選択ではないため。ADR-0069 D1 が LLM の `assignee` / `tier` を捨てるのとは扱いを
  分ける）。**planner は書けない**（execution-plan の schema に欄が無い）。
- 採用時の解決: 計画（v2）の採用のときに `PausePolicy` を工程の key の集合に解決し、`ExecutionPlanned` と同じトランザクションで
  `Event::PausePointsResolved{plan_id, phases: [key], source}` を残す。replan の採用のたびに解決し直す（済んだ工程の分は変えない）。
- v1 の計画と atomic の Task では「工程」が 1 つなので、`pause_after` は効かない（最後の工程の後は最終レビューがある）。受け付けたうえで
  `PausePointsResolved{phases: []}` を残し、GUI に「この Task は工程を持たないため途中確認はありません」と出す。

#### D2.2 遷移: `blocked(awaiting_human)`

- 新しい `Trigger::PhaseGate { phase: String }`: Running → Blocked、`Transitioned.reason = "awaiting_human"`。attempts を変えない
  （`replay` の attempts の集合に入らない）。統合 WU が done になり、その工程が停止点なら、次の工程の WU を `ready` にする**前に**適用する
  （次の工程は pending のまま）。
- 新しい `Trigger::PhaseResume { mode: Continue | Replan }`: Blocked → Ready、reason `phase_continue` / `phase_replan`。attempts を変えない。
- `Status` は増やさない（ADR-0072 R3 と同じ理由）。`ExecutionPhase`（表示の導出値）に `awaiting_human` を足す（Blocked で、直前の遷移の
  reason が `awaiting_human`）。
- `Trigger::Answer` は awaiting_human の Task には 409 で拒否する（「続ける」と「質問への回答」を混ぜない。GUI は 3 つのボタンだけを出す）。

#### D2.3 途中報告（決定的に作る。LLM は使わない）

`Event::PhaseReported{phase, report: PhaseReport}`（16 KiB 上限、ADR-0072 D8 と同じ決定的な切り詰め）と、同じ内容の
`artifacts/phase-reports/<n>-<phase>.md`（`ArtifactProduced`、GUI で読める。ADR-0067）:

- 済んだ工程の一覧（工程ごとに 1 行: WU の数・run の数・壁時計）
- この工程の WU ごとの要約: title、最終 checkpoint の `completed`（上位 5 件）と `decisions`、`known_failures`
- 統合の結果: merge した WU と commit、衝突と repair、検査の cmd / exit / summary（`PhaseIntegrated` から）
- Task ブランチの差分: `git diff --stat <base>..HEAD` の要約（最大 30 行）
- 次の工程: key / title / WU の title の一覧
- 使った quota と参考の定価（D4。ここまでの合計）
- 成果物へのリンク（その工程の run の `ArtifactProduced`）

SPEC §3.5「上に行くほど圧縮」の 1 段目として、ワーカーの checkpoint を機械的に束ねたものにする。CoS の要約は付けない（必要なら
D3 のマイルストーンの判定で CoS がまとめる）。

#### D2.4 受信箱と操作

- 受信箱: `AttentionItem::PhaseCheckpoint { task, phase, phase_title, phases_done, phases_total, report_idx, next_phase, at }`（新種別）。
  `questions` には出さない（`QuestionRaised` も approvals の行も作らない。これは認可 SPEC §3.6 ではなく進捗の確認）。
- 操作（管理系 = 人だけ。`require_admin`）: `POST /tasks/{id}/execution/phase-gate`、本文 `{"action": "continue" | "replan" | "withdraw", "note": "…"}`
  - `continue`: `PhaseResume{Continue}`。`note` があれば次の工程の WU の run に「人の指示」として渡す（`answers` の節と同じ形）。
  - `replan`: `note` 必須（空なら 422）。`PhaseResume{Replan}` の後、次の run を planner（replan、D5.3 の差分出力）にする。起こした理由 =
    「人の指示: <note>」。`max_replans` に数える。
  - `withdraw`: 既存の `Cancel`（`Transitioned.reason = "withdrawn"`）。後片付けは ADR-0043 D2 の中止のまま。部分成果を残したいときは
    取り下げる前に intake（merge / pr）する、と GUI に書く。
  - Task が awaiting_human でなければ 409。
- 通知: 失敗通知と同じ経路（`celeris::notify` の scan。`scan_task_failed` と同じ形の `scan_phase_checkpoint`）で `NotificationKind::PhaseCheckpoint`、
  `key = task_id:遷移番号`。本文は「『<Task>』が工程『<phase>』まで進みました。続ける / replan / 取り下げ → <link>」。
  `scan_question_blocked` は reason が `awaiting_human` の Blocked を**除外**する（二重に鳴らさない）。
- 待つ間: 期限を設けない（人の判断を待つ）。WU の worktree と Task の worktree は残る。quota は消費しない。

### D3. 案件レベルの計画（マイルストーン Task の DAG）

#### D3.1 マイルストーン Task の定義: **案件直下という位置で決める**

`task_core::is_milestone_task(&Task) -> bool` =
`project_id.is_some() && parent_id.is_none() && kind == Execute && conversation.is_none() && support_kind.is_none()`。
`milestone: true` のようなフラグは持たない。理由:

1. **構造と一致して崩れない**: 3 層を固定したので、マイルストーンは「案件の直下」以外にありえない。フラグだと、入れ子の Task に
   `milestone: true` が立つ・直下の Task に立たない、という食い違いが作れてしまい、関門・報告・判定の単位が曖昧になる。
2. **追加の列が要らない**: `tasks.project_id` / `parent_id`（ADR-0033 D2）で決まる。仕事の木（SPEC §3.3、GUI）の最上段がそのままマイルストーンの DAG になる。
3. **委譲の子は自動的にマイルストーンにならない**: 子 Task は `parent_id` を持つので、planner の分割提案（D3.7）で子が増えても
   案件の DAG は増えない（人が見る単位が増えない。ADR-0072 D2 と同じ考え方）。
4. **報告の圧縮の単位になる**: 子 → マイルストーン → 案件の順に上がる（SPEC §3.5）。

欠点と対処: 案件直下には今、対話（`conversation`）・`kind = plan` の分解タスク・support-task もいる。上の述語の除外でそれらを外す。
既存の案件の「`kind = plan` の子」（`parent_id` = plan タスク）はマイルストーンにならない（旧い案件は旧い見え方のまま。D3.8）。

**既存の `milestones`（途中目標）との関係**: マイルストーン Task と `milestones` の行を **1:1** にする。`milestones` は「人の判定の台帳」
（proposed / approved / in_progress / reached / redesigned / paused / cancelled、ADR-0038 の ok / 議論 / ng、ADR-0044 D6 の一時停止・中止）として残し、
マイルストーン Task は「その途中目標を実行する Task」。`tasks.milestone_id` が両者を結ぶ（子 Task は今どおり親の `milestone_id` を継ぐ。
ADR-0033 D2 監査 D-3）。新しい案件計画（D3.3）はマイルストーンごとに行と Task を 1 つずつ作る。

#### D3.2 DAG の依存: 既存の `depends_on` を使う

- マイルストーン Task 同士の依存は `tasks.depends_on`（`ready_tasks` が「依存がすべて Done」を見る。変えない）。
- 追加の規則（**途中目標の Go**）: 依存先のマイルストーンが `done` でも、その `milestones.status` が `reached` になるまで、依存する
  マイルストーン Task は dispatch しない（SPEC §7「いい感じだったら承認し次の途中目標までの Go サインを出す」）。判定は ADR-0044 D6 の
  「paused の途中目標に属するタスクは dispatch しない」と同じ場所に置く（決定的、LLM なし）。
- 依存の無いマイルストーン（agent-platform の自己改善に多い）は、承認されたら同時に進む（Task をまたぐ並列は従来どおり `max_concurrency`）。
  直列が多い案件（BenchFS）では、前のマイルストーンの reached が次の Go になる。
- 案件ごとの設定 `projects.auto_advance`（F4 で `Project` の JSON に `serde(default)`、既定 `false`）が `true` なら、依存先の Task が
  `done` になった時点で `reached` を待たずに進める（判定のレビューは後から行う）。

#### D3.3 CoS が起こす計画: schema `celeris.project-plan/1`

入口は既存の `POST /projects/{id}/plan`（`kind = plan`、担当 = 秘書）に `mode: "milestones"` を足したもの。計画の run は
`<artifacts>/project-plan.json` を書く:

```json
{
  "schema": "celeris.project-plan/1",
  "rationale": "…",                                        // ≤ 2,000 文字
  "milestones": [
    {"key": "survey", "title": "関連研究の調査", "objective": "…",
     "reach_criteria": "何が示せたら途中目標の達成か（人の判定の材料。SPEC §7「検証の合格線」）",
     "acceptance": [{"text": "…", "check": {"type": "human"}}],
     "depends_on": [], "genre": "literature", "skills": ["…"], "repos": ["pluvio"],
     "features": {"judgment": "high"}, "execution": "atomic",
     "pause_after": {"mode": "none"}},
    {"key": "poc", "depends_on": ["survey"], "…": "…"}
  ]
}
```

- `deny_unknown_fields`。`assignee` / `tier` / `model` は持たない（ADR-0069 D1。担当は matching が決める）。`execution` は ADR-0072 D13 のヒント（+2）。
- 検証（純粋関数 `task_core::project_plan::validate`）: 1..=12 件、key は一意、`depends_on` は既知の key で循環なし（トポロジカル順）、
  既存のマイルストーンの key と衝突しない、文字数の上限。不正なら 1 回だけ再試行し、それでも不正なら**何も作らず**秘書の返事として
  「計画を作れなかった: <理由>」を案件の対話に残す（DESIGN §5.6 の Plan kind の規則に近い扱い。Task は作らない）。
- 採用の前は**提案**: `Event::ProjectPlanProposed{version, supersedes, plan}` を plan タスクの events に残し（案件の計画の正本 = plan タスクの列）、
  マイルストーンごとに `milestones`（`proposed`）と top-level の Task（`draft`、`milestone_id` 付き、`depends_on` は key から解決）を作る。
- **人の承認 = HUMAN GATE**: 受信箱の `drafts`（`DraftGroup`、`plan_summary` = rationale と DAG の 1 行ずつ）に 1 つのまとまりとして出す。
  `POST /projects/{id}/project-plan/{version}/decide {decision: "approve" | "reject", note}`（管理系）:
  - `approve`: 1 トランザクションで、全マイルストーンの `proposed → approved`、全 Task の `draft → ready`（既存の `Accept`）、
    `ProjectPlanDecided{version, approved}`。依存の無いものから dispatch される。
  - `reject`: `note` 必須。行と Task を `redesigned` / `cancelled` にし、`note` を秘書への対話として送る（ADR-0038 の ng と同じ）。
  - 個々の draft を 1 件ずつ Accept する既存の操作は、案件計画の draft には使わせない（まとまりで承認する）。

#### D3.4 案件の replan

- 起点: (a) マイルストーンの判定で人が `ng` / `discuss`（ADR-0038）、(b) 人の依頼（GUI の「計画を見直す」）、(c) マイルストーン Task が
  failed / 取り下げ。いずれも秘書の計画 run（`mode: "milestones"`、replan）を起こす。
- 出力は**差分** `celeris.project-plan-delta/1`: `{base_version, rationale, add: [...], modify: [{key, …変える欄だけ}], remove: [key]}`。
  変えてよいのは**まだ dispatch されていない**マイルストーン（Task が draft / ready で run が 0）だけ。走っている・終わったものを変えたいときは
  `remove` ではなく `cancel: [key]` を明示し、承認で `Cancel` を適用する。
- 承認は D3.3 と同じ `decide`。承認までは現行の計画のまま動く（止めない）。承認で `ProjectPlanDecided{version: n+1}`、差分を適用する。

#### D3.5 GUI の案件ページ

- マイルストーンの DAG（節点 = マイルストーン Task、辺 = `depends_on`）。節点に: 途中目標の状態（proposed / approved / in_progress / reached …）、
  Task の状態、進み具合（WU の done / total、子 Task の done / total）、使った quota、止まっている理由（awaiting_human、question、failed）。
- 途中目標の達成判定: 節点を選ぶと ADR-0038 の「秘書のまとめ → ok / 議論 / ng」をその場で出す（今の途中目標カードを DAG の節点に移す）。
- 提案中の計画（draft）は破線で重ねて出し、「承認 / 却下」を置く。モバイル幅（ADR-0055）は DAG をトポロジカル順の縦の一覧に折り返す。

#### D3.6 途中目標の達成（SPEC §7 との整合）

- マイルストーン Task が `done` になると、既存の `ready_milestones`（`notify.rs:255` の `MilestoneReady`）の条件「途中目標に属する仕事がすべて終端」
  が満たされ、秘書の判定 run → 人の ok / 議論 / ng が起きる（ADR-0038 をそのまま使う）。
- DAG のときの `ok` の意味を改める: 「この途中目標を `reached`、**依存するマイルストーンの Go を開く**（D3.2）」。次の途中目標を `approved` にして
  計画 run を起こす旧い意味は、案件計画を持たない案件（D3.8）にだけ残す。
- `ng`: `redesigned`。案件の replan（D3.4）を起こす。依存するマイルストーンは dispatch されないまま（Go が開かない）。

#### D3.7 子 Task への分割（planner の提案 → 既存の委譲）

- execution-plan/2 の `children: [{key, title, objective, acceptance, genre, skills, repos, features, depends_on}]`（`delegate` メッセージ
  （ADR-0016 D2）と同じ中身。`assignee` は持たない）。planner は「この部分は別の deliverable（別の部署の skill・別のリポジトリ・人が個別に
  承認したい）」と判断したときだけ書く。
- daemon は計画の採用と同じトランザクションで、既存の委譲の検証（`delegate::materialize`、深さ・件数・木の run 数の上限、ADR-0069 D1 の担当の破棄、
  ADR-0033 の部をまたぐ提案は秘書への質問）を通して子 Task を作り、`Event::Delegated{run_id: <planner run>, task_ids}` を残す。
- 親の WU は `depends_on: ["child:<key>"]` で子の完了（`done`）を待てる（scheduler は子 Task の状態を外部の依存として決定的に読む）。
  子が failed なら、その WU は blocked（dependency_failed）→ D17 の replan。親の最終レビューは今どおり子が終わるまで待つ（ADR-0016 D2）。
- ADR-0072 D22「委譲を使う Task は atomic」を改める: **WU の run の中からの委譲**は今どおり禁止（WU の run に `available_genres` を渡さない）。
  planner の `children` だけが委譲の入口。
- 大きすぎる WU を WU の中で再分割することはしない（3 層固定）。planner が「WU に収まらない」と判断したら children にする。

#### D3.8 互換

- 案件計画を持たない既存の案件は、今の途中目標（直列）・`kind = plan` の子・ADR-0038 の `ok` の意味のまま。GUI は DAG の代わりに今の途中目標の一覧を出す。
- 案件直下に人が直接作った Task（`POST /tasks` に `project_id`）は、位置の定義でマイルストーンになる。`milestone_id` が無ければ、`task_ops::add` が
  同じトランザクションで途中目標の行（`approved`、title = Task の title）を作って結ぶ（1:1 の不変条件を保つ）。CoS の `create_task` が
  案件直下に作るものは draft にし、人の承認（D3.3 と同じまとまり）を通す。

### D4. 費用の主指標を quota の消費にする

#### D4.1 残量の取り方

- 情報源は既存の `AccountBook` の `RateLimitObservation`（claude-code の `rate_limit_event`、codex の `token_count` の `rate_limits` と
  `account/rateLimits/read`）。新しい外部 API は叩かない。
- `sources_view.rs:32-45` の `window_remaining` を `task_dispatch::accounts` に移して pub にし（llm-proxy は task-dispatch に依存している）、
  dispatcher と `GET /llm/sources` の両方が同じ関数で読む（値を捏造しない規律を 1 か所にする）。
- **run の前後の観測を残す**: dispatcher は WU / atomic の run の開始と終了で、その run のアカウントの観測を読み、
  `Event::QuotaEstimated`（D4.3）の `before` / `after` に入れる。
  - `after` は run の終わりに取った観測（run の中で `rate_limit_event` / `token_count` が来ていれば新しい）。
  - `before` の有効性: 「観測の時刻より後に、そのアカウントを使った消費（celeris の run・プロキシの要求）が無く、枠がリセットされていない」なら、
    300 秒より古くても基準として使う（消費が無ければ値は変わらないため）。そうでなければ `None`。

#### D4.2 run ごとの quota 消費の推定式

- **重み付きトークン** `W = in_uncached + 0.1·cache_read + 1.25·cache_creation + r_out·output`。`r_out` は model の定価の出力 / 入力比
  （`pricing.rs` に単価があればそれ。無ければ provider の既定: claude 5、codex 4）。`weights_version = "quota-weights/1"`。
  定額プランの quota は概ね計算量（≒定価）に比例する、という仮定の初期値で、後の較正で直す（U-F4）。
- 窓 w ∈ {five_hour, seven_day} ごとに、消費 `q_w`（パーセントポイント、0〜100）を次の順で決める（`method` に残す）:
  1. `measured`: `before` と `after` が有効で、同じ窓（`resets_at` が同じ）で、run の間にそのアカウントを使った他の run が無い →
     `q_w = 100·(u_after − u_before)`（負なら 0）。
  2. `apportioned`: 同じアカウントで重なった run の集合について、集合の最初の開始前と最後の終了後の観測が有効 →
     `Δu` を各 run の `W` の比で按分する。集合の最後の run が終わった時点で決まる（それまでは `pending`）。
  3. `estimated`: 上のどちらも無い → `q_w = k_w(source)·W`。`k_w` は同じ source の直近 14 日の `measured` の run（最大 20 件）の
     `Σq / ΣW`（比の和。外れ値に強い）。`measured` が 3 件未満なら 4 へ。
  4. `unknown`: `q_w = None`（0 にしない）。
  5. `free`: Qwen など定額でも従量でもない供給元（`openai-compatible`）は `q = 0`、`method = free`。
- **既知の限界**: 人が同じ Max / ChatGPT のアカウントを celeris の外で使うと、`measured` にその消費が混ざる（上に偏る）。celeris からは
  区別できない。`measured` は「その区間にそのアカウントで起きた消費」であって、run だけの消費の上限と読む（U-F3）。

#### D4.3 記録と集計

- `Event::QuotaEstimated { run_id, work_unit_id: Option<String>, source, account: Option<String>, windows: [{window, before, after, resets_at, used_pct}],
  weighted_tokens, method, calibration: Option<{k, samples}>, weights_version, list_price_usd: Option<f64> }`（新しい Event。
  `WorkerFinished` には足さない。ADR-0072 E1/E2 の逸脱節と同じく、struct literal の多数の箇所を避けるため）。`apportioned` は後から
  同じ `run_id` の Event をもう 1 件出す（最後の Event が有効）。
- `ExecutionMetrics` に追加（`serde(default)`）:
  - `quota: Vec<QuotaUse { source, account, window, used_pct, runs, method_counts }>`（アカウント × 窓の合計）
  - `quota_unknown_runs: u32`
  - `cost_usd_complete: bool`（run の中に単価の無いモデル・usage の無い run が 1 件でもあれば `false`）。`cost_usd` は参考値として残す。
- WU ごとの集計: `TaskExecutionView` の WU に `quota`（その WU の run の合計）を足す。
- `GET /metrics/execution`: グループごとに `quota`（source × 窓の合計と method の内訳）と `cost_usd_complete` を足し、応答の最上位に
  `accounts_now`（`GET /llm/sources` の accounts と同じ値: `remaining_short` / `remaining_long` / `cooldown_until`）を足す。集計は E6 で入った
  `execution_metrics_task_rows` の索引を使い、`QuotaEstimated` を持つタスクだけ events に落ちる（既存の `needs_events` と同じ形）。
- GUI（Execution 節と案件の DAG）: 主表示は「claude-a 5h 3.2pt / 7d 0.6pt（実測）· codex-b 7d 4.1pt（推定）」、定価は小さく
  「参考 $11.21（一部のモデルの単価が不明なため過小）」。`unknown` は「不明」と書き、0 と書かない。
- **quota は観測と記録だけ**。quota の値で dispatch を止めたり、lane・アカウントの選択を変えたりしない（それは ADR-0049 / 0053 の既存の
  残量スコアの仕事で、予算管理は別プロジェクト。CLAUDE.md の禁止）。

### D5. WU ごとの lane と planner のコスト

#### D5.1 planner の出力に WU ごとの `features`

- planner プロンプトの JSON 例と説明に `features`（`TaskFeatureHints` の 9 軸、`low | medium | high`）を足し、**各 WU に少なくとも
  judgment / ambiguity / verifiability / reversibility / consequence の 5 軸を書け**と指示する。軸ごとの 1 行の定義と、例
  （「単価表の更新は judgment=low, ambiguity=low, verifiability=high（checks がある）, reversibility=high」）を載せる。
- lane は書かせない（書けば schema 違反。ADR-0069 D1）。担当・モデル ID も同じ。
- 検証: `features` が `TaskFeatureHints` として読めなければ**計画の検証エラー**にする（今の `.ok()` で黙って捨てるのをやめる。v1 の保存済みの
  計画は読み取り時には今どおり寛容に読む）。5 軸が欠けた WU は拒否しない（欠けた軸は Task から推定した値のまま）が、`RoutingDecided` の
  `reasons` に `features_source: work_unit(partial)` を残す。
- `checks` を持つ WU は view の acceptance が `Check::Command` になり verifiability が上がる（今の `work_unit_view` のまま）。planner に
  「機械的に確かめられる WU には実行できる `checks` を書け」と明記する（E6-3）。

#### D5.2 gate の再利用と lane の上限

- lane は今どおり `decide_for_work_unit`（WU の view + WU の features → `ModelPolicy` → 組織の天井）。新しい規則表は作らない。
- **WU の lane の上限**: `WU の lane ≤ max(Task の lane, standard)`。WU の features で frontier に上がれるのは、Task 自身の判定が frontier
  （または人の明示）のときだけ。planner（LLM）が全 WU に judgment=high を書いて費用を膨らませるのを防ぐ。下げる方向（cheap）は制限しない。
  `[execution] work_unit_lane_cap = "task" | "none"`（既定 `task`）。丸めたら `clamped_by: "work-unit lane cap (task lane <l>)"`。
- 計画 1 つの lane の分布（frontier / standard / cheap の件数）を `ExecutionPlanned` の採用時に `RoutingDecided` とは別に計算し、
  GUI の計画の見出しに出す（膨らみを人が見て分かるように）。

#### D5.3 planner 自身の lane とサイズ

- `[execution.planner] tier`（新設、既定 **`standard`**）。planner run の `LaneDecision` は `rule_id = planner/system-standard`、
  `source = System`（組織の天井で丸めない扱いは今と同じ）。人の明示（Task の `tier:frontier`）があればそれに従う。
- `[execution.planner] max_turns` 既定 40 → **24**、`max_wall_secs` 1,200 → **900**。プロンプトに「計画 run はコードの調査をしない。
  調査が要るなら investigate の WU にせよ」と書く（E6-4 の時間の多くは探索）。
- **計画のサイズの上限**（`validate` で拒否。1 回だけ再試行 → それでも駄目なら ADR-0072 D14 のとおり atomic に倒す / replan なら blocked）:
  `rationale` ≤ 1,500 文字、WU の `objective` ≤ 2,000 文字、`title` ≤ 120 文字、`done_when` ≤ 8 件 × 300 文字、`checks` ≤ 6 件、
  計画の JSON 全体 ≤ 24 KiB。
- **replan は差分だけを出す**: schema `celeris.execution-plan-delta/1`
  `{base_version, rationale, add: [WorkUnitSpec], modify: [{key, …変える欄だけ}], remove: [key], phases?: [PhaseSpec]}`。
  done の WU・統合済みの工程は書かない（daemon が持ち越す）。daemon は旧版に差分を当てて新しい版の全体を作り、その全体を v1 / v2 の検証に
  通す（done の不変は構造上守られる）。`ExecutionPlanned` には今どおり**全体**を残す（監査と replay は変えない）。
  差分の件数（added / changed / removed）を `ExecutionPlanned.reason` の後ろに決定的な形で足す（E5 の未実装「版の差分の件数」も解消）。
  旧形式（全体）の replan 出力も受け付ける（移行期間）。

### D6. repair の分類の追加と成果物の登録

#### D6.1 `review_timeout`

- 条件（決定的）: 不合格の `Check::Command`（または workspace の check）の reason が `command timed out after` で始まる
  （`crates/task-dispatch/src/review.rs:295, 407, 531` の固定文言）。
- 処置: まず **daemon が同じコマンドを 1 回だけ、timeout を 2 倍（上限 1,800 秒）にして再実行する**（LLM なし。cold compile など環境の遅さなら
  これで通る）。通れば判定を差し替えて続ける（`ReviewVerdict` を追記。attempts は変えない）。
- それでも timeout なら repair WU（class `review_timeout`、max_turns 20、wall 1,200、lane cheap）: 目的は「コードを変えずに、この検査が時間内に
  終わる状態を作れ（同じ環境でビルドを温める等）。コードを変える必要があると判断したら `plan_issue` を書け」。

#### D6.2 `merge_base`（Task 内部の最終レビュー）

- 条件: 不合格の `Check::Command` の cmd が `merge-base --is-ancestor` を含む（E6 の criterion 5 の形）、または配送前の検査で
  base が先行している。
- 処置: daemon が Task の worktree で base を決定的に merge する（D1.4 の統合と同じ手順・同じ冪等性）。衝突が無ければ再レビュー。
  衝突があれば repair WU（class `merge_base`、D1.4 の 3 と同じ最小 context）。配送の `[delivery-repair]`（E6）はそのまま。
- repair の class は `Event::RepairScheduled{work_unit_id, key, class, origin: review|integration|delivery|planner}` として残す。
  `execution_metrics` は title の接頭辞ではなくこの Event を読む（`unknown` を無くす。planner が replan で書いた `kind = repair` は `planner`）。

#### D6.3 成果物の登録: **daemon の run 後の走査**（worker のプリアンブルでも API の走査でもない）

- ADR-0067 D3 の未申告成果物の走査を、git worktree の Task にも広げる: run の後に、その run の **`artifacts_dir` の中**
  （WU なら WU の artifacts）を走査し、人が読む成果物（`*.md`、`*.html`、`*.pdf`、`*.csv`、`*.png`。`checkpoint.json` / `result.json` /
  `request.json` / `prompt.txt` / `execution-plan*.json` / `project-plan*.json` と `phase-reports/` の重複は除く）で未登録のものを
  `ArtifactProduced{declared: false}` として記録する（上限は ADR-0067 D3 と同じ 20 件・1 MiB）。
- 採らない案: (a) プリアンブルで登録を促すだけ — LLM の遵守に頼り、E6 でも守られなかった。指示は足すが、登録は走査で保証する。
  (b) `GET /tasks/{id}/artifacts` がディスクを走査する — events が正本という原則（DESIGN 原則 6）に反し、run への帰属も失う。

#### D6.4 小さな修正（F1 に入れる）

- reviewer run の `runs` 索引の `finished_at` / `usage` 欠落（E6-10）: `on_review_finished` の「不合格 → replan」経路の `run_index_finish` を
  確かめ、`celerisctl replay --check` の fixture で再現してから直す。

---

## 4. 上限（ADR-0072 D18 の改訂）

| 設定 | 既定 | 変更 | 超えたとき |
|---|---|---|---|
| `parallel` | `false` | 新設。`true` で planner に v2 を出させる | — |
| `max_parallel_work_units` | 3（上限 6） | 新設。Task・CoS・人・profile で狭められる | 起こさない（待つ） |
| `max_phases` | 5 | 新設 | 計画の検証で拒否 |
| `max_work_units` | 8 → **10**（v2 のみ。v1 は 8 のまま） | 統合 WU・repair は数えない | 計画の検証で拒否 |
| `max_total_work_units` | 16 → **24**（統合 WU を除く） | 並列で repair が増えるため | replan を拒否 → blocked |
| merge の repair / 工程 | 2 | 新設 | replan |
| `max_repairs` / 同じ class | 3 / 2 | class に `review_timeout` / `merge_conflict` を追加 | 従来の `ReviewFail` |
| `max_replans` | 3 | phase-gate の replan も数える | blocked |
| `max_runs_per_task` | 24 | 変えない（並列は run の数を増やさない） | blocked |
| `[execution.planner] tier / max_turns / max_wall_secs` | standard / 24 / 900 | 変更（D5.3） | — |
| 計画のサイズ | D5.3 の表 | 新設 | 拒否 → 1 回再試行 |
| `work_unit_lane_cap` | `task` | 新設 | 丸めて記録 |
| 案件計画のマイルストーン数 | 12 | 新設 | 拒否 → 1 回再試行 |
| `max_task_tokens` | 無し | 変えない。**quota の上限は作らない**（D4.3） | — |

## 5. 監査・互換・SPEC との関係

### 5.1 監査（events が正本）

新しい Event（すべて追加のみ、Task の状態を変えるのは `Transitioned` だけ）: `WorkUnitCommitted`、`PhaseIntegrated`、`PausePointsResolved`、
`PhaseReported`、`QuotaEstimated`、`RepairScheduled`、`ProjectPlanProposed`、`ProjectPlanDecided`。新しい `Trigger`: `PhaseGate`、`PhaseResume`
（どちらも attempts を変えない。`attempt_history` / `replay` / `classify_task_failure` の集合に入れない。遷移の全列挙テストに足す）。
`RoutingRecord` は既存の `work_unit_id` を実際に埋める。

### 5.2 migration 0027（`SCHEMA_VERSION` 26 → 27。F2 で入れる）

```sql
-- ADR-0074 D1。すべて events の派生（lease は tasks の lease と同じく揮発で、replay の比較から外す）。
ALTER TABLE work_units ADD COLUMN phase TEXT;               -- v2 の工程の key（v1 は NULL）
ALTER TABLE work_units ADD COLUMN lease_run_id TEXT;
ALTER TABLE work_units ADD COLUMN lease_expires_at TEXT;
ALTER TABLE work_units ADD COLUMN branch TEXT;              -- celeris-wu/<task_id>/<key>
ALTER TABLE work_units ADD COLUMN base_commit TEXT;
ALTER TABLE work_units ADD COLUMN head_commit TEXT;         -- WorkUnitCommitted
ALTER TABLE work_units ADD COLUMN integrated_commit TEXT;   -- PhaseIntegrated
CREATE INDEX IF NOT EXISTS idx_work_units_lease ON work_units (lease_expires_at) WHERE lease_run_id IS NOT NULL;
```

- `kind` は TEXT なので `integrate` を足しても CHECK の変更は要らない（`WorkUnitKind::parse` に足す）。
- F1 / F3 / F4 は migration を持たない（F3 の quota は Event と既存の索引のフォールバック、F4 の案件計画は plan タスクの events と既存の
  `milestones` / `tasks` の列）。F4 で `Project.auto_advance` は `projects` の JSON の中（ある場合）か列の追加が要るかを F4 の着手時に確かめ、
  列が要るなら migration 0028 にする。
- `SchemaTooNew`（ADR-0013 D5）どおり旧いバイナリは 27 を開けない。ロールバックは ADR-0040 D2。追加の列は落としても events から作り直せる
  （`rebuild_work_units_and_runs` に phase / branch / commit の復元を足す。lease は比較しない）。
- API / GUI は追加だけ（`TaskExecutionView` の WU に phase / branch / quota、`ExecutionMetrics` の quota 欄、`AttentionItem::PhaseCheckpoint`、
  新しいエンドポイント 3 つ）。`api-v1.schema.json` / `event.schema.json` / `execution-plan.schema.json` を再生成し、新しく
  `project-plan.schema.json` を足す。

### 5.3 SPEC との関係（SPEC.md は編集しない）

- §3.3 仕事の木: 案件の最上段 = マイルストーンの DAG になる。WU は今どおり木に出さない（ADR-0072 D2）。子 Task は木に出る。
- §3.5 報告: 工程の途中報告（D2.3、機械的な束ね）→ マイルストーンの判定（ADR-0038、秘書のまとめ）→ 案件、の順で上ほど圧縮される。
  通知は「数時間単位」: 途中確認は既定で無効、止まるのは人・CoS が頼んだ工程だけ。
- §3.6 認可: 途中確認は認可（今回だけ / 今後ずっと）ではない。approvals の行を作らない。
- §5「部をまたぐ連携は秘書が認める」: planner の children が別の部に当たれば既存の秘書への質問（ADR-0033）。WU は今どおり部をまたがない。
- §7 途中目標: 「予め大まかに決めておいて、適宜再設計する」= 案件計画（D3.3）と案件 replan（D3.4）。「途中目標の達成ごとに判定」=
  マイルストーンごとの ADR-0038。**差分**: §7 は途中目標を「次の 1 つ」ずつ出す読み方もでき、今の実装（直列の鎖）はそれに合わせていた。
  本 ADR は「全体を大まかに DAG で出し、Go は 1 つずつ（依存ごと）」にする。人の決定 3 に基づく拡張で、SPEC の文言とは矛盾しない。
- §8（追記）「登録済みアカウントの予算とのバランス」: D4 は観測と記録だけ。バランスを取る選択は ADR-0049 / 0053 の既存の残量スコアのまま。
- DESIGN 原則 1（協調に LLM を使わない）: 統合の merge・途中報告・quota の推定・案件計画の検証と採用はすべて決定的。LLM は planner / worker /
  reviewer / 秘書の計画 run と repair WU だけ。

## 6. 採らない（代替案と却下の理由）

- **R1: 同じ worktree で WU を並列に走らせる**。同じファイルの同時編集、`.git/index.lock` の奪い合い、checkpoint の上書き。
- **R2: 並列の WU を子 Task にする**。ADR-0072 R1 と同じ（worktree・レビュー・配送が複製され、GUI が実行の雑音で埋まる）。
- **R3: 統合を常に LLM にさせる**。衝突の無い merge は決定的にできる。LLM は衝突の解消だけ（repair WU）。
- **R4: 工程ごとに LLM の reviewer**。費用と壁時計が工程の数だけ増える。工程では決定的な検査、人が見たいときは途中確認（D1.8）。
- **R5: 同じ工程の中で複数の依存を許す（多親の積み上げ）**。WU の基点を作るのに工程の中の小さな統合が要り、統合が 2 か所になる。後の工程に置けば足りる。
- **R6: 兄弟の WU が失敗したら走っている WU を止める**。途中の成果を捨てる。replan は done の WU を持ち越すので、完走させた方が得。
- **R7: `Status` に `awaiting_human` を足す**。ADR-0072 R3 と同じ。`Blocked` + reason + `ExecutionPhase` で足りる。
- **R8: マイルストーンを `milestone: true` のフラグで決める**。D3.1 の 1〜4。
- **R9: 途中目標を Task だけで表し `milestones` を捨てる**。reached / redesigned / paused（ADR-0044 D6）と ADR-0038 の判定の台帳が消え、
  今の GUI・通知（`MilestoneReady`）を作り直すことになる。1:1 で結べば両方を活かせる。
- **R10: 案件計画を人の承認なしで採用する**。SPEC §7 と人の決定 3（HUMAN GATE）に反する。
- **R11: 費用の主指標を定価 USD のままにする**。定額プランでは実支出ではなく、単価の無いモデルが 0 に見える（E6-6）。
- **R12: quota で dispatch を止める・選択を変える**。予算管理は別プロジェクト（CLAUDE.md）。選択は既存の残量スコア。
- **R13: planner に lane を書かせる**。ADR-0069 D1。features だけ書かせ、lane は policy と天井と上限（D5.2）で決める。
- **R14: replan で全体を書き直させる**。done の WU の写しで出力が膨らみ、写し間違いで拒否される（E6-5）。
- **R15: WU の中での再分割（再帰）**。3 層固定（人の決定 3）。大きすぎれば children。
- **R16: 成果物を API がディスクから拾う**。D6.3 (b)。
- **R17: 残量を外部の新しい API で取りに行く**。既存の観測（stream の rate limit と codex の rateLimits）で足りる。足りない分は `unknown` と書く。

## 7. Phase F1〜F5 の実施計画

各 Phase の完了時に `cargo fmt --all -- --check` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` を通す。
GUI を触る Phase では `pnpm typecheck` / `lint` / `test` / `gen:types` の差分ゼロ（2 回）/ `mobile-audit` の違反 0 / `pnpm build`。
テストは偽のアダプタとスタブの CLI・一時ディレクトリの git リポジトリだけを使い、外部ネットワークに出ない。

### F1: WU ごとの lane、planner の lane とサイズ、replan の差分、repair の分類、成果物の登録（小さく、費用に直結）

- **受け入れ条件**:
  - (a) planner プロンプトに WU ごとの `features` の説明と例があり、`build_execution_plan_prompt` のスナップショットで確かめる。
  - (b) `features` が `TaskFeatureHints` として読めない計画は検証エラー（1 回再試行 → atomic）。保存済みの v1 計画（任意の JSON）は読める。
  - (c) `judgment=low, ambiguity=low, verifiability=high, reversibility=high` と `checks` を持つ WU が cheap、features の無い WU は Task の lane、
    Task が standard のとき WU の features が frontier を示しても standard に丸まる（`clamped_by` に記録）。
  - (d) planner run が `standard`（`rule_id = planner/system-standard`）、`max_turns` 24 / `max_wall_secs` 900。人の明示 `tier:frontier` の Task では frontier。
  - (e) サイズ上限を超えた計画が拒否され、再試行の後に atomic に倒れる。
  - (f) replan の差分出力（add / modify / remove）が旧版に当てられ、done の WU を書かなくても v(n+1) が採用される。全体形式の replan も受け付ける。
    `ExecutionPlanned.reason` に差分の件数。
  - (g) `command timed out after` の不合格で、daemon が 2 倍の timeout で 1 回再実行し、通れば attempts 不変で合格に進む。通らなければ
    `review_timeout` の repair WU。
  - (h) `merge-base --is-ancestor` の不合格で、衝突なしなら daemon の merge → 再レビュー、衝突ありなら `merge_base` の repair WU。
  - (i) `RepairScheduled` が残り、`repairs_by_class` に `unknown` が出ない（planner の repair は `planner`）。
  - (j) git worktree の Task で `artifacts/report.md` が `ArtifactProduced{declared:false}` として登録され、`GET /tasks/{id}/artifacts` に出る。
  - (k) reviewer run の `runs` 索引の欠落が直る（`replay --check` の fixture）。
- **触るファイル**: `crates/task-core/src/{execution_plan.rs, model_policy.rs, execution.rs, execution_metrics.rs, model.rs}`、
  `crates/task-worker/src/claude_code.rs`（planner プロンプト）、`crates/task-dispatch/src/{dispatcher.rs, review.rs, undeclared_artifacts.rs}`、
  `crates/task-ops/src/execution.rs`（差分の適用）、`crates/celeris/src/config.rs`（`[execution.planner] tier`、`work_unit_lane_cap`）、
  `docs/protocol/execution-plan-delta.schema.json`（新規）。
- **テスト（名前は目安）**: `execution_plan::tests::features_must_parse_as_task_feature_hints`、`plan_size_limits_reject_oversized_rationale`、
  `model_policy::tests::work_unit_features_lower_a_mechanical_unit_to_cheap`、`work_unit_lane_is_capped_by_the_task_lane`、
  `dispatcher::tests::planner_run_uses_the_standard_lane_by_default`、`replan_delta_carries_done_units_without_restating_them`、
  `review_timeout_reruns_once_with_double_timeout_before_repair`、`merge_base_failure_merges_base_deterministically`、
  `repair_scheduled_event_names_the_class`、`git_worktree_task_registers_report_md_as_an_artifact`、
  `claude_code::tests::execution_plan_prompt_asks_for_per_unit_features`。
- **並行**: F1 は単独（F2 / F3 と `dispatcher.rs` を共有するので先に入れる）。

### F2: WU の並列（schema v2、WU の worktree、統合 WU、鍵 (task, WU)）

- **受け入れ条件**:
  - (a) execution-plan/2 の検証（工程の順、同じ工程の依存は 1 つまで、後の工程への依存の拒否、`children` は空だけ許す）。v1 の計画の挙動・プロンプトが変わらない。
  - (b) migration 0027 と旧い DB からの移行テスト。
  - (c) 3 つの独立した WU（同じ工程）が `max_parallel_work_units = 3` で同時に走り、それぞれ `celeris-wu/<task>/<key>` の worktree で commit し、
    統合 WU が決定的に merge して `PhaseIntegrated` を残す。`max_parallel_work_units = 2` なら 2 本ずつ。provider の `concurrency = 1` なら 1 本ずつ。
  - (d) 積み上げ: 同じ工程の `b depends_on a` は a の完了後に a のブランチから切られ、統合は葉（b）だけを merge する。
  - (e) 衝突する 2 つの WU で `merge-<phase>-<key>` の repair WU ができ、done の後に統合が続きから再開される（済んだ merge を飛ばす）。
  - (f) 統合後の検査の失敗が repair（分類に当たる）か replan（当たらない）になる。
  - (g) 兄弟が走っている間の WU の failed / question で Task が遷移せず、in-flight が 0 になってから replan / `WorkerQuestion` になる。
  - (h) 再起動の照合: WU の run の途中でデーモンを作り直すと、WU が needs_continuation / ready に戻り、統合の途中なら冪等にやり直す。
    全部が死んだ Task は既存の `reclaim_expired_leases` → `InfraRequeue`。
  - (i) Cancel で走っている全 WU の run が止まり、未完了の WU が cancelled。
  - (j) 公平性: Ready の別の Task がいるとき、並列 WU の 2 本目より先にその Task の 1 本目が起きる。
  - (k) remote / `dir` の repo / Shared の Task では並列 1 に倒れ、理由が記録される。
  - (l) GUI: WU の表に工程の列（見出しで工程ごとにまとめる）・同時に走っている run・ブランチ。`gen:types` の差分ゼロ、`mobile-audit` の違反 0。
- **触るファイル**: `crates/task-core/{migrations/0027_parallel_work_units.sql, src/store.rs, src/execution_plan.rs, src/model.rs, src/transition.rs}`、
  `crates/task-dispatch/src/{dispatcher.rs, execution_scheduler.rs, checkpoint.rs}`、新規 `crates/task-dispatch/src/integration.rs`（git の merge と冪等性。
  daemon 側の git 操作だけ）、`crates/task-worker/src/{protocol.rs, preamble.rs}`（WU の worktree と commit の指示）、`crates/task-ops/src/{execution.rs, replay.rs, view.rs}`、
  `crates/task-api`（types / schema）、`crates/celeris/src/config.rs`、`gui/app/components/ExecutionSection.tsx`、`gui/app/lib/task-execution.ts`。
- **テスト**: `execution_plan::tests::v2_rejects_two_intra_phase_dependencies`、`runnable_work_units_respects_the_parallel_limit`、
  `dispatcher::tests::three_independent_units_run_in_parallel_and_integrate`、`stacked_unit_branches_from_its_dependency`、
  `integration_conflict_creates_a_merge_repair_and_resumes`、`sibling_failure_waits_for_in_flight_units_before_replan`、
  `parallel_units_survive_dispatcher_restart`、`cancel_stops_every_work_unit_run`、`ready_task_first_run_beats_a_second_parallel_unit`、
  `remote_workspace_falls_back_to_serial`、`store::tests::migration_27_adds_work_unit_lease_columns`、`integration::tests::merge_is_idempotent_after_restart`（一時 git リポジトリ）。
- **並行**: F1 の後。(a) の schema の commit を最初に入れれば、F3 の途中確認はその上で並行できる。

### F3: 途中確認と quota 指標（2 つは独立。quota は F1 の後すぐ並行可、途中確認は F2 (a) の後に並行可）

- **受け入れ条件（途中確認）**:
  - (a) `PausePolicy`（none / each_phase / after）を人と CoS が書け、採用時に `PausePointsResolved` へ解決される。planner の出力には欄が無い。
  - (b) 停止点の工程の統合の後で `PhaseGate`（→ Blocked、reason `awaiting_human`、attempts 不変）、`PhaseReported` と `phase-reports/<n>-<phase>.md`。
  - (c) 受信箱に `PhaseCheckpoint`、questions には出ない。`NotificationKind::PhaseCheckpoint` が 1 回だけ鳴り、`QuestionBlocked` は鳴らない。
  - (d) `continue` / `replan`（note 必須）/ `withdraw` がそれぞれ `PhaseResume{Continue}` / `PhaseResume{Replan}` → planner / `Cancel` になる。awaiting_human 以外は 409。`Answer` は 409。
  - (e) v1 / atomic の Task で `pause_after` が無害に受け付けられる。
  - (f) GUI: Task 詳細に途中報告と 3 つのボタン、受信箱の新しい項目。
- **受け入れ条件（quota）**:
  - (g) run の前後の観測から `QuotaEstimated` が measured / apportioned / estimated / unknown / free を決定的に出す（表のテスト）。
  - (h) `before` の有効性（消費の無い古い観測は使う、枠のリセットや他の消費があれば使わない）。
  - (i) `ExecutionMetrics.quota` と `cost_usd_complete`、WU ごとの quota、`GET /metrics/execution` の `quota` と `accounts_now`。
  - (j) GUI: quota が主、定価が参考（「不完全」の表示）、unknown は「不明」。
  - (k) quota の値で dispatch・lane・アカウントの選択が変わらないこと（同じ入力で同じ `RoutingDecided` になることを確かめる）。
- **触るファイル**: `crates/task-core/src/{model.rs, transition.rs, execution_metrics.rs, notify.rs}`、新規 `crates/task-core/src/quota.rs`（推定式の純粋関数）、
  `crates/task-dispatch/src/{dispatcher.rs, accounts.rs}`、`crates/llm-proxy/src/sources_view.rs`（関数の移動だけ）、`crates/task-ops/src/{inbox.rs, add.rs, view.rs}`、
  `crates/task-api`（phase-gate、metrics、types）、`crates/celeris/src/notify.rs`、`gui/app/components/ExecutionSection.tsx`、受信箱の画面。
- **テスト**: `quota::tests::measured_when_exclusive_and_fresh`、`apportioned_by_weighted_tokens_when_runs_overlap`、`estimated_uses_ratio_of_sums_calibration`、
  `unknown_is_never_zero`、`dispatcher::tests::pause_after_design_blocks_with_awaiting_human`、`phase_gate_continue_resumes_the_next_phase`、
  `phase_gate_replan_requires_a_note`、`notify::tests::phase_checkpoint_is_not_a_question`、`execution_metrics::tests::cost_usd_complete_is_false_with_an_unpriced_model`。
- **並行**: quota（(g)〜(k)）と途中確認（(a)〜(f)）は別の担当で並行できる。`dispatcher.rs` の衝突は、quota は run の開始・終了の記録だけ、
  途中確認は統合の後の分岐だけを触るよう分ける。

### F4: 案件レベルの計画（CoS の計画、人の承認、GUI の DAG、子 Task の分割）

- **受け入れ条件**:
  - (a) `is_milestone_task` の述語（対話・plan・support・子を除く）と、案件直下の Task と途中目標の 1:1（`task_ops::add` が行を作る）。
  - (b) `POST /projects/{id}/plan {mode: "milestones"}` の秘書 run（偽のアダプタ）が `project-plan/1` を出し、検証を通ると `ProjectPlanProposed`・
    途中目標（proposed）・top-level の draft Task（`depends_on` 付き）ができ、受信箱の `drafts` に 1 まとまりで出る。
  - (c) `decide approve` で全部が approved / ready になり、依存の無いものから dispatch される。`reject` で redesigned / cancelled と秘書への対話。
  - (d) 依存先が done でも reached までは依存する Task が dispatch されない。`ok` で reached → Go が開く。`auto_advance = true` なら done で進む。
  - (e) 案件 replan の差分（dispatch 前のものだけ変えられる、`cancel` は明示）が同じ承認を通る。承認までは現行の計画で動く。
  - (f) execution-plan/2 の `children` が既存の委譲の検証を通って子 Task になり、`child:<key>` の依存で WU が待つ。部またぎは秘書への質問。
  - (g) 案件計画の無い既存の案件の挙動（直列の途中目標、ADR-0038 の `ok` の旧い意味）が変わらない。
  - (h) GUI の案件ページに DAG（節点の状態・進み・quota・止まっている理由）、提案中の計画の承認 / 却下、節点ごとの ok / 議論 / ng。モバイル幅は縦の一覧。
- **触るファイル**: 新規 `crates/task-core/src/project_plan.rs`（schema・検証・差分）、`crates/task-core/src/{org.rs, model.rs, delegate.rs}`、
  `crates/task-ops/src/{project_plan.rs, milestone_review.rs, add.rs, inbox.rs}`、`crates/task-dispatch/src/dispatcher.rs`（Go の判定、children の採用）、
  `crates/task-worker/src/claude_code.rs`（秘書の案件計画プロンプト、planner の children の説明）、`crates/task-api/src/{project_plan.rs, types.rs, schema.rs}`、
  `gui/app/routes/projects.$id.tsx` ほか、`docs/protocol/project-plan.schema.json`。
- **テスト**: `project_plan::tests::rejects_cycles_and_unknown_keys`、`task_ops::project_plan::tests::approve_readies_the_whole_dag_in_one_transaction`、
  `dispatcher::tests::dependent_milestone_waits_for_reached_not_done`、`project_replan_delta_cannot_modify_a_started_milestone`、
  `planner_children_become_delegated_child_tasks`、`legacy_project_keeps_linear_milestones`。
- **並行**: F2 / F3 の後（`dispatcher.rs` と GUI の衝突を避ける）。ops / api / GUI は分けて並行できる。

### F5: dogfood（2 回）

- **F5a（F1 の後）**: gate = on（本番設定の切り替えは人の判断）で、E6 と同規模の自己改善のタスクを 1 件流す。E6 と並べる指標:
  WU の lane の分布（frontier / standard / cheap）と `rule_id`、planner の run の時間と出力トークン、replan の出力の大きさ、repair の class
  （`unknown` が 0 か）、成果物の一覧、attempts、壁時計、定価（参考）。
- **F5b（F4 の後）**: `parallel = true`、案件計画（2〜3 のマイルストーン、独立 / 直列を混ぜる）と `pause_after` を 1 か所で流す。E6 と F5a に並べる指標:
  壁時計（工程ごと・並列の効き目）、統合の衝突と repair、途中確認の往復、アカウント × 窓の quota（method の内訳）、`accounts_now` の推移。
- 実行: 認証が使える環境ではエージェントが実行して証跡を PROGRESS に残す（ADR-0009 P-34）。使えなければ手順を書いて人に頼む。
  `parallel` と `gate` の既定を変えるかは結果を見て人が決める。

## 8. 未解決事項（F0 時点。既定を選んで記録する）

- **U-F1**: 同じ工程の並列 WU が同じファイルを触る計画をどこまで planner に避けさせるか。既定は「衝突は統合で repair」。F5b の衝突率で、
  WU の `context.paths` の重なりを検証で警告するかを決める。
- **U-F2**: 並列の WU が ADR-0066 の共有 `CARGO_TARGET_DIR` のロックで実質直列になる可能性。F5b で WU の run の中のビルド待ちを測り、
  WU ごとの target dir（コピーから始める）にするかを決める。
- **U-F3**: 人が celeris の外で同じ Max / ChatGPT のアカウントを使うと `measured` が上に偏る。区別の手段は無い。GUI に注記する。
- **U-F4**: 重み付きトークンの係数（cache 0.1、書き込み 1.25、出力比）と、定額プランの quota が定価に比例するという仮定は未検証。
  `measured` が溜まったら source ごとに `W` と `q` の相関を見て直す（`weights_version` を上げる）。
- **U-F5**: codex の `token_count` の rate limits がどの頻度で来るか（run の中で `after` が新しくなるか）。来なければ codex の run は
  estimated / unknown に寄る。F3 で codex のスタブ・F5 の実機で確かめる。
- **U-F6**: 案件直下に CoS が `create_task` で作る Task を draft にする変更（D3.8）が、今の CoS の使い方（案件の中の小さな雑用）を重くしないか。
  F4 の着手時に本番の案件直下の Task の作られ方を数えて決める。
- **U-F7**: `Project.auto_advance` を列にするか JSON にするか（§5.2）。
- **U-F8**: 途中報告を秘書に要約させるか（SPEC §3.5 の圧縮をもう 1 段）。既定はしない（機械的な束ねだけ）。F5b で人の読みやすさを見て決める。
- **U-F9**: `review_timeout` の再実行は環境の遅さを前提にする。本当に遅いテスト（無限ループ）では 2 倍の時間を無駄にする。上限 1,800 秒で抑えた。
- ADR-0072 §7 の U1（context 超過の文言）・U2（codex の peak context）・U7（idle timeout）・U8（上限到達の質問のボタン）・U9（remote の
  mechanical checkpoint）・U10（gate の閾値）は本 ADR では扱わない（F5 で機会があれば記録する）。U3（WU 並列）は D1、U4（WU の commit）は
  D1.2 で v2 について解消する。U5（部をまたぐ WU）は D3.7 の children で「別の Task」にする既定を保つ。

## Phase F1 実装時の逸脱・明確化（2026-09-26）

実装しながら見つかった、D5・D6 の記述とコードの食い違い・簡略化・明確化。黙って逸脱せず、ここに記録する
（branch `worktree-agent-abdca7309a6944c0a`、commit は `docs/PROGRESS.md` の Phase F1 節を参照）。

- **環境修正**: 着手前に確認したところ、NFS 移行後の toolchain（cargo/rustc 1.98.1）で
  `crates/task-worker/src/codex_account.rs` の `clippy::result_large_err` は既に別コミット
  （`ad165d3`、`rpc`/`read_account_limits` への `#[allow(clippy::result_large_err)]` + 理由コメント）
  で解消済みだった。F1 の着手時点で `cargo clippy --workspace --all-targets -- -D warnings` は
  warning 0 を確認済み（このコミットへの追加変更はしていない）。
- **D5.1 の `deny_unknown_fields`**: ADR は「`features` が `TaskFeatureHints` として読めなければ
  検証エラー」とだけ書いていたが、`TaskFeatureHints` 自体に `#[serde(deny_unknown_fields)]` が
  無かった（他の欄と違って寛容に読む型のまま）。これでは `{"lane": "frontier"}` のような
  D5.1 が禁じたい欄も「空の `TaskFeatureHints` として読める」ことになってしまうため、
  `TaskFeatureHints` に `deny_unknown_fields` を追加した（`docs/api/v1/event.schema.json` の
  `TaskFeatureHints` に `additionalProperties: false` が付く。`NewTaskSpec.features` 等、
  他の呼び出し元も同じ型を使っているので、そちらも自動的に厳格になる。既存の呼び出し元は
  すべて既知の欄しか書いていないため後方互換は壊れない）。
- **D5.2 の WU lane 上限の実装位置**: ADR は「WU の lane ≤ max(Task の lane, standard)」とだけ
  決めていたが、「Task の lane」をどの時点のどの計算から取るかは明示していなかった。
  `model_policy::decide_for_work_unit` に `task_lane: Option<Tier>` 引数を追加し、呼び出し側
  （`Dispatcher::decide_lane_for_work_unit`）が `[execution] work_unit_lane_cap` の設定に応じて
  `decide_for_task(task, ceiling)`（**エスカレーション前**の policy の結果）を渡す、という形にした。
  リトライのエスカレーション（D6「WU の run はエスカレーションを行わない」）を Task の上限にも
  持ち込まない、という既存方針に合わせた選択。
- **D5.3 の replan プロンプト**: 「replan は差分だけを出す」という決定を、プロンプトにも反映した
  （`replan_context_section` が `celeris.execution-plan-delta/1` の形を示し、差分で書けなければ
  全体形式でもよいと明記）。ADR 本文はプロンプト文面までは指定していなかったため、これは実装上の
  補足（受け入れ条件は「差分を受け付ける」であって「プロンプトを変える」ではないが、D5.3 の
  「replan は差分だけを出す」という決定の趣旨に沿わせた）。
- **D5.3 の差分パッチの「欄を消す」制約**: `WorkUnitPatch` の各欄は `Option<T>`
  （`Some` = 上書き、欠落 = 変えない）にした。素の `serde`/`serde_json` は `Option<Option<T>>` で
  「欄が無い」と「明示的に `null`」を区別できない（`serde_with::rust::double_option` のような
  追加クレートが要る）ため、`harness`/`features`/`budget` のような既に `Option` 型の欄を
  **空に戻す**ことは差分ではできない（新しい値を書く、または `remove` + `add` で作り直す）、
  という制約を型コメントと `docs/protocol/execution-plan-delta.schema.json` の説明に明記した。
  ADR 本文はこの制約に触れていなかったので、Phase F1 の簡略化として記録する。F2/F3 で困る場面が
  出たら見直す（U 節に追記候補）。
- **D6.1/D6.2 の実装位置**: timeout の 2 倍再実行と merge-base の決定的な merge は、
  「daemon が判定を差し替えて続ける」という書き方だったので、`try_review_repair`
  （repair WU を作る層）ではなく、その手前の `review_task`/`run_work_unit_checks`
  （`crates/task-dispatch/src/review.rs` の `exec_check_with_repair_retries`）の中に実装した。
  結果として、1 回目の（失敗した）試行そのものは別の Event として残らず、**最終的な**
  `Event::ReviewVerdict` だけが記録される（「判定を差し替える」を文字どおり実装した形。
  1 回目の timeout の証跡が欲しい場合は `WorkerProgress` に足すことを F2/F3 の課題として残す）。
- **D6.2 の merge コマンド**: Task 内部の最終レビューでの決定的な merge は `git merge --no-edit <ref>`
  を使った（D1.4 の工程統合が使う `--no-ff` は付けない）。Task 内部では複数 WU の統合ではなく
  「base に追いついているか」だけを見ているため、fast-forward できるならそれでよい、という判断。
  F2 で D1.4 の統合を実装するときに、そちらは ADR どおり `--no-ff` を使う。
- **D6.2 の origin の対応範囲**: `execution::RepairOrigin` に `Integration`/`Delivery` を型として
  用意したが、F1 では **`Review`（Task 内部の最終レビュー）と `Planner`（replan が自ら書いた
  repair WU）だけ**を実際に発行する。`Integration` は F2 の工程統合が無いと発生しない。
  `Delivery`（`crates/celeris/src/delivery.rs` の `[delivery-repair]`）は ADR-0074 §6 F1 の
  触るファイル一覧に無く、今回は変更していない（`Event::RepairScheduled` を出さない）。
  配送側の repair WU は Phase E6 の実装のとおり `"repair (<bucket>): …"` という題名の規約に
  従っているため、`execution_metrics::summarize` の**旧経路**（title の接頭辞からの復元）で
  今までどおり分類でき、`unknown` への退行は無い（F1 で新しく `unknown` になるのは、
  `RepairScheduled` も題名の規約も無い、既存の replan 由来 repair WU だけで、これは本 Phase の
  変更で解消済み）。配送側にも `RepairScheduled` を出すかは F2/F3 の課題として残す。
- **(k) で追加で見つかった不具合（本 Phase で修正）**: `Event::WorkerFinished.end` は reviewer
  run では常に `None` だった（`usage` だけでなく）ため、`task_ops::replay::rebuild_work_units_and_runs`
  はこれを常に `RunIndexStatus::HarnessError` として復元し、実際の判定（`completed`/`failed`）と
  食い違っていた。ADR の (k) は `finished_at`/`usage`/`end` の 3 つを挙げていたので、この
  `end` の食い違いも本 Phase の範囲内として直した（正常完了・実質的な不合格・供給側/インフラ都合の
  deferred・discarded の 4 経路それぞれで、`finish_reviewer_run_index` に渡す `RunIndexStatus` と
  同じ判定を `Event::WorkerFinished.end` にも書き戻す）。
- **(k) で見つかったが本 Phase では直していない別の不具合**: 上記の調査中、`dispatcher.rs` の
  「計画の無い Task（暗黙の WorkUnit）の worker run」の `runs` 索引書き込みが、
  `current_run_seq(&events) + 1` を呼んでいる箇所（Phase E2b で追加。`WorkerStarted` を追記した
  **あとに**呼んでいる）で `seq` が実際より 1 大きく記録される不具合を見つけた
  （`task_ops::derive::current_run_seq` の他の 2 箇所の呼び出しは「`WorkerStarted` が既に
  events に入っている前提で、その番号をそのまま返す」という契約で `+1` しておらず、この 1 箇所だけ
  契約を誤って `+1` している）。ADR-0074 §6 F1 の受け入れ条件にもテスト表にも無い、
  ADR-0072 Phase E2b 由来の別の欠陥のため、本 Phase では直さず、`docs/PROGRESS.md` の
  「提案」に記録する（`reviewer_run_usage_is_recorded_in_the_runs_index_and_survives_replay_check`
  テストは worker run の `seq` を比較対象から意図的に外している）。
