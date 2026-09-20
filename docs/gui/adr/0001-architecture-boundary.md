# ADR-GUI-0001: GUI と celeris / SQLite の境界

- 日付: 2026-09-14（初版 Proposed → 同日 人間の決定で改訂）
- 状態: **Accepted**（人間の決定 H1 / H5。celeris 側の対応する決定は `docs/adr/0013-celeris-api-and-gui-foundations.md`）
- 関連: `docs/DESIGN.md` §1（原則 2, 5, 6）、§5.1、§6 非目標 / `docs/gui/DESIGN-GUI.md` §6, §8 / `docs/gui/api.md` / `docs/gui/celeris-proposals.md`

## 文脈

Web GUI は celeris リポジトリの非目標（DESIGN §2「禁止: Web UI」、§6）であり、別プロジェクト `celeris-gui` として作る。「GUI のバックエンドは
どこに置き、celeris と SQLite にどう触るか」を決める必要があった。制約:

1. 協調判断に LLM を使わない。状態の真実は SQLite（原則 1, 2）。
2. 状態変更は `TaskStore::apply_transition(_with_events)` / `create_task` などの状態機械を必ず通す。
3. `celeris` と `celerisctl` は既に同じ DB を別プロセスで開いている。
4. `events` の主キーは `(task_id, seq)` で、タスクをまたぐ順序列が無かった。
5. デーモンのメモリにしか無い情報（cooldown、実行中 run、並列度、承認待ちで延期中の reviewing、tick）がある。
6. 単一の研究者、ラップトップまたは制御プレーン用ホスト、ローカルのブラウザか SSH ポートフォワード。

初版（Proposed）は「(d) `celeris-gui` が task-core を使って SQLite を直接読み書きし、デーモン状態は celeris が毎 tick DB のスナップショット表に書く」を
推奨した。人間の決定はこれと異なる（下記）。

## 選択肢（初版で比較したもの）

- (a) 直接 DB: GUI が `task-core` を依存に取り SQLite を直接読み書き。
- (b) celeris に HTTP: celeris に HTTP/JSON + SSE の API を持たせ、GUI はそれだけを叩く。
- (c) 独立 API サーバ（task-core 利用、別プロセス）。
- (d) (c) + デーモン状態の DB 書き出し（毎 tick `daemon_status` 表）。
- (e) GUI が `celerisctl` を子プロセスで呼ぶ。(f) celeris が Unix ドメインソケットで読み取り専用 JSON を返す。

## 決定（人間の決定 H1 / H5 に従う）

### D1. `celeris-gui` は別プロセス・別リポジトリだが、**celeris が提供する HTTP API v1 だけ**を使う（案 (b)）

```
ブラウザ ──HTTP──▶ celeris-gui（Remix = React Router framework mode のサーバ。BFF）
                      │  loader / action が HTTP/JSON + SSE で呼ぶ
                      ▼
                  celeris（デーモンプロセス内の task-api = axum。/api/v1）──▶ task-ops ──▶ 状態機械 ──▶ SQLite
                                                                                                   ▲
                                                                                        celerisctl（従来どおり DB を直接）
```

- `celeris-gui` は **SQLite を開かず、celeris の crate にも依存しない**。契約は `docs/gui/api.md`（celeris リポジトリでは `docs/api/v1/api-v1.schema.json`）だけ。
- celeris の API は crate `task-api`（axum）で、celeris のデーモンプロセス内で `[api]` 設定があるときだけ動く（既定は無効）。
- 状態変更は celeris 内の `task-ops` → 状態機械。API のハンドラは協調判断をしない（ADR-0013 D3）。
- **ブラウザは celeris を直接呼ばない**（BFF の loader / action が呼ぶ）。したがって celeris の API は CORS を出さない。
- **celeris 停止中は GUI も操作不可**（一覧・詳細も読めない）。GUI は「celeris に接続できません」を表示するだけで、非常時の操作は `celerisctl`（DB 直接）で行う。
  初版の「celeris が止まっていても承認・回答できる」利点は捨てる（人間の判断: 契約を HTTP に一本化し、GUI を celeris の内部型と DB スキーマから切り離す方が重要）。

### D2. デーモンのメモリ状態は**メモリから直接公開**する（DB スナップショット表 P-G4 は不採用）

- ディスパッチャが tick の最後に `DaemonSnapshot` を作り `tokio::sync::watch` に送る。API は最新値を `GET /api/v1/daemon` と SSE の `daemon` イベントで返す。
- cooldown は `ProviderPolicy::cooldowns(now)`（既定実装つき）で取る。`ProviderThrottled{provider, until, reason?}` を `Requeue` と同じトランザクションで記録する（履歴用）。
- 真実ではなく観測値（`replay` の対象外）。celeris が止まれば消える。止まっていること自体は API に繋がらないことで分かる。
- 理由: 毎 tick の DB 書き込みは無駄（人間の指摘）。HTTP 層があるなら、そこからメモリを読めばよい。

### D3. リアルタイム更新は celeris 側の SSE（`GET /api/v1/stream`）→ BFF が中継 → ブラウザ

- `events` に `id INTEGER PRIMARY KEY`（グローバル単調）を持たせ（H4、ADR-0013 D6）、celeris の API がそれをカーソルにポーリングして SSE で流す。
  `celerisctl` の書き込みも同じ経路で拾える（in-process 通知は使わない）。
- `celeris-gui` は resource route で SSE を**そのまま中継**し（`Last-Event-ID` を転送）、ブラウザは `EventSource` で受けて loader を再検証する。イベント本体から状態を組み立てない。

### D4. SQLite の同時アクセス（celeris 側。ADR-0013 D5）

WAL、明示的 `busy_timeout`（既定 5000 ms）、`synchronous=NORMAL`、`schema_migrations` による版数（`SchemaTooNew`）。3 接続（ディスパッチャ、API、celerisctl）が同時に開く。
`celeris-gui` は関与しない（DB を開かない）。

### D5. 認証とバインド（二段）

- **celeris API**: 既定 `127.0.0.1:7710`。loopback 以外は `token_file` 必須、`Authorization: Bearer`。`Host` 検査。CORS 無し。
- **celeris-gui**: 既定 `127.0.0.1:7700`。BFF が celeris のトークンをサーバ側に保持し、ブラウザには渡さない。ブラウザ ↔ GUI の認証は、
  loopback なら無し（SSH ポートフォワード前提）、非 loopback ならパスワード → セッションクッキー。詳細は DESIGN-GUI §8。

### D6. API の形

HTTP/1.1 + JSON + SSE（gRPC / WebSocket は採らない。ADR-0013 D2）。型は Rust（`schemars`）→ `api-v1.schema.json` → `json-schema-to-typescript` → TS。
OpenAPI は当面不要（利用者は自分の BFF と `curl` だけ）。

### D7. 暫定措置は無し

初版 D7（`celerisctl` の gate.rs 等を逐語的に写す）は不要になった。celeris の Phase 9a で `task-ops` が抽出され、9b で API が付いてから G フェーズを始める（DESIGN-GUI §10 の前提）。
GUI 側で celeris の判断ロジックを再実装することは**禁止**（`docs/gui/bootstrap/CLAUDE.md`）。

## 初版の推奨（(d)）と人間の決定で変えた点

| 観点 | 初版の推奨 (d) | 決定 (b) | 変わった理由・帰結 |
|---|---|---|---|
| GUI の celeris への依存 | `task-core` / `task-ops` に git タグで依存（P-G12） | **HTTP API と JSON Schema だけ**。crate 依存無し | Rust の版ずれ・スキーマ版数の検査が GUI 側から消える。P-G12 は不要 |
| GUI の言語構成 | Rust（axum）+ 埋め込み SPA の単一バイナリ | Node（React Router framework mode の SSR サーバ = BFF） | GUI は Node ランタイムで配布（DESIGN-GUI §9）。単一バイナリは Node SEA の実験項目 |
| celeris 停止中 | GUI から承認・回答できる | **できない**。`celerisctl` を使う | 契約の一本化を優先 |
| デーモン状態 | 毎 tick DB に `daemon_status` を書く（P-G4） | メモリから `watch` で公開 | DB 書き込み無し。停止後は消える（停止は接続失敗で分かる） |
| SQLite を開くプロセス | celeris / celerisctl / celeris-gui の 3 つ | celeris（ディスパッチャ + API の 2 接続）/ celerisctl | WAL / busy_timeout は変わらず必要 |
| リアルタイム | GUI が DB をポーリング → SSE | celeris の API が DB をポーリング → SSE → BFF が中継 | ポーリングの実装が celeris 側に移る |
| ファイル配信・パス検査 | GUI サーバの責務 | **celeris（task-api）の責務**。BFF は中継するだけ | ワークスペースの境界を守る場所が 1 か所になる |
| 認証 | GUI サーバで完結 | celeris API（Bearer）+ GUI（クッキー）の二段 | celeris がネットワークサービスになる分、既定 loopback + 非 loopback はトークン必須で守る |
| celeris 側の実装量 | 小（4 提案） | 中（task-ops + task-api + 基盤）。Phase 9 | GUI の G フェーズは Phase 9 完了後 |
| GUI の変更が celeris の ADR を要するか | 初回だけ | API v1 に無いものが要るたび | GUI 側は回避せず「celeris への提案」として止まる（BLOCKED。DESIGN-GUI §10） |

## 結果

- celeris リポジトリ: ADR-0013、Phase 9a/9b、`docs/api/v1/api-v1.schema.json`、`[api]` 設定。
- `celeris-gui` リポジトリ: `docs/celeris-api-v1.md`（= `docs/gui/api.md` の写し）を契約として読み、`app/celeris/types.ts` を生成する。DB・crate に触れない。
- `celeris-proposals.md` の P-G4 / P-G12 は置換、P-G1〜G3 / G5〜G7 / G9 / G11 は採用（ADR-0013）。

## 再評価の条件

- マルチユーザが目標になったとき（`by` の記録、権限。ADR-0013 D12 の P-G13）。
- celeris をリモートホストで動かし GUI を手元で動かす配置が要るとき（この設計では API を非 loopback にバインドしてトークンで守るだけで対応できる。SSH ポートフォワードが第一選択）。
- celeris の API のポーリング（250 ms）が tick を遅らせると観測されたとき。
