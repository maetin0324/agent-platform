# agent-platform — taskd と Web GUI

研究の作業を **タスクの木**として置いておくと、LLM エージェント（claude-code / codex）が順に実行し、
**受け入れ条件で判定**して、人の判断が要るところだけを人に返す基盤。決定はすべて手元の SQLite に残り、
`taskctl replay` で再現できる。

```
                        ┌──────────────┐
  ブラウザ ── 7700 ──▶  │  gui/        │ ── 7710（HTTP API v1）──▶ taskd（デーモン）
                        │  React Router│                              │
                        └──────────────┘                              ├─ ディスパッチャ（決定的。LLM を呼ばない）
                                                                      ├─ ワーカー（claude-code / codex を起動）
                                                                      └─ SQLite（tasks / events。唯一の真実）
                                                                            │
                                    コマンドだけ ssh で ──────────────────────┘
                                    pegasus / sirius（LLM は手元、コマンドはクラスタ）
```

## ディレクトリ

| パス | 中身 |
|---|---|
| `crates/task-core` | タスク・イベント・SQLite ストア（状態機械の真実） |
| `crates/task-ops` | 受信箱・一覧・詳細の組み立て（協調判断をしない読み取り側） |
| `crates/task-dispatch` | ディスパッチャ（誰にいつ何を割り当てるか。決定的） |
| `crates/task-worker` | ワーカー（アダプタ、ワークスペース、クラスタ実行 `ssh.rs`） |
| `crates/task-api` | HTTP API v1（axum。`docs/gui/api.md`） |
| `crates/taskd` | デーモン本体（設定・tick・API の起動） |
| `crates/taskctl` | CLI（`add` / `approve` / `ls` / `show` / `replay` / `worker run`） |
| `gui/` | Web GUI（別プロセス・別言語。taskd の API v1 だけを使う。ADR-0020） |
| `docs/DESIGN.md` | 設計。**ここが正**（変更は ADR を書いてから） |
| `docs/PROGRESS.md` | どこまで終わっているか、証拠、未解決の提案 |
| `docs/adr/` | 設計判断の記録（0001〜） |
| `config/*.example.toml` | 設定の雛形（fake / claude-code / クラスタ） |
| `scripts/` | クラスタへのログイン・点検、GUI 文書の同期 |

## 動かす

```sh
# 1. ビルドと検査
cargo build --workspace && cargo test --workspace

# 2. 設定を作る（DB は必ずローカルディスクに置く。NFS 上の SQLite は WAL が遅くて詰まる）
mkdir -p /local/$USER/taskd && cp config/taskd.claude-code.example.toml /local/$USER/taskd/taskd.toml

# 3. デーモン
target/debug/taskd --config /local/$USER/taskd/taskd.toml

# 4. タスクを入れる
target/debug/taskctl --db /local/$USER/taskd/taskd.sqlite3 \
  add --title "..." --objective "..." --check-cmd "cargo test"
target/debug/taskctl --db /local/$USER/taskd/taskd.sqlite3 approve <task-id>

# 5. GUI（別の端末で）
cd gui && pnpm install && TASKD_API_URL=http://127.0.0.1:7710 pnpm dev   # → http://127.0.0.1:7700
```

GUI の **「使い方」ページ（`/help`）** に、画面の意味・受け入れ条件の書き方・状態の読み方・困ったときの対処がある。

## クラスタでコマンドを実行する（pegasus / sirius）

LLM は手元で動き、**コマンドだけ**がクラスタで走る。ssh は 2 要素認証なので**人が多重接続を張る**。

```sh
scripts/cluster-login.sh pegasus                                   # 人が 2FA を通す（ControlMaster）
scripts/cluster-check.sh pegasus /work/NBB/$USER/workspace/rust/benchfs   # どの sync を使うか教えてくれる
```

`[[clusters]] sync` の選び方（ADR-0019）:

| 値 | 使う場面 |
|---|---|
| `worktree` | **既定の選択**。git 管理下のプロジェクト。`git worktree` を切って追跡ファイルだけを扱う（263 GB のリポジトリでも写しは数 MB） |
| `rsync` | git 管理外の小さなディレクトリ |
| `none` | 手元とクラスタでファイルシステムが共有されている場合 |

## 決まりごと

- ディスパッチャとストアに LLM 呼び出しを入れない（協調判断は run の中だけ）。
- 設計判断は `docs/adr/` に書いてから実装する。`docs/DESIGN.md` は提案を経てから変える。
- 各 Phase の完了時に `cargo test --workspace` と `cargo clippy --workspace -- -D warnings` を通し、
  `docs/PROGRESS.md` に証拠を残す（詳細は `CLAUDE.md`）。
