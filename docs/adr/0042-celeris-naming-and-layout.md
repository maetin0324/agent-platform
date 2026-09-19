# ADR-0042: この基盤の名前は Celeris。リポジトリ内設定は `.config/celeris/`、手元の中間物は `~/.local/celeris/`

- 日付: 2026-09-19
- 状態: **Accepted**（人間の指示「`.taskd/` だとちょっと名前がダサいのでこの基盤の名前は celeris にすることにして、
  `.celeris/` みたいなのを増やしすぎるとよくわからなくなるので、各リポジトリには `.config/celeris/workspace.toml` みたいな
  感じで設定を置けるようにして下さい」「タスクの中間管理ドキュメントなどは `~/.local/celeris/` などに適宜」）
- 関連: SPEC §5（成果物は `~/workspace/...`）、ADR-0036（成果物の置き場）、ADR-0040（リリースの置き場 `~/taskd/releases`）、
  ADR-0043（ワークスペース）、ADR-0044（タスク管理）

## 1. 決定

### D1. 名前

- 基盤の名前は **Celeris**。GUI の表示名・文書・新しく作るパスは Celeris を使う。
- **crate 名・バイナリ名（`taskd` / `taskctl`）・systemd unit 名（`taskd@`）・API のパス・`~/taskd/`（設定と DB とリリース）の
  改名は、この ADR では行わない**。ADR-0043 / 0044 の機能の工事と混ぜると差分が読めなくなるため、それらが本番で動いた後に
  「改名だけの Phase」を 1 つ切る（`celeris` / `celerisctl`、`celeris@.service`、`~/.config/celeris/config.toml`、
  `~/.local/celeris/`。旧パスからの移行スクリプト付き）。人が別の順を望めばそれに従う。

### D2. リポジトリの中の設定は `.config/celeris/` に集める

- リポジトリごとの設定は **`<repo>/.config/celeris/workspace.toml`**（ADR-0043 D4）。Dockerfile を置くなら
  `<repo>/.config/celeris/Dockerfile`。**ルートに `.celeris/` や `.taskd/` は作らない**。
- 既存の `<workspace>/.taskd/artifacts/<task_id>/`（ADR-0036 の「作業場所を共有するときの成果物の置き場」）は、
  ADR-0043 D2 でタスクの作業場所が常に `~/.local/celeris/workspaces/<task_id>/` になるので**使われなくなる**。
  互換のため読む側は残し、書く側は作らない。

### D3. 手元の中間物は `~/.local/celeris/` の下に生やす

```
~/.local/celeris/
  workspaces/<task_id>/            # タスクの足回り（ADR-0043 D2）: repos/<name>/（worktree）, artifacts/, inputs/, runs/, worktree.json
  containers/                      # 作業環境イメージのビルドキャッシュ（ADR-0043 D3）
  docs/                            # （将来）案件に紐付かない下書きなど
```

- `taskd.toml` の `workspace_root` の**既定値**を `~/.local/celeris/workspaces` にする（従来は設定ファイル基準の `workspaces`。
  明示してあればそのまま）。本番は配備時に `workspace_root = "/home/rmaeda/.local/celeris/workspaces"` に直す。
- 成果物（人が読むもの、論文、図、文書ページ）は**案件のリポジトリの中**（ADR-0043 D6 / ADR-0044 D7）。中間物（run のログ、
  worker の入出力、レビューの記録）は `~/.local/celeris/workspaces/<task_id>/` にだけ置く。**リポジトリを汚さない**。
- worktree のブランチの接頭辞の既定は `celeris/`（ADR-0041 D1 の `taskd/` を改める。`[workspace] worktree_branch_prefix` で変えられる）。

## 2. 採らない

- 今すぐの全面改名（D1 の理由）。
- リポジトリのルートに `.celeris/` を置く。
