# ADR-0024: Claude アカウントのプール（`CLAUDE_SECURESTORAGE_CONFIG_DIR`）、残量に基づく負荷分散、GUI からの登録とログイン

- 日付: 2026-09-16
- 状態: **Accepted**（人間の依頼 2026-09-16:「GUI からプロバイダを登録できるようにして下さい。また、CLAUDE_SECURESTORAGE_CONFIG_DIR 環境変数を使って
  このマシンで二つ以上の claude アカウントをアクセスできるようにし、プロバイダはそれらから自動で claude code のエージェントセッションで使用する
  プロバイダの認証情報を選択し、それぞれのアカウントのバジェットの残量からいい感じにロードバランスするようにして下さい」）
- 関連: ADR-0012（複数アカウント）、ADR-0017（管理 API）、ADR-0022（管理画面は作らない・監視しない）、ADR-0013 D4 / D11、DESIGN §5.4 / §5.5 / §6 非目標

## 文脈

### 以前の決定との関係（この ADR が上書きするもの）

| 以前の決定 | 今回 |
|---|---|
| ADR-0022 D1「GUI のアカウント管理画面は作らない」 | **上書き**。GUI からプロバイダの追加・変更・削除、アカウントの追加・ログイン・確認・削除を行う |
| ADR-0017 D2「ログインの対話は肩代わりしない」 | **上書き**。`claude auth login` を taskd が子プロセスとして起動し、URL の表示と認可コードの受け渡しだけを中継する（OAuth は実装しない） |
| ADR-0017 D3 / DESIGN §6 非目標「残量推定に基づく複数アカウントの自動切替」、CLAUDE.md「予算管理の実装（別プロジェクト）」 | **人間の指示で範囲に入れる**。ただし推定はしない: Claude Code 自身が出す実測値（下記）だけを使う。課金額の予算管理（コスト上限）は引き続き範囲外 |

DESIGN.md の該当箇所（§6 非目標、§5.5）への反映は提案として PROGRESS.md に書く（P-62）。本 ADR が採用されるまでの実装の根拠はこの ADR。

### 事実（2026-09-16 にこのホストで確認）

- `claude` 2.1.273 のバイナリは `CLAUDE_SECURESTORAGE_CONFIG_DIR` を読む。指定したディレクトリには `.credentials.json`（600）だけが作られ、
  設定（`~/.claude`、`~/.claude.json`）は共有のまま。`/status` のアカウント表示は切り替わらないが、認証・レート制限・プランは切り替わる（人間の確認）。
- `claude -p ... --output-format stream-json --verbose` は、最初の応答の後に次の行を出す（ヘッドレスで取れる。status line は不要）:
  ```json
  {"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1789605600,"rateLimitType":"five_hour",
   "overageStatus":"rejected","isUsingOverage":false,
   "unifiedWindows":{"five_hour":{"utilization":0.14,"resetsAt":1789605600},"seven_day":{"utilization":0.24,"resetsAt":1790031600}}}}
  ```
  `utilization` は 0〜1、`resetsAt` は Unix 秒。枠が欠けることがある。値はその応答時点のスナップショット。
- `claude auth login`（`CLAUDE_SECURESTORAGE_CONFIG_DIR` 付き）は、`If the browser didn't open, visit: <URL>` を出して
  `Paste code here if prompted >` で **標準入力から 1 行**読む（パイプでも読む。誤ったコードでは `Login failed: ... 400` で exit 1）。
  URL は OSC 8 のエスケープで囲まれて出る。認可後のページに表示されるコードを貼ると `.credentials.json` が作られる。

## 決定

### D1. アカウント = `accounts_dir` の下の 1 ディレクトリ

```toml
[accounts]
claude_dir = "claude-accounts"      # 相対なら設定ファイル基準。各サブディレクトリ <id>/ が 1 アカウント
max_runs_per_account = 2            # 1 アカウントで同時に走らせる run の上限（既定 2）
check_model = "haiku"               # 確認（D6）に使うモデル。枠はアカウント単位なので最も安いモデルでよい
```

- `<claude_dir>/<id>/` がアカウント。`id` は `^[A-Za-z0-9_-]{1,64}$`（プロバイダ ID と同じ規則）。`.` で始まる名前は予約（`.removed/` 等）。
- `.credentials.json` があれば **ログイン済み**、無ければ **未ログイン**（選ばない）。中身は読まない・出さない。
- アカウントの一覧は **選択のたびに（tick につき高々 1 回）ディレクトリを読む**。ログインや追加の後に reload は要らない。
  これは ADR-0022 D4 の「設定の監視」ではない（設定ファイルは読み直さない。アカウントの有無と認証ファイルの有無を見るだけ）。
- 秘密は API に出さない: トークンの中身は読まない。`dir`（パス）は出す（ログイン手順に要る）。

### D2. プールを使うプロバイダ

```toml
[[providers]]
id = "claude-pool"
adapter = "claude-code"
tiers = ["frontier", "standard", "cheap"]
concurrency = 4                      # プロバイダ全体の上限（従来どおり）
account_pool = true                  # 追加（既定 false）。claude-code でだけ有効
```

- `account_pool = true` のプロバイダの run は、D3 で選んだアカウントの `CLAUDE_SECURESTORAGE_CONFIG_DIR=<claude_dir>/<id>` を
  **環境の最後に重ねて**起動する（taskd の環境 < `[adapters.claude_code].env` < `[[providers]].env` < アカウント）。
- 検証: `account_pool = true` は `adapter = "claude-code"` と `[accounts]` を要求する（設定エラー）。
- 選べるアカウントが無い（全て未ログイン・上限・cooldown・枯渇）なら、そのプロバイダは**満杯**として扱い、次のプロバイダへ
  フォールバックする（ADR-0012 D2 の除外集合）。行き先が無ければ Busy（待つ）。
- 実装: `WorkerAdapter` に既定実装付き `fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>>`（既定 `None`）。
  claude-code は env を足した複製を返す。Reviewer run（ADR-0010 D9）も同じ経路で選ぶ。
- `taskctl worker run --provider <pool>` は `--account <id>` を受け取る（省略時は D3 と同じ選び方、ただし観測値はファイルから読むだけ）。

### D3. 残量に基づく選択（決定的。LLM を呼ばない）

観測値（D4）から、各アカウントについて次を計算する。`now` は壁時計の Unix 秒。

1. 窓 `w ∈ {five_hour(18000 s), seven_day(604800 s)}` の実効使用率 `u_w` = 観測があり `now < resetsAt` ならその `utilization`、それ以外 0。
2. **除外**: 未ログイン / `in_use ≥ max_runs_per_account` / アカウントの cooldown 中 / `u_5h ≥ 0.97` / `u_7d ≥ 0.97` /
   直近の `status = "rejected"` でその `resetsAt` 前。
3. **スコア**（大きいほど良い）:
   - `h_5h = 1 − u_5h`
   - `h_7d = min(1, (1 − u_7d) / max(t_7d, 0.1))`（`t_7d` = 週次枠のリセットまでの残り時間 / 7 日。週の残り時間に対して多く残っているほど高い。
     週の終わり近くで余っている枠は使い切ってよい）
   - `score = min(h_5h, h_7d) − 0.05 × in_use`（観測は遅れるので、走っている run の分を割り引く）
   - 観測が一度も無いアカウントは `score = 1 − 0.05 × in_use`（まず使って観測を得る）
4. `score` 最大を選ぶ。同点は `in_use` の少ない方、次に `id` の昇順。

### D4. 観測値の取得と保存（`AccountBook`）

- claude-code アダプタは `rate_limit_event` を解析し、`EventSink::rate_limit(RateLimitObservation)`（既定 no-op）に渡す。
  ディスパッチャはプールのアカウントで走っている run のシンクから、その値を `AccountBook` に記録する（run の途中でも更新される）。
- run の結果が `AuthFailed` → そのアカウントを `error_cooldown_secs` の cooldown にし `auth_failed` を記録（GUI に「再ログインが必要」）。
  `Throttled` / `Exhausted` → 観測に `resetsAt` があれば最も遅い（使い切った）窓の `resetsAt` まで、無ければ `retry_after` / `error_cooldown_secs` まで cooldown。
  **プール経由の run の失敗はプロバイダを cooldown にしない**（1 アカウントの枯渇でプール全体を止めない）。requeue の扱いは従来どおり。
- 保存: `<claude_dir>/.taskd-usage.json` に観測値と cooldown（Unix 秒）を書く（更新のたび、一時ファイル + rename）。起動時に読む。
  **タスクの真実（DB・イベント）ではない観測値**なので replay の対象外。ファイルが壊れていたら無視して空から始める（warn）。
- `Event::WorkerStarted` に任意フィールド `account: Option<String>` を追加（どのアカウントで走ったかの記録。ADR-0012 D1 の `provider` と同じ扱い。
  replay は `WorkerStarted` を読まないので影響しない。`SCHEMA_VERSION` は変えない）。

### D5. API（task-api。管理系はすべて ADR-0017 M3 のとおりトークン必須）

| エンドポイント | 権限 | 内容 |
|---|---|---|
| `GET /accounts` | 読み取り | `AccountList{root: string\|null, max_runs_per_account, items: AccountView[]}`。`[accounts]` 未設定なら `root: null, items: []` |
| `POST /accounts` `{id}` | 管理 | ディレクトリを 0700 で作る → 201 `AccountView`。既存 409 `account_exists`、`[accounts]` 未設定 409 `accounts_unavailable` |
| `DELETE /accounts/{id}` | 管理 | `<claude_dir>/.removed/<id>-<unix秒>` へ移動（消さない）。`in_use > 0` なら 409 `account_in_use` → 200 `{}` |
| `POST /accounts/{id}/check` | 管理 | D6 → 200 `AccountCheckResponse{result, checked_at, detail?, usage?}` |
| `POST /accounts/{id}/login` | 管理 | D7 を開始 → 200 `AccountLoginStart{url, expires_at}`。既に進行中なら古い方を止めて新しく始める |
| `POST /accounts/{id}/login/code` `{code}` | 管理 | D7 のコードを渡す → 200 `AccountLoginResult{result: "ok"\|"failed", detail?}`。進行中でなければ 409 `login_not_started` |
| `DELETE /accounts/{id}/login` | 管理 | 進行中のログインを止める → 200 `{}` |

```
AccountView {
  id, dir, logged_in: bool, in_use: u32,
  usage?: { five_hour?: {utilization, resets_at}, seven_day?: {utilization, resets_at}, status?: string, observed_at, source: "run"|"check" },
  score?: f64,                       // D3 のスコア（除外なら無し）
  excluded_reason?: "not_logged_in"|"at_capacity"|"cooldown"|"five_hour_exhausted"|"seven_day_exhausted"|"rejected",
  cooldown?: {until, reason: "auth_failed"|"throttled"|"exhausted"},
  last_check?: {at, result, detail?},
  login_pending: bool,
  stats: {runs, done, error, input_tokens, output_tokens}   // WorkerStarted.account と WorkerFinished から集計（ProviderStats と同じ作り）
}
```

- `ProviderConfigView` / `ProviderView` / `ProviderCreateBody` / `ProviderPatchBody` / `providers.d` のファイルに `account_pool: bool` を追加。
- `DaemonSnapshot.accounts: Vec<AccountLive>`（serde default）を足し、`GET /accounts` はスナップショットの値（in_use・usage・score・cooldown・last_check・login_pending）と
  ディレクトリの実在（logged_in）と集計（stats）を合わせる。
- `AdminRequest` に `AccountCheck{id, reply}` / `AccountLoginStart{id, reply}` / `AccountLoginCode{id, code, reply}` / `AccountLoginCancel{id, reply}` を足す
  （ワーカー・子プロセスの起動は taskd 側。DESIGN §5.10 / ADR-0017 M2 と同じ境界）。
- ログには操作とアカウント id だけを出す（`who = "admin"`）。**認可コード・URL の `code_challenge`/`state`・トークンは出さない**。

### D6. アカウントの確認（手動のときだけ。ADR-0022 D3 を維持）

`claude -p "Reply with exactly: ok" --output-format stream-json --verbose --max-turns 1 --no-session-persistence --model <check_model>`
をそのアカウントの env で 60 秒まで実行し、`rate_limit_event` を `AccountBook` に `source = "check"` で記録する。
結果は `ok`（result 行が来た）/ `auth_failed` / `throttled` / `spawn_failed`（ADR-0022 の分類器を使う）。定期実行はしない（枠を食うため）。

### D7. GUI からのログイン（`claude auth login` の中継）

1. `login` で `CLAUDE_SECURESTORAGE_CONFIG_DIR=<dir> BROWSER=/bin/true claude auth login` を stdin パイプ付きで起動（`[adapters.claude_code].command` を使う）。
2. 標準出力から `https://…/oauth/authorize?…` を取り出す（OSC 8 等のエスケープを除く。15 秒以内に出なければ `failed`）。
   GUI はそれをリンクとして出す。人は自分のブラウザで開いて認可し、表示されたコードを GUI に貼る。
3. `login/code` でコード + 改行を stdin に書き、30 秒以内の終了を待つ。exit 0 かつ `.credentials.json` ができたら `ok`、それ以外は `failed`（`detail` は出力の最後の 1 行、200 文字まで。URL は伏せる）。
4. 進行中のログインは 10 分で打ち切る（子を kill）。taskd の終了時も kill する。同時に進行できるのはアカウントごとに 1 つ。

GUI は平文 HTTP で LAN に出しうる（パスワード認証付き）。認可コードは 1 回限り・短命だが、信頼できるネットワークでのみ使う旨を画面と README に書く。

### D8. GUI

- `/providers`: 「プロバイダを追加」フォーム（id・adapter・tiers・concurrency・model・`account_pool`・env（KEY=VALUE 行。値は表示しない））、各カードに「編集」「削除」「疎通確認」。
  追加・変更・削除の後は GUI の action が続けて `POST /reload` を呼ぶ。
- `/accounts`（新規、ナビ「運用」に追加）: アカウントのカード（ログイン状態、5 時間枠・週次枠の使用率バーとリセット時刻、スコアと除外理由、実行中、cooldown、最後の確認、集計）、
  「アカウントを追加」、「ログイン」（URL を開く → コードを貼る）、「残量を確認」、「削除」。
- 管理 API は GUI の BFF が `TASKD_API_TOKEN_FILE` のトークンで呼ぶ。トークンが無い構成で 401 が返ったら、設定方法を画面に出す（GUI 側で回避しない）。

## 採らない

- `/api/oauth/usage` 等の非公開エンドポイントのポーリング（仕様外。枠の値は Claude Code が stream-json で出すものだけを使う）。
- `~/.claude/.credentials.json` をプールへ複製する（リフレッシュトークンの回転でどちらかが無効になる）。プールには各アカウントで新しくログインする。
- 残量の推定・外挿（観測値の最新だけを使う）。課金額の予算（コスト上限）。

## 受け入れ条件（Phase 13）

1. `[accounts]` と `account_pool = true` のプロバイダで、観測値の異なる 2 アカウントがあるとき、スコアの高い方に run が割り当てられ、
   `WorkerStarted.account` とワーカーの `CLAUDE_SECURESTORAGE_CONFIG_DIR` が一致する（スタブの claude で e2e）。
2. 片方が `throttled` で終わると、そのアカウントだけが cooldown になり、次の run はもう片方に行く。プロバイダは cooldown にならない。
3. `rate_limit_event` が run の途中で `AccountBook` と `GET /accounts` に反映され、taskd を再起動しても残る。
4. `POST /accounts` → `login` → `login/code` → `logged_in: true` がスタブの `claude auth login` で通る。管理系はトークン無しで 401。
5. GUI からプロバイダの追加・変更・削除（reload まで）とアカウントの追加・ログイン・確認・削除ができる（Playwright）。
6. `cargo test --workspace` / `cargo clippy --workspace -- -D warnings` / GUI の lint・typecheck・test・build・e2e・gen:types 差分ゼロ。
7. 実機: このホストの実アカウントで `check` が `rate_limit_event` を記録する（ログインは人が GUI から行う）。

## 実装メモ（Phase 13 の実装と監査で決めた細部。2026-09-16）

- **M1（`rejected` で `resetsAt` が無い観測）**: D3 の「`status = "rejected"` でその `resetsAt` 前」は `resetsAt` が無いと判定できない。
  その場合は観測から 5 時間（`FIVE_HOUR_SECS`）だけ除外する（短い枠より長く止めない。次の観測で上書きされる）。
- **M2（`t_7d` の上限）**: `max(t_7d, 0.1)` だけを取り、上限では切らない（時計のずれで 7 日超になっても `min(1, …)` で頭打ちになる）。
- **M3（アカウントの cooldown の理由）**: `auth_failed` は `throttled` / `exhausted` より優先して理由に残す（期限は遅い方）。GUI の「再ログインが必要」を隠さないため。
- **M4（起動失敗）**: プール経由の run でも、CLI が起動できない（`Spawn`）のはアカウントの問題ではないので、**プロバイダ**を cooldown にする（従来どおり）。
  アカウントを cooldown にするのは `AuthFailed` / `Throttled` / `Exhausted` だけ。
- **M5（プールの失敗とイベント）**: プール経由でアカウントを cooldown にした失敗は `ProviderThrottled` を追記しない（プロバイダは cooldown にならないため）。
  記録は `WorkerFinished`（outcome・requeue の遷移理由）と `.taskd-usage.json` の cooldown に残る。どのアカウントだったかは同じ run の `WorkerStarted.account` で分かる。
- **M6（削除は taskd 側）**: `DELETE /accounts/{id}` は管理チャネルで taskd に渡し、ディスパッチャの実際の `in_use` で判定してから `.removed/` へ移し、
  観測値の記録も消す（スナップショットの 1 tick 遅れによる競合と、同じ id を作り直したときの古い記録の引き継ぎを避ける）。
- **M7（`[accounts]` と reload）**: `[accounts]` の変更は reload では反映しない（`POST /reload` が 400 で「再起動が必要」を返す）。
  プールのプロバイダの追加・変更は reload で反映される。
