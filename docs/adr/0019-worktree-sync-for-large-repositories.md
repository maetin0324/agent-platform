# ADR-0019: 大きなリポジトリのための git worktree 同期（ADR-0018 D4 の改訂）

- 日付: 2026-09-15
- 状態: Accepted（人間の判断「git worktree を切る方針で。git 管理外のプロジェクトはほぼ存在しない」）
- 関連: ADR-0018（クラスタでのコマンド実行）、DESIGN §5.9 補足 2 / §6 Phase 12、PROGRESS「実機確認」の P-60

## 文脈

ADR-0018 D4 は「クラスタのディレクトリを丸ごと手元へ pull → 編集 → push」とした。実機で確かめたところ、
人間が実際に作業しているプロジェクトではこれが成り立たない。

`/work/NBB/rmaeda/workspace/rust/benchfs`（pegasus）の実測:

| | 大きさ |
|---|---|
| 全体 | **263 GB** |
| `target/` | 30 GB |
| 追跡ファイル（`git ls-files`、258 件） | **41 GB** |
| うち `lib/pluvio/examples/mpi_example/results/`（ベンチ結果） | **39 GB** |
| ソース（`src/`） | 2.2 MB |

- 未追跡のデータが 220 GB 以上あり、丸ごとの rsync は論外。
- 追跡ファイルに限っても 41 GB で、その 95% が 1 つの結果ディレクトリ。
- リポジトリは git 管理下で、作業中の変更は 1 件（未追跡ディレクトリ）だけだった。

人間の方針: **git worktree を切る**。git 管理外のプロジェクトはほぼ無い。

## 決定

### D1. `sync = "worktree"` を足す（`rsync` / `none` に続く 3 つ目）

`[[clusters]] sync = "worktree"` のとき、run 1 回の流れは次のようになる。

1. **クラスタ側で worktree を作る**: `git -C <project> worktree add -B taskd/<task_id> <worktree_dir> <base>`
   （既にあれば再利用。`base` は設定 `worktree_base`、既定は `HEAD`）。
   - `worktree_dir` は `worktree_root`（既定 `<project>/.taskd-worktrees`）の下の `<task_id>`。
   - **追跡ファイルだけが checkout される**ので、未追跡の巨大データは最初から入らない。
2. **絞り込み（任意）**: `worktree_paths` があれば sparse-checkout を使う
   （`git sparse-checkout set --cone <paths>`）。`benchfs` なら `src Cargo.toml lib/pluvio/src` のように指定すると
   41 GB が数 MB になる。
3. **pull**: worktree → 手元の写し（`rsync`。除外は ADR-0018 D4 と同じ + `rsync_excludes`）。
4. ワーカー（手元の LLM）が写しを編集する。
5. **push**: 写し → worktree（既定では削除しない）。
6. **判定**: `Check::Command` を **worktree の中で**実行する（元のプロジェクトのディレクトリでは実行しない）。
7. **pull**: 判定で生まれた成果物を取り込む。

### D2. 変更は worktree のブランチに残す。taskd はコミットしない

- ブランチ名は `taskd/<task_id>`（`-B` で作る）。**taskd は commit も push もしない**。変更は worktree の作業ツリーに残る。
- 人間が結果を見てから `git -C <worktree> diff` / `commit` / `merge` する。`TaskDetail` と API に worktree のパスとブランチ名を出す。
- 後片付け（`git worktree remove`）も人間の操作。taskd は自動で消さない（実行結果を消してしまわないため）。
  設定 `remove_worktree_when = "never" | "done"` を用意し、既定は `"never"`。

### D3. 元のプロジェクトのディレクトリは触らない

- `sync = "worktree"` では、taskd が書き込むのは worktree の中だけ。`WorkspaceSpec::Remote{path}` が指すのは
  **元のリポジトリ**で、そこから worktree を切る。ADR-0018 D4 の「既存プロジェクトに `artifacts/` が作られる」問題も消える。
- git リポジトリでないディレクトリに `sync = "worktree"` を指定したら、設定エラーではなく**その run の供給側失敗**にする
  （`git rev-parse --git-dir` が失敗する）。`sync = "rsync"` に変えるよう促すメッセージを出す。

### D4. どのモードをいつ使うか（運用の指針）

| モード | 使う場面 |
|---|---|
| `worktree` | **既定の選択**。git 管理下のプロジェクト。巨大な未追跡データがあっても安全 |
| `rsync` | git 管理外の小さなディレクトリ。taskd 専用の作業ディレクトリ |
| `none` | 手元とクラスタでファイルシステムが共有されている場合 |

## 結果

- `[[clusters]]` に `sync = "worktree"`、`worktree_root`、`worktree_base`、`worktree_paths`、`remove_worktree_when` を足す。
- `SshWorkspace` に worktree の準備（`git worktree add` と sparse-checkout）を足し、以降の同期・コマンド実行の対象を worktree にする。
- `TaskDetail` / `GET /tasks/{id}` に `worktree`（パスとブランチ）を出す。
- テストは localhost の git リポジトリで行う（外部ネットワークに出ない）。実クラスタでの確認は人の操作を伴う。
