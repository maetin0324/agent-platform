# Celeris — 研究・開発・インフラ管理を任せるエージェント基盤

Celeris は、複数の案件を、**記憶を持つエージェント組織に継続して任せるための基盤**です。
秘書役の Chief of Staff（CoS）に相談し、各所から届く報告を読み、必要なところに指示を出す。
その間の調査・計画・実装・検証・レビューを進め、案件を同時に抱える人の負担を減らすことを目指しています。

たとえば「Pluvio を基盤に新しい研究テーマを探したい」という依頼から、関連研究調査、候補の議論、
PoC、クラスタでの実験、結果の整理、論文執筆へと仕事をつなぎます。コードの修正やクラスタの点検など、
小さな依頼も同じ画面から投げられます。

## 使い方の中心

入口は Web GUI の **Console** です。全案件の対話・作業の進行・報告・質問・認可要求を一本の流れで確認し、
その場で指示や返答を送れます。案件や担当で絞り込み、作業の詳細は必要なときに開きます。

1. **相談する** — CoS に依頼やアイデアを伝え、案件の理解、大まかな方針、最初の途中目標をすり合わせる。
   小さな依頼は、Console の一言から直接タスクを作って実行する。
2. **任せる** — 仕事を依存関係のあるタスクに分解し、能力と実行条件に合う担当へ割り当てる。
   コーディング、文献調査、Web 調査、データ分析など、用途に応じた実行手段を使う。
3. **途中で口を出す** — 仕事の木や成果物を確認し、担当との対話やタスクへのコメントで方針を修正する。
   研究全体の議論から実装の細かな指示まで扱う。
4. **結果を判断する** — 途中目標ごとに「得られた結果」と「次の提案」を受け取り、**ok / 議論 / ng** で答える。
   認可が必要な操作には「今回だけ / 今後ずっと / 拒否」で応じる。
5. **成果と経験を残す** — コードは普段のリポジトリへ取り込み、文書は GUI で読む。
   次の案件でも使える知識は、出典付きの Markdown として蓄積する。

## 組織・仕事・知識

| 概念 | 役割 |
|---|---|
| **組織の木** | CoS を根に Engineering・Research・Operations などの担当を置く。各ノードは知識、道具、権限、実行方針、レビュー基準を持ち、子が親の設定を継承する |
| **案件と途中目標** | 継続して取り組む目的と、人が成果を判断する区切り。案件は複数のリポジトリを扱える |
| **仕事の木・DAG** | 案件を実行可能なタスクに分解したもの。依存関係、担当、進行状況を確認する |
| **スキル** | `rust`・`benchmark`・`paper-writing` などの能力タグ。担当の選択に使う |
| **ハーネス** | 対話・計画・実装・調査などの実行契約。組織と一対一にせず、入力・成果物・指示・予算を定義する |
| **モード** | `prototype`・`production`・`research`。試作、通常の開発、根拠を伴う研究で進め方と検証基準を変える |
| **記憶と共有知識** | 担当ごとの手帳と、案件をまたいで利用する知識ベース。環境の使い方、人の好み、判断、解決方法を引き継ぐ |

モデルの供給と作業の実行手段を分け、必要な能力とアカウントの残量に応じて使い分ける構想です。
現在は Claude Code・Codex・ACP 経由の実行、PaperQA・Local Deep Research による調査、
LangMem による知識整理などのアダプタを持ちます。利用にはそれぞれの設定・認証・実行環境が必要です。

知識ベースの正本は、既定で `~/.local/share/celeris/knowledge` に置く Git 管理の Markdown です。
有効化すると、仕事の終了後に知識の抽出・整理を行い、確信度の高い追加・更新は取り込み、
確認が必要な候補は GUI の受信箱に残します。詳細は [知識ベース](docs/knowledge.md)を参照してください。

## 実行を支える仕組み

- **状態と実行履歴を永続化する。** タスク状態は SQLite に置き、状態遷移やレビュー結果を追記イベントに残す。
  `celerisctl replay` でタスク状態を再構成できる。
- **提案と実行制御を分ける。** LLM は対話・計画・作業・レビューを担う。担当の選択、ディスパッチ、
  再試行、状態遷移は決定的なコードで扱う。CoS の操作提案も検証してから適用する。
- **完了を検証する。** ワーカーが返す成果と証拠を、受け入れ条件に沿ってコマンドやレビュアーで判定する。
  途中目標の達成と次の方針は人が判断する。
- **作業場所を管理する。** タスクごとの Git worktree、複数リポジトリ、ホスト・コンテナでの実行に対応する。
  GUI から差分を確認し、変更の取り込みや PR 作成を行う。
- **クラスタを使う。** 手元で編集し、必要なコードを同期して SSH 経由でコマンドを実行する。
  大きなリポジトリには worktree 同期を使い、転送対象を抑える。
- **報告を集約する。** 担当からの報告を上位で圧縮し、問題や判断待ちを GUI と設定済みの Discord 通知へ届ける。

```text
ブラウザ
  │ :7700
  ▼
Web GUI（React / React Router、Node BFF）
  │ :7710 / HTTP API v1 + SSE
  ▼
celeris（Rust デーモン）
  ├─ 対話・案件・組織・報告・認可・知識整理
  ├─ 決定的なディスパッチャ
  ├─ ワーカー → 各ハーネス／アダプタ → ローカル・コンテナ・SSH での作業
  ├─ SQLite（タスク・イベント・案件など）
  └─ ファイル（Git リポジトリ・成果物・記憶・共有知識）
```

GUI は API だけを使い、SQLite を直接読み書きしません。API トークンは Node 側で保持します。

## 現在地

Console、組織プロファイルと担当の選択、案件と途中目標、レビューと認可、報告の集約、
ワークスペース、共有知識とその自動整理まで実装されています。
2026-09-20 の進捗記録には、**Console への指示 → タスク作成 → 担当決定 → 実行 → レビュー → 報告 → 知識の保存**を
実機で通した証跡があります。Celeris 自身の改善を扱うリリース作成・検証・昇格・ロールバックの仕組みもあります。

各機能の検証結果と未解決事項は [PROGRESS.md](docs/PROGRESS.md) の該当 Phase と末尾の実機記録を参照してください。
先頭の Phase 一覧や初期設計には古い記述が残っています。

## 起動する

必要なものは Rust、Node.js 24 以上、pnpm 11 です。実際の仕事を動かす場合は、使うアダプタの CLI・認証なども用意します。

```sh
# リポジトリのルートで
cargo build --workspace

# 新規環境用の設定を用意する（既存環境はその設定を使う）
mkdir -p ~/.config/celeris
cp -n config/celeris.example.toml ~/.config/celeris/config.toml
cp -n config/org.example.toml ~/.config/celeris/org.toml
```

起動前に `~/.config/celeris/config.toml` を編集します。

- 冒頭の `db`・`workspace_root` と `[memory] dir` を保存先に合わせる。**SQLite はローカルディスクに置く**。
  相対パスは設定ファイルのあるディレクトリ基準になる。
- トップレベルの `org_include = "org.toml"` を有効にする。組織の種は DB が空のときだけ読み込まれ、以後は GUI で編集する。
- `[api]` と `listen = "127.0.0.1:7710"` のコメントを外す。
- 例のプロバイダは **`fake`（開発用）**。実際の依頼を処理するには `[[providers]]` と必要なアダプタ設定を変更する。
  [Claude Code](config/celeris.claude-code.example.toml)、[Codex](config/celeris.codex.example.toml)、
  [複数アカウント](config/celeris.multi-account.example.toml)、[調査](config/celeris.research.example.toml)の例を参照する。
  これらは用途別の設定例なので、組織・ハーネス・API の設定と合わせて使う。

```sh
# デーモン
target/debug/celeris --config ~/.config/celeris/config.toml
```

別の端末で GUI を起動します。

```sh
cd gui
pnpm install --frozen-lockfile
CELERIS_API_URL=http://127.0.0.1:7710 pnpm dev
```

ブラウザで `http://127.0.0.1:7700/` を開くと Console が表示されます。
API の `token_file` を設定した場合は、GUI にも `CELERIS_API_TOKEN_FILE` を指定してください。
GUI の認証・公開設定は [gui/README.md](gui/README.md)、継続運用は [デプロイ手順](docs/selfdeploy.md)を参照してください。
画面の説明と操作方法は GUI の `/help` にあります。

## クラスタで実行する

クラスタ設定は [設定例](config/celeris.clusters.example.toml)を参照してください。
2 要素認証が必要な接続は、人が SSH の多重接続を張ります。

```sh
scripts/cluster-login.sh pegasus
scripts/cluster-check.sh pegasus /work/NBB/$USER/workspace/rust/benchfs
```

| `sync` | 用途 |
|---|---|
| `worktree` | Git 管理下のプロジェクト。追跡ファイルを扱い、大きな作業ディレクトリ全体のコピーを避ける |
| `rsync` | Git 管理外の小さなディレクトリ |
| `none` | 手元とクラスタでファイルシステムを共有している場合 |

## リポジトリと資料

| パス | 内容 |
|---|---|
| `crates/task-core` | タスク・組織・プロファイルなどのモデル、状態機械、SQLite ストア |
| `crates/task-ops` | 操作の検証と適用、担当の選択、画面向けデータの組み立て |
| `crates/task-dispatch` | ディスパッチ、リース、再試行、レビュー、報告 |
| `crates/task-worker` | ワーカープロトコル、アダプタ、ワークスペース、SSH 実行 |
| `crates/task-api` | HTTP API v1 と SSE（axum） |
| `crates/celeris` / `crates/celerisctl` | デーモンと CLI |
| `gui/` | Web GUI と BFF |
| `config/` | 設定例と組織の種 |
| `scripts/` / `deploy/` | クラスタ、コンテナ、知識整理、リリースと運用の道具 |

- [SPEC.md](docs/SPEC.md) — 何を実現したいか。利用場面と人の関わり方。
- [DESIGN.md](docs/DESIGN.md) — 初期のタスク管理層の設計。以後の拡張・変更は ADR と併読する。
- [ADR](docs/adr/) — 設計判断。[組織](docs/adr/0046-organization-as-agent-profiles.md)、
  [知識](docs/adr/0047-knowledge-base.md)、[Console](docs/adr/0048-console.md)が現在の構成を説明する。
- [PROGRESS.md](docs/PROGRESS.md) — 実装・検証・実機運用の記録と未解決事項。
- [モデル供給とアカウント](docs/providers.md) — 自動選択、Codex の残量確認、既存の Claude 固定設定の移行。
- [ワークスペース](docs/workspace.md) / [知識ベース](docs/knowledge.md) / [API](docs/gui/api.md) — 各機能の仕様と使い方。

## 開発と検証

設計判断は ADR に記録し、実装・検証の結果を `docs/PROGRESS.md` に残します。
ディスパッチャとストアには LLM 呼び出しを入れず、LLM を使う処理は run とアダプタの境界で扱います。

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

GUI の変更では `gui/` で `pnpm lint`、`pnpm typecheck`、`pnpm test`、`pnpm build` を実行します。
実 celeris を使う E2E の手順は [GUI の開発手順](gui/README.md#開発)を参照してください。
