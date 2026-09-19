# リポジトリの中の設定 `.config/celeris/workspace.toml`

- 関連: ADR-0042 D2（リポジトリの中の設定は `.config/celeris/` に集める）、ADR-0043 D4（この設定の中身）、
  ADR-0043 D2（タスクの作業場所）、ADR-0043 D8（成果物の置き場）
- 実装: `crates/task-core/src/workspace_config.rs`（TOML、`deny_unknown_fields`）
- Phase 52 で導入

## 1. これは何か

Celeris が案件のリポジトリで仕事をするとき、**そのリポジトリのことはそのリポジトリに書いてある**ようにする
ための設定である。置き場所は

```
<リポジトリのルート>/.config/celeris/workspace.toml
```

の 1 つだけ（ルートに `.celeris/` や `.taskd/` は作らない。ADR-0042 D2）。

**書いてあることだけを使う。言語やファイル構成からの推定はしない**（ADR-0043 §3）。ファイルが無ければ
全部既定で動く（従来どおり）。TOML として壊れている・知らないキーがある場合は **warn して既定に倒す**
（設定の間違いで案件が止まらないように）。

## 2. 全部の項目

```toml
[workspace]
name = "benchfs"                                  # 任意。案件に登録するときの既定の名前
description = "ad-hoc FS のベンチマーク（Rust）"   # 任意。計画 run と前置きに出す 1 行

[run]
mode = "host"                                     # "host"（既定）| "container"

[container]                                       # mode = "container" のときだけ意味がある
image = "ghcr.io/…/rust-dev:1.90"                 # か dockerfile = ".config/celeris/Dockerfile"
mounts = ["/dev/infiniband:/dev/infiniband"]
env = { CARGO_TARGET_DIR = "/workspaces/.cargo-target" }

[commands]
setup = ["cargo fetch"]                           # worktree を作った直後に一度だけ
check = ["cargo test --workspace", "cargo clippy --workspace -- -D warnings"]

[outputs]
docs = "docs"                                     # 文書ページの根。既定 "docs"
deliverables = "."                                # コード以外の成果物の根。既定はリポジトリのルート
```

| 節 | 項目 | 既定 | いつ使われるか |
|---|---|---|---|
| `[workspace]` | `name` | 無し | 人がリポジトリを案件に登録するときの名前の候補（現状 taskd は読むだけ。GUI が使う） |
| `[workspace]` | `description` | 無し | **計画 run**（「この案件のリポジトリ」の一覧）と、タスクの前置きの「作業場所」 |
| `[run]` | `mode` | `"host"` | `"container"` ならこのリポジトリを使うタスクの run は**コンテナの中**（§7。Phase 56） |
| `[container]` | `image` / `dockerfile` / `mounts` / `env` | 無し | `mode = "container"` のときのイメージ・追加マウント・環境変数（§7） |
| `[commands]` | `setup` | `[]` | worktree を作った直後に**一度だけ**ホストで流す。記録は `<task_dir>/runs/setup.log`。1 つでも落ちたら run を始めず、タスクを `blocked` にして人に聞く |
| `[commands]` | `check` | `[]` | タスクの前置きに「このリポジトリの検査コマンド」として出す。加えて、**タスクの `acceptance` に検査コマンドが 1 つも無いときだけ**、レビューの暗黙の条件（`exit 0` を期待）になる |
| `[outputs]` | `docs` | `"docs"` | 前置きの「文書は `<repo>/<docs>` の下に置け」 |
| `[outputs]` | `deliverables` | `"."` | 前置きの「コード以外の成果物（図・表・原稿）は `<repo>/<deliverables>` の下に置け」 |

## 3. どう効くか（前置きの例）

タスクが `benchfs`（git）と `data`（git ではないディレクトリ）を使うとき、ワーカーのプロンプトの
「## 作業場所」にはこう出る:

```
この案件のリポジトリのうち、このタスクが使うものは次のとおり:
- `benchfs` → `/home/rmaeda/.local/celeris/workspaces/01J…/repos/benchfs`（worktree、ブランチ `celeris/01J…`、base `9602b596826c`（main）） — ad-hoc FS のベンチマーク（Rust）
- `data` → `/home/rmaeda/.local/celeris/workspaces/01J…/repos/data`（ディレクトリ。読み書き可。git ではない）
カレントディレクトリは `/home/rmaeda/.local/celeris/workspaces/01J…/repos/benchfs`。編集はこの作業場所の中だけで行い、元のリポジトリには直接書くな。
git のリポジトリでは taskd が用意したブランチにコミットせよ。`main` に直接コミットするな。`git checkout` でブランチを変えるな。
`benchfs` のこのリポジトリの検査コマンド: `cargo test --workspace` / `cargo clippy --workspace -- -D warnings`
コード以外の成果物（図・表・原稿）は `/home/rmaeda/.local/celeris/workspaces/01J…/repos/benchfs/`、文書は `…/repos/benchfs/docs` の下に置け。`artifacts/` は run の中間物・ログ・機械向けの `result.json` だけで、人が読む成果物を置く場所ではない。
```

計画 run（`POST /projects/{id}/plan`）にはさらに「この案件のリポジトリ」の一覧が出て、プランナーは
子タスクごとに `"repos": ["benchfs"]` と**名前で**選ぶ（ADR-0043 D2。知らない名前を書くと計画は差し戻される）。

## 4. 例: このリポジトリ（agent-platform）

`.config/celeris/workspace.toml`:

```toml
[workspace]
name = "agent-platform"
description = "Celeris（taskd / taskctl / GUI）本体。Rust のワークスペース + gui/ の Remix アプリ"

[run]
mode = "host"

[commands]
check = ["cargo test --workspace", "cargo clippy --workspace -- -D warnings"]

[outputs]
docs = "docs"
```

- `setup` は書いていない（`cargo` は初回のビルドで依存を取ってくるので、worktree ごとに流す必要が無い）。
- `check` は `CLAUDE.md` の「各 Phase 完了時に必ず」と同じ 2 本。自己改善の実装タスクが受け入れ条件に
  検査コマンドを書かなかったときの保険になる。
- `deliverables` は書いていない（既定 = リポジトリのルート）。この案件の成果物は `docs/` と `crates/` の
  中にあるので、`docs` だけで足りる。

## 5. 注意

- **`setup` は 1 タスクにつき 1 回**。判定は `<task_dir>/runs/setup.log` があるかどうかだけ
  （worktree を作り直しても、同じタスクなら 2 回目は流れない）。
- `setup` が落ちたタスクは `blocked` になり、質問「setup が失敗しました …」が積まれる。人が
  `POST /tasks/{id}/answer` で答えると次の run から再開する（`setup.log` があるので `setup` は再実行しない）。
- `check` をレビューの暗黙の条件に足すのは、**タスクの `acceptance` に `Check::Command` が 1 つも無いとき
  だけ**である。タスクが自分で検査コマンドを書いていれば、それが勝つ（ADR-0043 D4）。
- `[run] mode = "container"` と `[container]` は §7 のとおりに効く（Phase 56）。`mode` を書かなければ
  従来どおりホストで走る。

## 6. 変更の取り込み（ADR-0043 D5。Phase 54）

タスクは `<task_dir>/repos/<name>/` の worktree で働き、ブランチ `celeris/<task_id>` に変更を積む。
終端（`done` / `failed`）になっても worktree もブランチも**消えない**（ADR-0043 D2）。そこから先、
それを `main` に入れるかどうかを決めるのは**人**である（SPEC §3.6。組織の「人」＝ワーカーには
この経路が無い）。

### 6.1 見る

- `GET /api/v1/tasks/{id}/changes` — リポジトリごとに `base` / `head` / `ahead`（コミット数）/
  変わったファイルの一覧（`+` / `-` 付き）/ `dirty`（未コミットの変更があるか）。
  **コミットが 1 つも無ければ `ahead = 0`**（調査のようにコードを伴わないタスク。GUI は「変更なし」）。
  `dir` のリポジトリは対象外。
- `GET /api/v1/tasks/{id}/changes/{repo}/diff?path=…` — 1 ファイルの unified diff（**200 KiB で切る**）。
- worktree を消した後でもブランチが残っていれば、元のリポジトリから同じものが見える。
  どちらも無ければ `missing: true`。
- GUI ではタスク画面の `/tasks/<id>/changes`。

### 6.2 取り込む（`POST /api/v1/tasks/{id}/changes/{repo}/integrate`。人だけ）

| `method` | すること | 後片付け |
|---|---|---|
| `merge` | 一時 worktree でブランチを `default_branch` に `rebase` → 成功なら `default_branch` を進める。**push はしない** | worktree を消し、ブランチを `git branch -D` |
| `pr` | `git push -u origin <branch>` → `gh pr create`（本文は目的・受け入れ条件・最新の報告・Celeris のリンク） | PR が merge されたと分かった時点で消す |
| `discard` | 何も取り込まずに捨てる（`{"confirm": true}` が要る） | worktree を消し、ブランチを `git branch -D` |

`merge` の細かい規則:

- **人のチェックアウト（`project_repos.location.path`）が `default_branch` を出していて汚れていたら
  409「`<default_branch>` が編集中」**。何も触らないので、片付けてからもう一度押す。
- 出していて綺麗なら `git -C <path> merge --ff-only <sha>`（人の作業ツリーもそのまま進む）。
- 別のブランチを出していれば `git update-ref` でブランチの先だけ動かす（**人の作業ツリーには触らない**）。
- `rebase` が衝突したら `rebase --abort` して worktree もブランチも残し、**「衝突の解消: <題名>」タスク**を
  同じ担当で自動的に作る。そのタスクは**親の worktree の上で**（`mode = "shared"`）働き、
  受け入れ条件は「作業ツリーが clean」「rebase が進行中でない」「`default_branch` が `HEAD` の祖先」。
  終わったら人がもう一度「取り込む」を押す。

### 6.3 GitHub（`[github]`）

```toml
[github]
gh = "gh"              # GitHub CLI の場所。PATH にあれば既定のまま
merge_method = "merge" # 「Celeris で merge」= `gh pr merge --<method> --delete-branch`
```

- `gh` が無い・認証されていない（`gh auth status` が失敗する）ときは、PR の経路が 409 になり、
  ローカルの `merge` と `discard` だけが使える。判定はプロセス内で 60 秒だけ覚える。
- **PR の状態の同期は画面を開いたときだけ**（`gh pr view`。常時同期はしない）。`merged` を見つけた
  時点で worktree とローカルのブランチを片付ける。
- 「Celeris で merge」は `POST /api/v1/tasks/{id}/changes/{repo}/pr/merge`。GitHub 側でも merge される。
- 案件画面の「PR と取り込み」は `GET /api/v1/projects/{id}/integrations`（タスク × リポジトリごとに
  最新の 1 件。1 回に同期する PR は 20 件まで）。

## 7. コンテナ実行（ADR-0043 D3。Phase 56）

リポジトリが `run = container`（案件のリポジトリの設定）か、`run = auto`（既定）で `workspace.toml` に
`[run] mode = "container"` と書いてあるとき、そのタスクの **ワーカー（ハーネスの CLI そのもの）が
コンテナの中で起きる**。devcontainer と同じ考え方で、ホストにツールチェーンを生やさずに済ませる。

```toml
[run]
mode = "container"

[container]
image = "ghcr.io/…/rust-dev:1.90"            # か dockerfile = ".config/celeris/Dockerfile"
mounts = ["/dev/infiniband:/dev/infiniband"] # 追加のマウント（そのまま runtime に渡る）
env = { CARGO_TARGET_DIR = "/w/.cargo-target" }
```

### 7.1 いつホストにするか（大事）

**迷ったらホスト**（既定）。コンテナにするのは「ツールチェーンをホストに置きたくない」ときだけである。
次のものは**ホスト実行**（リポジトリの `run` を `host` に）にすること:

- **ホストのデバイスが要るもの**（benchfs の InfiniBand、GPU、`/dev/fuse`、特権が要る計測）。
  `[container] mounts` でデバイスを渡すこともできるが、権限と `cgroup` の都合でまず壊れる。
  デバイスが要るなら最初から `run = host` が正しい。
- **リモートのクラスタで動かすもの**（ADR-0018 / 0019 の (a)）。リモートのタスクは常にホスト扱い。
- **`paperqa` / `local-deep-research` のタスク**。この 2 つは**設定に関わらず常にホスト**で走る
  （道具立てがホストの venv にあり、コンテナに持ち込むと別物になる。ADR-0043 Phase 56 追記）。

### 7.2 何がどう見えるか

タスクごとに `<runtime> run --rm -i --network host …` が 1 つ起きる。中身は決定的:

| 何 | どうする | なぜ |
|---|---|---|
| uid | podman: `--userns=keep-id` / docker: `--user <uid>:<gid>` | マウントした worktree にホストと同じ持ち主で書けるように |
| タスクのディレクトリ | `<workspace_root>/<task_id>` を**同じパス**で読み書き可 | worktree・`artifacts/`・`runs/` が前置きに書いたパスのまま見える |
| `dir` のリポジトリ | シンボリックリンクの**実体**を同じパスで読み書き可 | リンクはコンテナの中では辿れないため |
| 認証情報 | `CLAUDE_CONFIG_DIR` / `CLAUDE_SECURESTORAGE_CONFIG_DIR` / `CODEX_HOME` / `OPENCODE_CONFIG` の指す場所を**同じパスで読み取り専用** | アダプタが env に書いた場所だけを、書けない形で渡す（ADR-0024 のアカウントプールが選んだものだけ） |
| ネットワーク | `--network host` | LLM の API と、手元の Qwen のポート（`bnode150:18000` の中継など）に届く必要がある |
| cwd | `-w <repos[0] の作業ツリー>` | ホスト実行と同じ場所 |
| 環境変数 | `HOME=<task_dir>` → アダプタの env → `[container] env`（後勝ち） | ホームは**マウントしない**ので、書ける HOME をタスクのディレクトリに置く |
| 目印 | `--label celeris.task=<task_id>` | 取り残したコンテナをラベルで消せるように |

**見せないもの**: ホームディレクトリ、`~/taskd`、`~/.local/celeris` の根。必要なものだけを同じパスで渡す。

### 7.3 イメージ

1. `[container] image` — **そのまま使う**（Celeris はビルドもプルもしない。手元に無ければ runtime が引く）。
2. `[container] dockerfile`（リポジトリ相対。例 `.config/celeris/Dockerfile`）— **Dockerfile の内容と
   `.config/celeris/` の中身の sha** からタグ `celeris-ws-<sha12>` を作り、`~/.local/celeris/containers/<tag>/`
   でビルドしてキャッシュする。**同じ内容なら再ビルドしない**（`<runtime> image inspect` で見る）。
   文脈に送るのは `.config/celeris/` の写しだけで、リポジトリ全体ではない。ビルドは
   `<runtime> build --network host`（run と同じネットワーク。入れ子のコンテナでは `RUN` が
   ブリッジを張れないため）。
   記録は `<task_dir>/runs/container-build.log`。上限は `[containers] build_timeout_secs`（既定 1800 秒）。
3. どちらも無い — `[containers] image_default`（既定 `celeris-worker:latest`）。中身は
   `deploy/containers/celeris-worker/Dockerfile`（Debian stable slim + node LTS と claude/codex CLI +
   rust stable + python3/uv + git・gh・rsync・ssh）。作るのは `scripts/containers/build-worker.sh`
   （**`cargo test` の一部ではない**。ネットワークに出るので人が 1 度叩く）。

タスクが複数のリポジトリを使い、複数がコンテナを要求したときは、**primary → `repos[0]` の順で最初に
要求したもの**のイメージ・`mounts`・`env` を使う（環境は混ぜない。ADR-0043 D3）。

### 7.4 `setup` もコンテナの中

`[commands] setup` は worktree を作った直後に一度だけ走るが、**コンテナのタスクではコンテナの中で走る**
（ADR-0043 D3）。`runs/setup.log` の 1 行目に `# 実行環境: コンテナ <image>（<runtime>）` が出る。

### 7.5 runtime が使えないとき

`[containers] runtime`（既定 `auto` = podman → docker）を taskd が起動時に 1 度だけ `<runtime> info` で
確かめる。結果は `GET /api/v1/daemon` の `containers` とログに出る。**どれも使えなければ、コンテナが要る
タスクは run を始めずに `blocked` になり**、人に質問が積まれる:

> コンテナ runtime が使えません（`benchfs` が `run = container` を要求しています）: podman: … / docker: …。
> `[containers] runtime` の設定か、podman / docker の用意を見てください。
> ホストで走らせてよければ、そのリポジトリの `run` を `host` にしてください。

イメージのビルドが落ちたときも同じ経路（`blocked` + 質問）で、記録は `runs/container-build.log` にある。
`POST /api/v1/tasks/{id}/answer` で答えると次の run から再開する。
