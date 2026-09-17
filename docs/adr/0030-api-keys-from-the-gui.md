# ADR-0030: API キーを GUI から預かる（`[secrets]` と `env_from_secrets`）

- 日付: 2026-09-17
- 状態: **Accepted**（人間の依頼「検索エンジンとして Tavily や Exa の API Key を用意してくるので、その間に API Key を GUI から渡せるようにしておいてください。
  アカウントのカテゴリで大丈夫かと思います」）
- 関連: ADR-0017（管理 API。「API キーを GUI から入力して保存する」を**採らない**としていた）、ADR-0024 / 0025（アカウント）、ADR-0029（LDR の検索エンジン）

## 1. 文脈

ADR-0029 で分かったとおり、このホストからは鍵なしの一般 Web 検索が実用にならない。Tavily / Exa のような鍵付き API を使うには、
鍵を taskd のワーカーに渡す必要がある。今の手段は `[[providers]].env` に**平文で書く**か、taskd の環境変数に入れるかで、
どちらも設定ファイルが秘密を持つ（ADR-0012 D1 で「推奨しない」としていた形）。

人間の依頼により、**ADR-0017 の「API キーを GUI から入力して保存しない」を上書きする**。置き場所は taskd が持ち、
GUI は「アカウント」画面の一区画として扱う（人間の指定）。

### 実機で確かめた事実（2026-09-17）

- Local Deep Research の設定は `LDR_` + 設定キーを大文字化した環境変数で上書きできる（`env_settings.py`:
  `self.env_var = "LDR_" + key.upper().replace(".", "_")`）。つまり
  **Tavily = `LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY`**、**Exa = `LDR_SEARCH_ENGINE_WEB_EXA_API_KEY`**。
- したがって「鍵を環境変数として run に渡す」仕組みがあれば、LDR だけでなく他のアダプタ（`ANTHROPIC_API_KEY` 等）にも同じ形で使える。

## 2. 決定

### D1. 秘密は taskd が `[secrets]` の下にファイルで持つ（1 秘密 = 1 ファイル）

```toml
[secrets]
dir = "secrets"    # 相対なら設定ファイル基準。0700 で作る。1 ファイル 1 秘密（0600）
```

- ファイル名 = 秘密の id（`^[A-Za-z0-9_-]{1,64}$`）。中身は値 1 行（末尾の改行は落とす）。**値はどこにも出さない**。
- `[secrets]` を書かない構成では、この機能は無効（管理 API は 409 `secrets_unavailable`）。

### D2. 使い方は「環境変数への流し込み」（`env_from_secrets`）

```toml
[adapters.local_deep_research]
env_from_secrets = { LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily", LDR_SEARCH_ENGINE_WEB_EXA_API_KEY = "exa" }

[[providers]]
id = "ldr-tavily"
adapter = "local-deep-research"
env_from_secrets = { LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily" }   # 行ごとの上書きも同じ形
```

- 値は**アダプタを組み立てるとき**に読む（`build_adapters`）。したがって鍵を入れ替えたら `POST /reload`（GUI は保存後に自動で呼ぶ）。
- 優先順は既存の env と同じ: taskd の環境 < `[adapters.*].env` < `[adapters.*].env_from_secrets` < 行の `env` < 行の `env_from_secrets`。
- **秘密が見つからないのは設定エラーにしない**（鍵を入れる前に設定だけ先に書けるように）。warn を出してその環境変数を渡さず、
  run はワーカー自身のエラー（鍵が無い）で失敗する。`GET /secrets` の `used_by` で「設定はあるが鍵が無い」が分かるようにする。
- `[[providers]].env` に平文で書く従来の方法も残す（既存の設定を壊さない）。

### D3. API（すべて管理系。`token_file` 未設定でも 401。ADR-0017 M3）

| エンドポイント | 内容 |
|---|---|
| `GET /secrets` | `{dir, items: [{id, updated_at, fingerprint, used_by: [{scope: "adapter"\|"provider", name, env}]}]}`。**値は返さない** |
| `PUT /secrets/{id}` `{value}` | 作成 / 置き換え（0600）→ 200 `{id, updated_at, fingerprint}`。空文字は 422 |
| `DELETE /secrets/{id}` | 削除 → 200 `{}`。無ければ 404 `secret_not_found` |

- `fingerprint` = 値の sha256 の先頭 8 桁（人が「入れ替えた鍵が届いているか」を確かめるため。値そのものは復元できない）。
- `used_by` は**設定から導く**（どの `env_from_secrets` がその id を指しているか）。GUI が再計算しない。
- **値はログにも応答にも出さない**（`who = "admin"`, `op`, `secret_id` だけ記録する。ADR-0024 D5 と同じ規律）。

### D4. GUI は「アカウント」画面の一区画（人間の指定）

- `/accounts` に **「API キー」** の節を足す: 一覧（id・使われている環境変数・更新時刻・fingerprint）、追加 / 更新（id と値。
  値の入力は `type="password"`・`autocomplete="off"`・**保存後は二度と表示されない**旨の注意）、削除（確認付き）。
- 保存・削除の後は GUI の action が続けて `POST /reload` を呼ぶ（プロバイダ管理と同じ作り。ADR-GUI-0012 D2）。
- **注意書き**: 値は平文 HTTP を通る（GUI は LAN で平文。ADR-0024 D7 と同じ注意）。信頼できるネットワークでだけ使う。

## 3. 採らない

- 鍵の暗号化保存（OS のキーチェーン等）。単一利用者・信頼されたホストという前提（ADR-0022 の運用前提）で、
  ファイル権限（0600）と管理 API のトークンで守る。暗号化するなら鍵の置き場所の問題が再発する。
- 値の読み出し API（`GET /secrets/{id}` で値を返す）。書き込みと削除だけにする。
- `taskctl` からの秘密の登録（今回は GUI と API のみ。必要なら後で足す）。

## 4. 受け入れ条件（Phase 20）

1. `[secrets] dir` を設定でき、`PUT /secrets/{id}` がファイルを 0600 で作り、`GET /secrets` が値を返さずに
   id・更新時刻・fingerprint・`used_by` を返す。`DELETE` で消える。管理系はトークン無しで 401。
2. `env_from_secrets` がアダプタと行の両方で効き、優先順が D2 のとおり（`build_adapters` のテスト）。
   秘密が無いときは warn だけで設定エラーにしない。
3. 値がログ・API 応答・`GET /config` のどこにも出ない（テストで確認）。
4. GUI の「アカウント」画面から API キーを追加・更新・削除でき、保存後に `reload` が走る。
5. 実機: Tavily か Exa の鍵を GUI から入れ、`search.tool` をそのエンジンにした `web-research` のタスクが `done` になる
   （**鍵は人間が用意する**ので、鍵が届くまではここだけ保留）。
6. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の検査一式。
