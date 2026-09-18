# ADR-0039: 案件が作業場所（コードのある場所）を持ち、計画・委譲の子がそれを継ぐ

- 日付: 2026-09-18
- 状態: **Accepted**（人間の決定。本番で子タスクがワーカーに `ssh` させて人のリポジトリへ直接書いた事故を受けたもの）
- 関連: SPEC §2.1（成果物は普段のパス `~/workspace/rust/…` に）、SPEC §3.7 追記（**手元で編集してリモートで検証**）、
  ADR-0018 D1/D3/D4（Remote workspace: クラスタ側が正、写しは `workspace_root/<task_id>`、`.taskd/remote-exec`）、
  ADR-0019（`sync = worktree`: 大きいリポジトリの往復）、ADR-0033 D2/D4（案件・途中目標・分解）、
  ADR-0036 D1（成果物はタスクごと。共有 workspace では `.taskd/artifacts/<task_id>/`）、
  DESIGN §5.8（`WorkspaceSpec`）、`docs/protocol/worker-protocol.md`

## 1. 文脈 — 実機で起きたこと（本番、2026-09-18）

秘書が委譲した PoC の子タスク 2 件（`genre = coding-poc`、claude-code、`max_turns = 20`）が両方
`error_max_turns` で落ちた。run の中身を読むと、起きていたのはこうである:

- 子タスクは**空のローカル workspace** に置かれていた（計画タスクの workspace をそのまま継いだ
  `workspaces/<plan_task_id>/`。`plan::materialize` / `materialize_delegated` は
  `workspace: parent.workspace.clone()`）。
- ワーカーは、Pluvio のリポジトリが pegasus 上（`/work/NBB/rmaeda/workspace/rust/benchfs/lib/pluvio`）に
  あることを **objective の文面から**知り、**自分で `ssh pegasus` して**（人が張った ControlMaster を借りて）
  リモートの作業ツリーに直接 `examples/dpu_offload_poc/` を書き、ビルドまで進めて計測の直前でターンが切れた。

原因は 2 つある。

1. **計画・委譲の子に作業場所を指定する手段が無い**。案件（`projects`）は作業場所を持たず、
   `NewTask`（`plan.json`）にも `DelegateTask`（`delegate.json`）にも `workspace` が無いので、
   子は親の workspace を継ぐことしかできない。「このコードベースで作業せよ」は objective の**文面**でしか
   伝えられず、ワーカーは自分で場所を探しに行く。
2. その結果、ワーカーが taskd の同期機構（ADR-0018 / ADR-0019 の Remote workspace + `sync = worktree`）を
   **迂回して人のリポジトリに直接書いた**。SPEC §3.7 追記の「手元で編集してリモートで検証」に反し、
   taskd からは何が変更されたのか見えず、レビューも同期も効かない。

Remote workspace も worktree 同期も**既に実装されている**（Phase 12 / ADR-0019）。足りなかったのは
「案件にコードのある場所を持たせ、分解した仕事にそれを継がせる」という**繋ぎ**だけである。

## 2. 決定

### D1. 案件が作業場所を持つ（`projects.workspace`）

- `projects` に `workspace TEXT NULL`（JSON の `WorkspaceSpec`）を足す（migration 0010、`SCHEMA_VERSION = 10`）。
  導入前の案件・作業場所を決めていない案件は `NULL`（従来どおり）。
- `POST /projects` / `PATCH /projects/{id}` で受け、`GET /projects/{id}`（と `GET /projects`）で返す:

  ```json
  {"kind": "local",  "path": "~/workspace/rust/pluvio-poc"}
  {"kind": "remote", "cluster": "pegasus", "path": "/work/NBB/rmaeda/workspace/rust/benchfs"}
  ```

- `kind = remote` の `cluster` が `[[clusters]]` に無ければ 422（`validation_failed`）。存在しないクラスタを
  案件に書かせない（タスクを作った後に「クラスタが無い」で詰まらせない）。
- `PATCH` の `status` は**任意**になる（`workspace` だけを直せるように）。`workspace` を省略すれば
  変更しない。`"workspace": null` を明示すれば消す（案件を「作業場所なし」に戻す）。

### D2. 継承の規則（決定的。LLM は使わない）

計画 run と、その子（`plan.json` の `NewTask` / `delegate.json` の `DelegateTask`）の作業場所は

> **タスクが明示 > 案件の `workspace` > 親の workspace（従来）**

の順で決める。

- `NewTask` / `DelegateTask` に `workspace: Option<WorkspaceSpec>` を足す（プランナー／ワーカーが子ごとに
  別の場所を選べるように）。プロンプトには「この案件の作業場所は X。**別の場所が要るときだけ** `workspace` を
  書け」と出す（D3）。
- 案件の workspace が `Remote` なら、子も `Remote` になり、**従来の Remote 経路**（ADR-0018: 写しを
  `workspace_root/<task_id>` に作り、`.taskd/remote-exec` でクラスタ側にコマンドを流す。ADR-0019:
  `sync = worktree` で往復する）でそのまま動く。**新しい同期の仕組みは作らない**。
- 案件の分解を起こす計画 run（`POST /projects/{id}/plan`。ADR-0033 D4 追記）そのものにも、案件の workspace を
  与える（`Local` は `workspace`、`Remote` は `cluster` + `workspace`）。
- **対話 run（秘書との会話、途中目標のレビュー）には与えない**。会話は編集をしないので、人のリポジトリの中で
  走らせる理由が無い（従来どおりタスクごとの `workspace_root/<task_id>`）。

成果物の置き場所は **ADR-0036 D1 のまま**である: 案件の workspace を継いだ子は `parent_id` を持ち、
ディレクトリの末尾がタスク id ではないので「共有」と判定され、`.taskd/artifacts/<task_id>/` に書く。
人のリポジトリの直下に `artifacts/` を作らない（`.taskd/` は ADR-0018 D3 で同期から除外されている）。
分解を起こす計画 run（親なし）だけは従来どおり「所有」で `<workspace>/artifacts`。

### D3. プロンプトに作業場所を出す

- `RunContext` に `workspace_note: Option<String>`（決定的に組んだ 1 行。ディスパッチャが埋める）を足す。
  **案件が作業場所を持つ run にだけ** `Some`。無い案件・案件に属さないタスクでは `None` で、
  プロンプトは Phase 42 までと**バイト単位で同じ**。
- 前置き（`preamble::render`。`claude-code` / `codex` / `acp` が使う）に「## 作業場所」節を出し、
  **リモートの作業ツリーに `ssh` で直接書くな。編集は手元の作業ディレクトリで、検証はリモートで**を書く
  （`Remote` のときは `.taskd/remote-exec` の存在は ADR-0018 D3 の指示文が別に伝える）。
- 計画プロンプト（`build_plan_prompt`）には加えて「子タスクはこの作業場所を継ぐ。別の場所が要る子にだけ
  `workspace` を書け」、委譲の指示（`delegation_instructions`）にも同じ一文を足す（どちらも
  `workspace_note` があるときだけ）。

### D4. 秘書は「コードはどこにあるか」を最初に聞く

`[[roles]] secretary` の `instructions`（設定例）に一文を足す: 案件の理解確認（SPEC §7 の (a)〜(c)）で、
コードを扱う案件なら**作業場所（どのリポジトリか。手元か、どのクラスタか）を聞く**。これはプロンプトの仕事で、
taskd のコードは何も判定しない。

### D5. `~` はローカルの作業場所でだけ展開する

`WorkspaceSpec::Local { path }` の `~` / `~/…` は `$HOME` で展開し、絶対パスはそのまま使う
（SPEC §2.1「普段のパス `~/workspace/rust/…`」）。展開は**値が外から入るところ**（`POST` / `PATCH /projects`、
`plan.json` / `delegate.json` の `workspace`）で行い、DB には展開済みの絶対パスを入れる
（GUI・CLI・ディスパッチャが同じ文字列を見るため）。`Remote` の `path` の `~` は**クラスタ側の home** であり
taskd には展開できないので触らない。

相対パスの扱いは従来どおり（ディスパッチャが `workspace_root` 基準で解決する。ADR-0005 D3）。
`workspace_root` の外にある案件の作業場所（`~/workspace/rust/…`）でも、所有・共有の判定は D2 のとおり
ADR-0036 の規則をそのまま使う。

## 3. 採らない

- **taskd がリポジトリを clone する**: 案件の作業場所は人が決めた既存のパス。無ければワーカーが
  そこに作る（`git init` も仕事のうち）。taskd が勝手に clone すると「普段のパス」の意味が壊れる。
- **ワーカーの `ssh` を機械的に禁止する**（サンドボックス・コマンドの検閲）: ADR-0022 の方針どおり、
  taskd はワーカーの中を監視しない。**場所を与えて指示文で伝え、受け入れ条件で判定する**。
- **案件ごとに新しい同期の仕組み**: ADR-0018 / ADR-0019 の Remote workspace と worktree 同期をそのまま使う。
- **対話 run にも案件の workspace を与える**: 会話が人のリポジトリで走る必要は無い（D2）。

## 4. 影響

- `task_core::plan::materialize` / `task_core::materialize_delegated` の引数に
  `WorkspaceContext { project, home }` が増える（既定値は従来どおりの挙動）。
- `SCHEMA_VERSION` が 10 になる。migration 0010 は `ALTER TABLE projects ADD COLUMN workspace TEXT`
  の 1 行で、既存の行は `NULL` = 従来の挙動。
- `docs/protocol/worker-protocol.md`（`delegate.tasks[].workspace` / `plan.json` の `workspace` /
  `context.workspace_note`）と `docs/gui/api.md`（`POST` / `PATCH` / `GET /projects`）を更新する。
