# ADR-0017: GUI からのアカウント管理

- 日付: 2026-09-15
- 状態: **Accepted**（Phase 11 実装済み。D1〜D4 を実装、細部は末尾の「実装メモ」M1〜M5 参照）
- 関連: ADR-0012（複数アカウント）、ADR-0013 D11（秘密を出さない）、ADR-0015（観測可能性）、DESIGN §5.5 / §6 Phase 11

## 文脈

現状できているのは「使う側」だけ。

- `[[providers]]` の各行が 1 アカウント。並列度の上限に達した行・cooldown 中の行を飛ばして次の行へ決定的にフォールバックする。
- GUI のプロバイダ画面に run 数・done / error / requeue・トークン使用量・cooldown の残り・`env` のキー名が出る。

足りないのは「増やす・保守する」側。

- アカウントの追加は、設定ファイルを手で編集し、`CLAUDE_CONFIG_DIR=... claude` で `/login` してから taskd を再起動する手作業。
- ログイン済みかどうかは、実際に run を投げて `auth_failed` で落ちるまで分からない。
- 残量（週次上限までどれだけ使ったか）は Claude Code / codex が API で出さないので、taskd からは観測できない。

## 決定（案）

### D1. アカウントの定義は設定ファイルが真実。API は「提案 → 再読込」

- `POST /api/v1/providers`（追加）/ `PATCH`（並列度・tier・model の変更）/ `DELETE` は、**`taskd.toml` を書き換えず**、
  `providers.d/<id>.toml` に 1 ファイルずつ書き、`[providers] include = "providers.d/*.toml"` で読み込む形にする。
- 反映は `POST /api/v1/reload`（または SIGHUP）で、**次の tick から**。実行中の run には影響しない。
  cooldown はメモリなので再読込で消える（`StaticPolicy` を作り直すため。ADR-0012 の観測値の扱いと同じ）。
- 書き込みは API の権限のうち「管理」に限る（`token_file` 必須。loopback でも管理操作にはトークンを要求する）。

### D2. ログインは「手順の案内」+ 「状態の確認」まで（対話は肩代わりしない）

- `claude` / `codex` のログインは対話が要る（ブラウザまたは端末）。**GUI が肩代わりしない**。
- GUI は次を行う:
  1. アカウント追加時に `CLAUDE_CONFIG_DIR` / `CODEX_HOME` のディレクトリを作り、**実行すべきコマンドをそのまま表示**する（コピーできる形）。
  2. `POST /api/v1/providers/{id}/check` で疎通確認: そのアカウントの env で `claude -p "ok"` 相当を 1 回だけ、短い制限（30 秒 / 1 ターン）で実行し、
     `ok` / `auth_failed` / `throttled` / `spawn_failed` を返す。結果は `Event` にしない（タスクに紐づかない観測値）。
  3. プロバイダ画面に「最後の確認時刻と結果」を出す。
- 秘密は API に出さない（ADR-0013 D11）。`env` は**キー名だけ**、`token_file` の中身とパスは出さない。

### D3. 残量の推定はしない（非目標のまま）

- 週次上限・課金の残りは提供元が出さないので推定しない。GUI に出すのは**実測の使用量**（`WorkerFinished.usage` の合計）と cooldown の履歴だけ。
- 「使い切ったら次のアカウント」は現状の決定的フォールバックで足りる（`Throttled` / `Exhausted` → cooldown → 次の行）。

### D4. 監査

- 管理操作（追加・変更・削除・再読込・疎通確認）は `taskd` のログに `who`（トークンの識別子ではなく `"admin"` 固定）と操作内容を残す。
  タスクのイベント列（真実の系列）には混ぜない。

## 採らない

- GUI がブラウザを開いて OAuth を代行する。
- API キーを GUI から入力して保存する（鍵は `CLAUDE_CONFIG_DIR` / `CODEX_HOME` の中に置く方式を維持）。
- 残量推定に基づく自動切替（DESIGN §6 非目標のまま。供給層の担当）。

## 影響

- 設定: `[providers] include` の追加、`providers.d/`。API: 管理系エンドポイント 5 本と `reload`。
- 受け入れ条件は Phase 11（DESIGN §6）に書く。

## 実装メモ（Phase 11、2026-09-15）

D1〜D4 の決定は変えていない。実装時に決めた細部:

- **M1（TOML キー名の変更）**: `[providers] include = "providers.d/*.toml"` は文字どおりには実装しなかった。
  TOML は同じキー `providers` を配列テーブル（`[[providers]]`）と単純テーブル（`[providers]`）の両方には束縛できないため、
  文字どおりの構文は既存の `[[providers]]`（23 ファイルが使用: 全 example config、e2e シナリオ、`taskctl worker run` のテスト等）を
  破壊的に置き換える必要があった。代わりにトップレベルのフラットな任意キー `providers_include: Option<String>`
  （末尾が `/*.toml` である glob 文字列。既定 `None`）を追加し、`[[providers]]` は無変更のまま両立させた。
  `Config::load` は `providers_include` があれば、そのディレクトリの `*.toml` をファイル名昇順で読み、各ファイルを
  1 件の `ProviderConfig`（`[[providers]]` の 1 行と同じ形）として `deny_unknown_fields` で解析し、
  `cfg.providers` に追記してから既存の `validate()`（重複 id・アダプタ種別・concurrency）を通す。
- **M2（管理 API の実行境界）**: DESIGN §5.10 は task-api に「LLM 呼び出し・ワーカー起動・`Check::Command` 実行をしない」ことを求めている。
  `POST/PATCH/DELETE /api/v1/providers...` は `providers.d/<id>.toml` へのファイル読み書きだけなので task-api 内で完結させた
  （ワーカーもLLMも起動しない）。`POST /api/v1/reload` と `POST /api/v1/providers/{id}/check` は、それぞれ「稼働中の
  `Dispatcher` の再構築」と「実際に 1 回ワーカーを起動する疎通確認」で、どちらも task-worker/task-dispatch への依存が要る
  （task-api の `Cargo.toml` に両クレートへの依存は追加していない）。そこで task-api に `AdminRequest`
  （`Reload{reply}` / `Check{provider_id, reply}`、`tokio::mpsc` + `oneshot`）を新設し、`ApiSettings.admin_tx` 経由で
  taskd（既に task-worker/task-dispatch に依存している）へ委譲する。taskd の `tick_loop` が `admin_rx` を
  `tokio::select!` の1腕として受け、`Reload` はその場で（`Config::load` の再読込→`StaticPolicy`/アダプタ/実効モデルの
  再構築→`Dispatcher::reload_providers` で差し替え）処理し、`Check` は tick をブロックしないよう `tokio::spawn` した
  タスクで処理する（`Config::load` を再読込し、対象 1 件だけの使い捨てアダプタを組み立て、`/tmp` 配下の使い捨て
  ワークスペースで 30 秒・1 ターンの合成タスクを実行し、結果を `ok`/`auth_failed`/`throttled`/`spawn_failed` に写す。
  タスク／イベントには残さない、D2 のとおり）。
- **M3（管理系のトークン必須）**: 既存の `guard` ミドルウェアは `token_file` 未設定（loopback 限定構成）なら全エンドポイントで
  認証をスキップする。5 本の管理エンドポイントだけは、ハンドラの先頭で `token_digest` の有無に関わらず bearer を検査する
  `require_admin` を呼び、`token_file` 未設定なら 401 にする（D1「loopback でも管理操作にはトークンを要求する」）。
  読み取り系（`GET /providers` 等）は既存のグローバル guard のままで無変更。
  運用上の含意: 管理 API を使いたい運用者は loopback 限定構成でも `[api].token_file` を設定する必要がある
  （`Config::validate()` では強制しない。管理 API を使わない運用は今までどおりトークン無しで動く）。
- **M4（`GET /providers` / `GET /config` の反映元）**: `reload` は `Dispatcher` の `policy`/`adapters`/`models` を
  差し替えるのに加えて、次の tick のスナップショットに乗る `ProviderLive` 一覧（`env_keys` を追加）も更新する
  （`Dispatcher::set_snapshot_providers`）。`GET /providers` と `GET /config` は、起動時に固定される
  `config_view.providers`（`ApiState.inner` は不変）ではなく、**スナップショットがあればそちらを優先**して
  プロバイダの id/adapter/tiers/concurrency/model/env_keys を組み立てるよう変更した。これにより `ApiState.inner` に
  可変状態（`Mutex`）を足さずに reload の結果が読み取り系に反映される（起動直後、最初の tick 前だけ `config_view` に
  フォールバック）。
- **M5（監査ログ）**: D4 のとおり、管理操作は `tracing::info!(who = "admin", op = ..., provider_id = ...)` で記録し、
  `env` の値や `token_file` の中身は一切ログに出さない。タスクの `Event` 列には混ぜない。
