# ADR-GUI-0007: Phase G4（プロバイダとデーモン）で確定した細部

- 日付: 2026-09-15
- 状態: **Accepted**（Phase G4 の実装で確定。docs/DESIGN.md §4.5, §4.6, §6.5, §10 Phase G4 の範囲内の細部）
- 関連: docs/DESIGN.md §4.5, §4.6, §6.5, §10 Phase G4 / docs/taskd-api-v1.md §3.19〜§3.21, §5.8 / docs/adr/0004, 0005, 0006

## 1. 文脈

G4 は「プロバイダとデーモン」（light）。`GET /providers` と `GET /daemon` の本格的な画面、`scripts/taskd.sh fixture multi-account` / `fixture unroutable`、
taskd 停止/復旧バナーの検証、実行中 run の in_flight 表示を作る。実装前に、taskd 側のディスパッチャの実装を調査し（Explore agent、`/home/rmaeda/workspace/agent-platform`
の `crates/task-dispatch/src/policy.rs` / `dispatcher.rs` / `crates/taskd/src/lib.rs` を確認）、fixture の作り方に直結する制約が見つかったので記録する。

## 2. 決定

### D1. `cooldown`（`DaemonSnapshot.cooldowns` / `ProviderView.cooldown`）はプロセス内メモリのみで、DB からは絶対に再構築されない

- `StaticPolicy`（`crates/task-dispatch/src/policy.rs`）は `cooldown_until: HashMap<ProviderId, (Instant, CooldownReason)>` を `StaticPolicy::new` で**必ず空**に作る。
  `Instant` は単調クロックで、プロセスをまたいで意味を持たない。`report()`（cooldown への書き込み）はディスパッチャが `ProviderThrottled` 系の outcome を見た
  その場でしか呼ばれず、起動時に `ProviderThrottled` イベントを読んで復元する経路はどこにも無い（`Dispatcher::new` / `crates/taskd/src/lib.rs::build_dispatcher` を確認）。
  `ProviderThrottled` イベントは「ポリシーの状態 → イベント」の一方通行の監査記録で、逆方向（イベント → ポリシー状態）の経路は無い。
- したがって: **`scripts/taskd.sh fixture multi-account` で一度 `--until-idle` を回してスロットルを起こし、その後 taskd を再起動して e2e から見る、という
  他の fixture（`basic`）と同じパターンは使えない。** 再起動した瞬間に acct-a の cooldown は消える。
- 対応: `fixture multi-account` は設定ファイル（2 プロバイダ、フェイクワーカー）と workspace だけを用意し、**DB には何も入れない**（`prepare()` 相当。`basic` のように
  `taskctl add/approve` や `--until-idle` を fixture コマンド側で先に実行しない）。実際にスロットルを起こす `taskctl add`/`approve` は `e2e/g4.spec.ts` の `beforeAll` が、
  **`scripts/taskd.sh start multi-account` で起動した後の、同一の生きたプロセスに対して**行う（G3 の `Slow-F` を fixture に焼き込まず e2e 内で作ったのと同じ考え方。
  ADR-0006 D6）。このプロセスを G4 の e2e の間はずっと動かし続け、他の受け入れ条件（2〜4）も同じプロセスに対して検証する（`stop`/`start` を試す受け入れ条件 3 を除く）。

### D2. `awaiting_human` / `unroutable` は毎 tick、現在の DB の状態から再計算される（`fixture basic` の再起動パターンで問題ない）

- 実機で確認: `fixture basic` を作り直して `start basic` した直後（`--until-idle` で作った DB に対する新しいプロセス）でも `GET /daemon` の `awaiting_human` に
  Human-B の id が正しく出る。taskd 側は `recover_reviews()`（毎 tick、`Status::Reviewing` のタスクを DB から再照会）で `awaiting_human` を作るため、
  D1 の cooldown と違い、プロセスの再起動に耐える。`unroutable` も同様に「この tick の判定」（`docs/taskd-api-v1.md` §3.20）であり、`Status::Ready` のタスクと
  現在の設定を突き合わせて毎 tick 決まる。
- したがって: `fixture unroutable`（cheap タスクに frontier だけのプロバイダ）と、受け入れ条件 2 の `fixture basic` の `awaiting_human` 確認は、
  他の fixture と同じ「`--until-idle` で DB を作ってから通常起動」のパターンで問題ない。

### D3. `in_flight` の経過時間、`cooldown` の残り時間、`last_tick_at` の遅延判定は GUI 側で計算する（派生値の再計算の禁止には当たらない）

- `docs/taskd-api-v1.md` §3.20 は「`last_tick_at` が `now` から `3 × tick_ms` 以上古ければ **GUI は**「ディスパッチャが遅延」と表示する（API は判定しない）」と明記しており、
  時刻の単純な差分表示は taskd 側が意図的に GUI に委ねている（判断ロジックの再実装ではなく表示整形）。同じ理由で:
  - `in_flight[].since` と `DaemonView.now` の差分を経過時間として表示する。
  - `cooldown.until` と `DaemonView.now` / `Providers` 取得時刻の差分を残り時間として表示する（`until` の値自体はスナップショット由来のままで、GUI は減算するだけ）。
  - いずれも taskd の状態遷移や承認可否には使わない（表示のみ）。

### D4. `multi-account` フィクスチャの構成（taskd 本体の e2e テスト `throttled_account_falls_back_to_the_next_account` を移植）

- `test/taskd/fixtures/multi-account-worker.sh`: `$ACCOUNT` が `a` なら常に `{"type":"error","message":"429 rate limited","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":300}}`
  （`usage` は付けない。ワーカープロトコルの `Error` メッセージに `usage` フィールドは無い — `crates/task-worker/src/protocol.rs` の `WorkerMessage::Error` を確認）、
  `b` なら `account.txt` を書いて `{"type":"done","summary":"...","evidence":[],"usage":{"input_tokens":120,"output_tokens":40}}`。
- `test/taskd/multi-account.toml.tmpl`: `[[providers]]` を `acct-a`（`concurrency=1`, `env={ACCOUNT="a"}`）/ `acct-b`（同 `b`）の 2 本にする（taskd の `TWO_ACCOUNTS` 定数と同じ形）。
  `retry_backoff_base_secs = 0`、`max_requeues` は既定のままで、300 秒の cooldown 中に acct-b へフォールバックする（taskd 側の e2e と同じ挙動）。
- `scripts/taskd.sh fixture multi-account`: 上記 toml を `.run/multi-account/taskd.toml` に展開し、workspace を用意するだけ（D1 のとおり DB には触らない）。
- 受け入れ条件 1 の「acct-a が requeue 1、acct-b が done 1」は `ProviderStats.requeue` / `.done`（`GET /providers`）に直接出る。tokens 合計は acct-a 側が 0
  （`Error` に `usage` が無いため）、acct-b 側が `usage` の値のみなので、`GET /tasks/<id>/runs` の 2 run の `usage` の和と一致する。

### D5. `fixture (b)`（受け入れ条件 2 の awaiting_human）は既存の `basic` を流用し、新しい fixture は追加しない

- Human-B（`accept: "someone signs off"` の Human 条件）が `reviewing` のまま `awaiting_human` に入ることは D2 で確認済み。G4 用に別の fixture を作る必要は無い。

### D6. 「遅い fake ワーカー」（受け入れ条件 4）は `basic` の worker script に新しい分岐を追加し、e2e が都度 `taskctl add`/`approve` する

- `test/taskd/fixtures/basic-worker.sh` に `Slow-H`（20 秒 sleep してから done）の分岐を追加する。fixture 本体（`--until-idle`）には含めない
  （含めると fixture 構築が 20 秒余計にかかり、かつ `--until-idle` で完了させてしまうと in_flight を観測できない。ADR-0006 D6 と同じ理由）。
  `e2e/g4.spec.ts` が `basic` の生きたプロセスに対して直接 `add`/`approve` する。

### D7. `/providers` 画面と `/daemon` 画面は生成型をそのまま表で出す（派生の集計はしない）

- `Providers.items[]`（`ProviderView` = 定義 + `in_use` + `cooldown` + `ProviderStats`）をそのまま表にする。`ProviderStats` は既に taskd 側で
  `done`/`error`/`requeue`/`question`/`lease_expired`/`runs`/`input_tokens`/`output_tokens`/`by_day` を持っているので、GUI 側で件数やトークンを合算・分類し直さない。
- `/daemon` は G2 で作った最小限の版（`app/routes/daemon.tsx`）を拡張する: `in_flight`（task へのリンク、run_id、provider、D3 の経過時間）、`cooldowns`（provider、reason、
  D3 の残り時間）、`awaiting_human` / `unroutable`（task へのリンクの一覧）、D3 の遅延バナーを追加する。`replay` は G2 のまま変更しない。

### D8. taskd 停止/復旧バナー（受け入れ条件 3）は新規実装なしで検証する

- G0/G1 で作った root loader の `GET /health` 失敗時バナー（5 秒ごとに再検証）と `EventSource` の自動再接続は `/providers` / `/daemon` にもそのまま効く
  （child route の loader も同じ `TaskdUnavailable` を `taskdErrorResponse()` で投げ、`ErrorBoundary` が同じバナーを出す規約に従うだけ）。G4 では
  `e2e/g4.spec.ts` に検証シナリオを追加するだけで、`app/root.tsx` 自体の変更は不要と見込む（実装時に不足が見つかれば追記する）。

## 3. 影響

- `scripts/taskd.sh`: `cmd_fixture` に `multi-account` / `unroutable` を追加。`multi-account` は他の fixture と違い DB を作らない「準備のみ」コマンドになる
  （`fixture basic` 等と挙動が異なる点を `scripts/taskd.sh` のコメントに明記する）。
- `test/taskd/fixtures/basic-worker.sh`: `Slow-H` 分岐を追加（既存の分岐には触れない）。
- 新規: `test/taskd/multi-account.toml.tmpl`、`test/taskd/fixtures/multi-account-worker.sh`、`test/taskd/unroutable.toml.tmpl`（または `fixture_unroutable` 内でヒアドキュメント）。
- 新規ルート: `app/routes/providers.tsx`（`/providers`）。既存 `app/routes/daemon.tsx` を拡張。`app/routes.ts` に `/providers` を追加。
- `e2e/g4.spec.ts`: 受け入れ条件 1〜4 の 4 シナリオ。`multi-account` の生きたプロセスを維持したまま複数の受け入れ条件を検証する点が他フェーズと異なる。
