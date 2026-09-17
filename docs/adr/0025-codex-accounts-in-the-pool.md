# ADR-0025: codex アカウントもプールに入れる（`CODEX_HOME`、デバイス認証、`token_count` の残量）

- 日付: 2026-09-16
- 状態: **Accepted**（人間の依頼「codex のアカウント追加方法も実装して下さい」）
- 関連: ADR-0024（Claude アカウントのプール。ここはその拡張）、ADR-0012 D1（`CODEX_HOME` でのアカウント分離）、ADR-0008（codex アダプタ）、ADR-0017 / 0022

## 文脈（このホストで確認した事実。codex-cli 0.154.0、2026-09-16）

- アカウントの分離は `CODEX_HOME`（ADR-0012 D1 のまま）。claude と違い**設定も履歴もこのディレクトリの下**（`auth.json` / `config.toml` / `log/` / `sessions/`）。
  ログイン済みかどうかは `auth.json` の有無で分かる（中身は読まない）。
- ログインは 2 通り。
  - `codex login`: ローカルの **1455 番ポート**にコールバックするブラウザ認証。別の PC のブラウザからは使えない。
  - `codex login --device-auth`: `https://auth.openai.com/codex/device` と**一回限りのコード**（`ABCD-EFGHI` の形、15 分で失効）を表示し、
    人が別のデバイスで入力し終わるのを待つ。**標準入力は使わない**。GUI にはこちらを使う。
- 残量は `codex exec --json` の `token_count` イベントに載る（`rate_limits.primary` / `.secondary`、各 `used_percent`（0〜100）・`window_minutes`・リセットまでの秒数）。
  ヘッドレスで取れる点は claude と同じ。
- 未ログインで `codex exec` すると `401 Unauthorized: Missing bearer or basic authentication` を 10 回ほど再試行して失敗する（約 40 秒）。

## 決定

### D1. アカウントはアダプタごとの根ディレクトリに分ける

```toml
[accounts]
claude_dir = "claude-accounts"   # ADR-0024。<claude_dir>/<id>/ = CLAUDE_SECURESTORAGE_CONFIG_DIR
codex_dir  = "codex-accounts"    # 追加。<codex_dir>/<id>/ = CODEX_HOME
max_runs_per_account = 2
check_model = "haiku"            # claude の確認に使うモデル（codex の確認はモデルを指定しない）
```

- アカウントは `(adapter, id)` で識別する。**id は同じでもアダプタが違えば別のアカウント**（`GET /accounts` の `items[]` は `adapter` を持つ）。
- `[accounts]` はどちらか一方だけでもよい。`account_pool = true` のプロバイダは、その `adapter` に対応する根ディレクトリを要求する（無ければ設定エラー）。
- ログイン済みの判定: claude-code は `.credentials.json`、codex は `auth.json`。どちらも存在だけを見る。
- 観測値の保存（`.taskd-usage.json`）はアダプタごとの根ディレクトリの下に置く（アカウントの記録はそのアダプタの中で閉じる）。

### D2. 選択と env

- 選び方（スコア・除外・同点の扱い）は ADR-0024 D3 のまま。アダプタが違えば候補集合が違うだけ。
- プールのアカウントで起動するとき、claude-code は `CLAUDE_SECURESTORAGE_CONFIG_DIR`、codex は `CODEX_HOME` を**環境の最後に重ねる**（`WorkerAdapter::with_env`。codex アダプタにも実装する）。
- `WorkerStarted.account` は id をそのまま記録する（どのアダプタかは同じイベントの `adapter` で分かる）。

### D3. 残量の観測（codex）

- codex アダプタは stream の各行から `rate_limits`（`token_count` イベント）を拾い、`RateLimitObservation` に写して `EventSink::rate_limit` に渡す。
- 写し方: `used_percent / 100` を `utilization` に、リセット時刻は「観測時刻 + リセットまでの秒数」（無ければ「観測時刻 + `window_minutes` × 60」）。
  枠の割り当ては `window_minutes` で決める: **1440 分以下なら 5 時間枠の位置**、それより長ければ**週次枠の位置**（`primary` / `secondary` という名前ではなく窓の長さで判断する）。
  これで ADR-0024 D3 の計算をそのまま使える（`five_hour` は「短い枠」、`seven_day` は「長い枠」として読む）。

### D4. 確認（`POST /accounts/{id}/check`）

- codex は `codex exec --json --skip-git-repo-check "Reply with exactly: ok"` を使い捨てディレクトリで 60 秒まで実行する（モデルは指定しない）。
- 出力に 401 の行が出たら**再試行を待たずに**打ち切って `auth_failed` にする（未ログインだと 40 秒ほど再試行するため）。
- `turn.completed` / 最終メッセージが来れば `ok`、分類できない失敗は `spawn_failed`。`rate_limits` があれば観測値として記録する（`source = "check"`）。

### D5. ログイン（`POST /accounts/{id}/login`）— アダプタで流儀が違う

| | claude-code（ADR-0024 D7） | codex（本 ADR） |
|---|---|---|
| コマンド | `claude auth login` | `codex login --device-auth` |
| 応答 | `{kind: "paste_code", url, expires_at}` | `{kind: "device_code", url, user_code, expires_at}` |
| 人の操作 | URL を開いて認可 → 表示されたコードを GUI に貼る | URL を開いて `user_code` を入力する（GUI には貼り戻さない） |
| `POST …/login/code` | コードを渡して完了 | **使わない**（409 `login_code_not_supported`） |
| 完了の判定 | コード送信の結果 | 子プロセスの終了を待つ（最大 15 分）。`auth.json` ができていれば成功 |
| 中止 | `DELETE …/login` | 同じ |

- `AccountLoginStart` に `kind`（`paste_code` / `device_code`）と `user_code`（codex のみ）を足す（追加のみ。v1 のまま）。
- GUI は `kind` を見て画面を変える。codex は完了を待つ間 `login_pending: true` のままで、成功すると `logged_in: true` になる（GUI は tick ごとの再検証で気づく）。
- **`user_code` はログに出さない**（URL も従来どおり出さない）。

### D6. API の後方互換

- `AccountList` に `roots: {"claude-code": string|null, "codex": string|null}` を足す。既存の `root` は **claude-code の根の別名**として残す（ADR-0024 で出したばかりの形を壊さない）。
- `POST /accounts` の本文に `adapter`（`"claude-code"` 既定 / `"codex"`）。`DELETE /accounts/{id}`・`check`・`login` は `?adapter=` を受け取る（省略時 `claude-code`）。
- `AccountView.adapter` を足す。`GET /accounts` は両方のアダプタのアカウントを `adapter` → `id` の順で並べて返す。

## 採らない

- `codex login`（1455 番ポートのコールバック）を GUI から使う。別の PC のブラウザでは完了できない。
- `codex login --with-api-key` を GUI から使う（API キーを GUI で受け取らない。ADR-0017「採らない」のまま）。
- codex の `config.toml` を taskd が書く（アカウントのディレクトリは codex のもの。taskd は作るだけ）。

## 受け入れ条件（Phase 14）

1. `[accounts] codex_dir` と `adapter = "codex"` の `account_pool` プロバイダで、残量の多い codex アカウントが選ばれ、`CODEX_HOME` がその値になる（スタブの codex で e2e）。
2. codex の `token_count` の `rate_limits` が観測値になり、`GET /accounts` に出る（短い枠 / 長い枠の割り当てが `window_minutes` に従う）。
3. `POST /accounts {"id":…, "adapter":"codex"}` → `login` が `kind: "device_code"` と `user_code` を返し、スタブの `codex login --device-auth` の完了後に `logged_in: true` になる。`login/code` は 409。
4. GUI からアダプタを選んでアカウントを追加し、codex はコード表示だけでログインできる（Playwright）。
5. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の lint・typecheck・test・build・e2e・gen:types 差分ゼロ。
6. 実機: このホストの本物の `codex` で、未ログインのアカウントの `check` が 401 を見て `auth_failed` になり、`login` が本物の URL と `user_code` を返す。
