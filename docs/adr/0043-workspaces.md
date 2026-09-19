# ADR-0043: ワークスペース — 案件は複数のリポジトリを持ち、タスクはその一部を worktree で触り、変更は差分を見て取り込む

- 日付: 2026-09-19
- 状態: **Accepted**（人間の回答 2026-09-19。要点: 1 案件が複数リポジトリを跨げること（benchfs と benchfs-paper）、取り込みは
  GitHub が使えるなら Celeris と GitHub の両方から見えて Celeris で merge すれば GitHub でも merge される、コードを伴わない
  タスクは柔軟に、環境は devcontainer 的にホストのリポジトリをマウントしてコンテナ内でツールチェーンを完結させるが benchfs の
  ようにホストのデバイスが要るものはホスト実行に切替可、リモートは今の (a)（手元で編集・rsync・実行だけリモート）を既定に
  (b)（ログインノード上で編集も実行も）も選べる、git でない場所は `dir` で十分、成果物は案件のリポジトリの中、中止したタスクの
  worktree とブランチは消す）
- 関連: SPEC §2.1 / §5、ADR-0018 / 0019（クラスタ、リモート worktree）、ADR-0036（成果物）、ADR-0039（案件の作業場所）、
  ADR-0041 D1（タスクごとの worktree）、ADR-0042（名前とパス）、ADR-0044（タスク管理。取り込みの UI はタスク画面）

## 1. 文脈

ADR-0039 / 0041 で「案件に作業場所が 1 つ、タスクごとに worktree」までは来た。しかし (1) 論文（benchfs-paper）とコード
（benchfs）のように 1 案件が複数のリポジトリを跨ぐ、(2) リポジトリごとに環境（toolchain、`pnpm install`、テストのコマンド）が
違う、(3) 終わったタスクのブランチを**誰がどう取り込むか**が無い、(4) 環境をホストに直接生やすとホストが汚れる、(5) GUI から
作業ツリーの中が見えない。Cursor / Claude Code が単体で持つ「差分を見て受け入れる」「環境を閉じる」を、組織の道具として
持つ必要がある。

## 2. 決定

### D1. 案件は「リポジトリ」を複数持つ（`project_repos`）

migration `0012_project_repos.sql`:

```
project_repos(id TEXT PK, project_id TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('git','dir')),
  location_json TEXT NOT NULL,      -- {"kind":"local","path":"/abs"} | {"kind":"remote","cluster":"pegasus","path":"/abs"}
  default_branch TEXT NULL,         -- git のみ。無ければ検出（origin/HEAD → main → master）
  sync TEXT NULL,                   -- remote のみ: "worktree"(既定=ADR-0019 の (a)) | "rsync" | "none"
  run TEXT NOT NULL DEFAULT 'auto', -- 'auto'（workspace.toml に従う、無ければ host）| 'host' | 'container'
  is_primary INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, UNIQUE(project_id, name))
```

- `name` は案件内で一意の slug（既定はディレクトリ名）。**`is_primary`** は案件の「主なリポジトリ」（成果物と文書の既定の置き場。
  ADR-0044 D7）。1 案件に 1 つ。
- 既存の `projects.workspace`（ADR-0039）は migration で **`is_primary = 1` のリポジトリ 1 件に写す**（`kind` はパスが git なら
  `git`、でなければ `dir`。`mode = shared` だったものは `run = host` のまま `dir` として扱う）。`projects.workspace` 列は残すが
  書かない（API の `Project.workspace` は primary の写しを返す。GUI の後方互換）。
- API: `GET /projects/{id}/repos`、`POST /projects/{id}/repos`（管理系）、`PATCH /repos/{id}`、`DELETE /repos/{id}`
  （タスクが参照中なら 409）。`local.path` の `~` は展開して保存（ADR-0039 D5）。`remote.cluster` は `[[clusters]]` に無ければ 422。
- 秘書は案件の理解確認で「コードや文書はどこにあるか」を聞き（ADR-0039 D4 を拡張: 複数可）、人が GUI の案件画面「リポジトリ」で
  登録・編集する。

### D2. タスクは使うリポジトリを選ぶ。git のものはリポジトリごとに worktree

- `tasks.repos_json`（`Vec<RepoRef {repo_id, name}>`。migration 0012 で列追加）。**空なら親から継ぐ**（ADR-0039 D2 の規則と同じ:
  明示 > 親 > 案件の primary）。計画 run（`POST /projects/{id}/plan`）には案件のリポジトリ一覧（name / kind / 説明）を渡し、
  子タスクごとに `repos: ["benchfs"]` のように**名前で**指定させる（ADR-0028 の計画出力に `repos` を足す。知らない名前は 422 で
  計画を差し戻す）。人は GUI で編集できる（ADR-0044 D1）。
- タスクの足回りは常に **`<workspace_root>/<task_id>/`**（ADR-0042 D3。既定 `~/.local/celeris/workspaces/`）:

```
<workspace_root>/<task_id>/
  repos/<name>/     # git: worktree（ブランチ celeris/<task_id>）。dir: 実体へのシンボリックリンク
  artifacts/ inputs/ runs/ worktree.json
```

- cwd は**タスクの最初のリポジトリ**（`repos[0]`）の worktree。前置きの「作業場所」に全リポジトリを列挙する:
  「`benchfs` → `repos/benchfs/`（worktree、ブランチ `celeris/<id>`、base `main@<sha7>`）／`benchfs-paper` → `repos/benchfs-paper/`（同）／
  `data` → `repos/data/`（ディレクトリ。読み書き可。git ではない）」。base の規則は ADR-0041 D1（main / current / head）。
- `dir` はシンボリックリンクで見せる（コピーしない。大きいデータを想定）。コンテナ実行（D3）ではその実体をマウントする。
- worktree の後片付け（ADR-0041 D1）を改める: **終端で自動では消さない**。消えるのは D5 の取り込み（merge / discard）のとき、
  または**中止**（cancel）のとき（worktree を消し、ブランチも `git branch -D`。人の指示）。`done` で未取り込みのものは残る
  （差分を見るために要る）。`failed` も残す（人が見る）。
- リポジトリが 1 つも無いタスク（純粋な調査など）は従来どおり `artifacts/` だけ。

### D3. 実行環境: `host` か `container`（devcontainer 流）

- リポジトリの `run` が `container`（または `auto` で `workspace.toml` の `[run] mode = "container"`）なら、**そのタスクの
  ワーカー（ハーネスの CLI そのもの）をコンテナの中で起こす**。アダプタが起こすコマンド（`claude` / `codex` / `opencode` /
  `pqa` / python）を `runtime run --rm -i …` で包む（差し込み点は 1 か所: `task-worker::subprocess` のコマンド組み立て）。
  ハーネスの stdio 契約はそのまま使えるので、アダプタごとの変更は無い。
- タスクの全リポジトリのうち **1 つでも `container` なら、そのタスクの run はコンテナ**（環境は混ぜない）。イメージは
  primary → `repos[0]` の順で最初に `container` を要求したリポジトリのもの。
- コンテナの形（決定的）:
  - runtime: `[containers] runtime = "podman" | "docker"`（既定: `podman` があれば podman、無ければ docker、どちらも無ければ
    設定エラー）。rootless を想定し `--userns=keep-id`（podman）で**ホストと同じ uid** に見せる（マウントした worktree に
    そのまま書ける）。
  - マウント: `<workspace_root>/<task_id>/` を**同じパス**で（worktree もリンク先の `dir` 実体も同じパスに）。`~/.local/celeris/`
    の下は他を見せない。ハーネスの認証情報は**読み取り専用**で必要な場所にだけ（claude: アカウントの `CLAUDE_CONFIG_DIR`、
    codex: `CODEX_HOME`。アカウントプール（ADR-0024）が選んだものだけ）。`workspace.toml` の `[container] mounts` で追加
    （`/dev/infiniband:/dev/infiniband` など。benchfs のようにデバイスが要るなら**そもそも `run = host`** にする）。
  - ネットワーク: `--network host`（LLM の API と、手元の Qwen のポートに届く必要がある）。
  - 環境変数: アダプタが渡すもの（プロバイダの `env`、`[adapters.*].env`）＋ `workspace.toml` の `[container] env`。
  - 作業ディレクトリ: cwd と同じ。
- イメージ: `workspace.toml` の `[container] image = "…"` か `dockerfile = ".config/celeris/Dockerfile"`（リポジトリ相対）。
  Dockerfile はその内容の sha でタグを付けて `~/.local/celeris/containers/` でビルドし、キャッシュする（同じ内容なら再ビルドしない）。
  どちらも無く `mode = container` なら **Celeris の既定イメージ `celeris-worker`**（`deploy/containers/celeris-worker/Dockerfile`:
  Debian、node + claude/codex CLI、rust toolchain、python3 + uv、git、gh、rsync、ssh クライアント。`scripts/selfdeploy/` ではなく
  `scripts/containers/build-worker.sh` でビルド）。
- `setup`（D4）は worktree を作った直後に**その実行環境で**一度走る（結果は `runs/setup.log`。失敗したら run を始めずタスクを
  `blocked` にして人に聞く）。
- **ホスト実行（`host`）は従来どおり**。既定は `auto` → `workspace.toml` が無ければ `host`。
- 制約（この LXC）: podman / docker はある。rootless のオーバーレイ、`/dev/fuse` の有無、nesting は Phase で確認して
  PROGRESS に書く。動かなければ `run = container` のタスクは dispatch せず `blocked`「コンテナ runtime が使えません」。

### D4. リポジトリの中の設定 `.config/celeris/workspace.toml`（ADR-0042 D2）

```toml
[workspace]
name = "benchfs"                       # 任意。案件に登録するときの既定の name
description = "ad-hoc FS のベンチマーク（Rust）"   # 計画 run と前置きに出す

[run]
mode = "container"                     # "host"（既定）| "container"

[container]
image = "ghcr.io/…/rust-dev:1.90"      # か dockerfile = ".config/celeris/Dockerfile"。どちらも無ければ celeris-worker
mounts = ["/dev/infiniband:/dev/infiniband"]
env = { CARGO_TARGET_DIR = "/workspaces/.cargo-target" }

[commands]
setup = ["cargo fetch"]                                   # worktree 作成直後に一度
check = ["cargo test --workspace", "cargo clippy --workspace -- -D warnings"]   # 実装者とレビュー担当が使う

[outputs]
docs = "docs"                          # 文書ページの根（ADR-0044 D7）。既定 "docs"
deliverables = "."                     # コード以外の成果物（図・表・原稿）を置く根。既定はリポジトリのルート
```

- **書いてあることだけ使う**。言語から推定しない。無ければ全部既定。
- `check` は前置きに「このリポジトリの検査コマンド」として出し、レビュー担当の `Check::Command` の既定にもなる（タスクに
  `acceptance` が明示されていればそれが勝つ）。
- 読み取りは taskd 側（`task-core::workspace_config`。TOML、`deny_unknown_fields`、不正なら warn して既定）。

### D5. 変更の取り込み（intake）: 差分を見て、merge / PR / 捨てる

- `GET /tasks/{id}/changes` → リポジトリごとに `{repo, base, head, ahead (commits), files: [{path, status, +, -}], stat, dirty: bool}`。
  `GET /tasks/{id}/changes/{repo}/diff?path=` → unified diff（1 ファイル、200 KiB で切る）。git でない `dir` は対象外。
  コミットが無ければ `ahead = 0`（調査などコードを伴わないタスク。GUI は「変更なし」）。
- `POST /tasks/{id}/changes/{repo}/integrate {method: "merge" | "pr" | "discard", note?}`（管理系。人だけ。組織の「人」には出さない）:
  - `merge`: 一時 worktree で `rebase` を default_branch に試す → 成功なら **default_branch を fast-forward**。ただし人の
    チェックアウト（`local.path`）が default_branch を checkout 中で**未コミットの変更があるなら 409**「main が編集中」（人が
    片付けてから押す）。成功したら worktree を消し、ブランチを消す。`origin` があれば `git push origin <default_branch>` は
    **しない**（人のリポジトリを勝手に push しない。PR 方式を使う）。
  - `pr`: `git push -u origin celeris/<id>` → `gh pr create --base <default_branch> --title <task title> --body <生成: 目的・受け入れ・
    報告の要約・Celeris のタスクへのリンク>`。PR の URL・番号を `task_integrations` に記録。以後 GUI のタスク画面と案件画面に
    PR が出る。**同期は画面を開いたときだけ** `gh pr view --json state,mergeable,reviewDecision,url`（人の回答: 毎回確認して
    絶対同期しなくてよい）。「Celeris で merge」= `gh pr merge --merge --delete-branch`（方法は `[github] merge_method`）。
    merge されたら（開いたときに検知）worktree を消す。
  - `discard`: worktree とブランチを消す（確認付き）。
  - **rebase で衝突**したら: worktree はそのまま、**「衝突の解消: <題名>」タスク**を同じ担当（`assignee`）に自動で作る
    （親 = 元のタスク、repos 同じ、前置きに衝突ファイル一覧、受け入れ = `git status` が clean で rebase が完了していること）。
    それが `done` になったら人が再び `merge` を押す。
- 取り込みの権限は人だけ（SPEC §3.6）。組織の「人」が `main` を動かす経路は無い。
- `task_integrations(id, task_id, repo_id, method, state, pr_number, pr_url, merged_at, created_at)`（migration 0012）。

### D6. GUI での作業ツリー閲覧（読み取り）

- `GET /tasks/{id}/tree?repo=&path=`（一覧）/ `GET /tasks/{id}/tree/file?repo=&path=`（本文。テキストのみ 512 KiB まで、バイナリは
  サイズだけ）。`..` は 403（ADR-0003 D5 の規則）。`dir` のシンボリックリンク先も同じ API で見える。
- GUI: タスク画面の「ファイル」タブ（ツリー・本文・差分タブは D5）。

### D7. リモート (b): ログインノード上で編集も実行も（後続）

- `project_repos.location.remote` に `sync = "none"` かつ `run = "remote"` の組み合わせを **予約**する: ハーネスの CLI をログイン
  ノードで `ssh` 越しに起こし、worktree もそこに切る。CLI と認証情報がクラスタ側に要る。**この ADR の Phase では実装しない**
  （設定すると 422「未対応」）。(a)（ADR-0019: 手元の worktree を rsync、実行だけリモート）が既定。

### D8. 成果物と中間物の置き場（ADR-0042 D3 の適用）

- コード以外の成果物（図・表・原稿・文書ページ）は **primary リポジトリの `[outputs].deliverables` / `docs`** の下（既定はルート /
  `docs/`）。前置きにそのパスを書く。`artifacts/` は run の中間物・ログ・機械向けの `result.json` だけ。
- ADR-0036 の `.taskd/artifacts/` は作らない（読む側の互換だけ残す）。

## 3. 採らない

- 言語やファイル構成からの環境の推定。`workspace.toml` に書いてあることだけ。
- 組織の「人」が `main` に merge / push する経路。取り込みは人が押す。
- GitHub との常時同期。開いたときに `gh` で見るだけ。
- タスクごとに新しいコンテナイメージを作る。イメージはリポジトリ（Dockerfile の内容）単位。

## 4. 受け入れ条件（Phase 名は PROGRESS で振る）

- **A1（D1 / D2 / D4 / D6 / D8）**: `project_repos` と migration（既存 `workspace` の写し）、repos API、タスクの `repos` と継承、
  複数 worktree（`repos/<name>/`）と `dir` のリンク、`workspace.toml` の読み取りと前置き（description / check / outputs）、`setup`
  の実行（ホスト）、計画出力の `repos`、後片付けの改定（終端で消さない・cancel で消す）、ファイル閲覧 API。GUI: 案件画面の
  「リポジトリ」（追加・編集・primary）、タスク画面の「ファイル」タブ、案件フォームの複数リポジトリ。
- **A2（D5）**: changes / diff / integrate（merge・pr・discard・衝突タスク）、`task_integrations`、`gh` の有無と認証の検出、
  GUI のタスク画面「変更」タブ（ファイル一覧・差分・3 ボタン・PR の状態）と案件画面の PR 一覧。実機: 自己改善案件の 1 タスクの
  ブランチを GUI から PR にし、Celeris の「merge」で GitHub 側が merge される。
- **A3（D3）**: `run = container`、runtime 検出、`celeris-worker` イメージ、Dockerfile のビルドとキャッシュ、マウント・uid・
  ネットワーク・認証情報の規則、`setup` をコンテナで、runtime が無いときの `blocked`。実機: agent-platform 自身のリポジトリに
  `.config/celeris/workspace.toml`（`mode = container`）を置いて 1 タスクを回し、ホストに何も生えないことを確認。
- **A4（D7）**: 後続。
- どの Phase も `cargo test --workspace` / clippy / GUI 一式、PROGRESS の実機の証跡。

## Phase 54 追記（A2 の実装で D5 から離れたところ。2026-09-19）

D5 を実装して分かった 4 つ。**決定そのものは変えていない**（本文は読み替えない）。詳細と証拠は
`docs/PROGRESS.md` の Phase 54（P54-1〜P54-4）にある。

1. **`task_integrations` の列**（P54-1）: D5 の
   `(id, task_id, repo_id, method, state, pr_number, pr_url, merged_at, created_at)` に **`repo_name`**
   （タスクの中でのリポジトリの名前）、**`detail`**（人に見せる一行）、**`updated_at`** を足し、
   **`repo_id` を NULL 可**にした。Phase 49 の 1 リポジトリのタスクは `project_repos` の行を持たないが、
   そういうタスクのブランチも取り込む必要があり、API の URL（`/changes/{repo}`）も名前で引くため。
   migration は **0014**（版 13 は ADR-0044 B1 の `task_comments` が使う）。

2. **衝突の解消タスクの作業場所**（P54-2）: D5 は「worktree はそのまま、同じ担当でタスクを作る」としか
   書いていない。子に `repos` を持たせると**子が自分の worktree を切ってしまう**（別のブランチになる）ので、
   子は **`repos` を空にし、`workspace` を親の worktree のパス + `mode = "shared"`**（ADR-0041 D1 の逃げ道）に
   した。既存のディスパッチャがそのまま「そのディレクトリで走る」ので、ワーカープロトコルにも
   `worktree.json` にも新しい概念を足さずに済む。子の cwd は親の `repos/<name>/`、ブランチは
   親の `celeris/<parent_id>` のまま。

3. **API が `git` / `gh` を起こす**（P54-3）: ADR-0013 は「API はワーカーの起動・コマンドの実行をしない」と
   決めているが、D5 は `GET /tasks/{id}/changes` と `POST …/integrate` を API のエンドポイントとして
   要求している。そこで **`git` と `gh` だけ**を、待ち時間の上限付きで、**人が押したときと画面を開いたときに**
   起こすことにした（`task_ops::changes`）。ワーカーも LLM も起こさない。ディスパッチャ経由にしなかったのは、
   取り込みが tick とは無関係な人の同期操作だからである。

4. **`files` はコミットだけでなく作業ツリーまで**（P54-4）: D5 の `files` / `stat` は、`base` から
   **いまの作業ツリー**までの差分（コミット済み + 未コミット + 追跡外）にした。`dirty` と `ahead` が別に
   あるので情報は失われず、「ワーカーがコミットしなかったタスク」でも人が中身を見られる。worktree が
   無くブランチだけのときは `base..<branch>`（コミットだけ）になる。

## Phase 56 追記（A3 の実装で D3 から離れた／D3 が決めていなかったところ。2026-09-19）

D3 を実装して決めた 8 つ。**決定そのものは変えていない**（本文は読み替えない）。詳細と証拠は
`docs/PROGRESS.md` の Phase 56（P56-1〜P56-8）にある。

1. **`paperqa` / `local-deep-research` は常にホスト**（P56-1）: D3 は「アダプタごとの変更は無い」と
   言っているが、この 2 つは道具立てが**ホストの venv**（`uv` が作った `.venv`、`PAPERQA_*` と LDR の
   設定、埋め込みモデルの置き場）に生えていて、コンテナに入れると別物になる。そこで
   `container::HOST_ONLY_ADAPTERS` に入れ、`container::decide` が**リポジトリの設定に関わらず**
   この 2 つのタスクをホストに倒す。研究文献調査課・Web 調査課の運用は 1 バイトも変わらない。

2. **差し込み点は「1 関数」で、呼ぶ場所は 5 つ**（P56-2）: D3 は「差し込み点は 1 か所:
   `task-worker::subprocess` のコマンド組み立て」と書いているが、`subprocess.rs` を通るのは
   `fake` だけで、`claude-code` / `codex` / `acp` は自分で `Command` を組む。包む処理は
   **`container::wrap` の 1 関数**に閉じ（組み立て終えた `Command` を読み直して作り直す）、
   その 1 行を `subprocess.rs` / `claude_code.rs` / `codex.rs` / `acp.rs` / `workspace.rs`
   （`setup` と判定コマンド）の**コマンド組み立ての直後**に置いた。`wrap(cmd, None)` は恒等なので、
   ホスト実行は 1 バイトも変わらない。アダプタへの計画の渡し方は `WorkerAdapter::with_container`
   （`with_env` と同じ形。既定 `None`）。

3. **`blocked` の作り方は Phase 52 の `setup` 失敗と同じ経路**（P56-3）: D3 は「dispatch せず
   `blocked`」と書いているが、専用の dispatch 抑止を足すと「なぜ ready のまま動かないのか」が
   人に見えない。そこで **run を始める前に `Terminal::Question` を返す**（`setup` の失敗とまったく
   同じ形）。タスクは `blocked` になり、質問が人の受信箱に出る。ワーカーは起こさない。
   イメージのビルドが落ちたときも同じ（記録は `runs/container-build.log`）。

4. **`HOME` はタスクのディレクトリ**（P56-4）: D3 は環境変数について「アダプタが渡すもの ＋
   `[container] env`」としか言っていない。ホームは**マウントしない**ので、そのままだとコンテナの中の
   `HOME` が `/` になり、ツールが書き込みで転ぶ。`--env HOME=<task_dir>` を**いちばん先**に置いた
   （アダプタの env と `[container] env` で上書きできる）。

5. **認証情報は env から拾う**（P56-5）: D3 は「claude: `CLAUDE_CONFIG_DIR`、codex: `CODEX_HOME`」と
   アダプタ名で書いているが、アダプタを増やすたびに分岐が増える。実際には**アダプタが env に書いた
   パスが正**なので、`CLAUDE_CONFIG_DIR` / `CLAUDE_SECURESTORAGE_CONFIG_DIR` / `CODEX_HOME` /
   `OPENCODE_CONFIG`（ファイルなので親ディレクトリ）を見て、その場所だけを `:ro` で同じパスに渡す。
   読み書きのマウントの下にあるものは重ねない。

6. **ビルドの文脈は `.config/celeris/` の写しだけ**（P56-6）: D3 は「`~/.local/celeris/containers/` で
   ビルドしてキャッシュする」としか書いていない。リポジトリ全体を文脈にすると大きな案件で送信だけで
   分単位かかるので、`<build_dir>/<tag>/context/` に **`.config/celeris/` の中身だけ**を写して
   `build -f context/<Dockerfile> .` する。タグが `.config/celeris/` の中身の sha を含むのはこのため
   （文脈が変わればタグも変わる）。

7. **ビルドも `--network host`**（P56-8）: D3 は run のネットワーク（`--network host`）しか決めて
   いないが、**入れ子のコンテナ（この LXC）では `docker build` の `RUN` がブリッジを張れず**
   `OCI runtime create failed: recvfrom(PF_NETLINK)` で落ちる。run が `--network host` である以上
   ビルドを別のネットワークにする理由が無いので、`<runtime> build --network host …` にした。

8. **後片付けはラベル**（P56-7）: `--rm` と signal の転送で普通はコンテナも止まるが、`run` の
   クライアントだけを殺した場合（ADR-0044 のプロセスグループ kill）に取り残さないよう、
   `--label celeris.task=<task_id>` を付け、`container::stop_by_label`（`ps -aq --filter label=…` →
   `rm -f`）を用意した。`run_subprocess` は run の後始末で必ず呼ぶ。`ContainerStopper` trait で
   公開してあるので、ほかの kill の経路からも同じ口を呼べる。
