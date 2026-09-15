# ADR-0017: GUI からのアカウント管理

- 日付: 2026-09-15
- 状態: **Proposed**（設計のみ。実装は人間の判断を待つ）
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
