# agent-platform / taskd

## 最初に読むもの（毎セッション）
1. `docs/DESIGN.md` — 実装方針。ここに書かれた設計原則とPhase順は変更しない
2. `docs/PROGRESS.md` — どのPhaseまで終わっているか。ここが現在地
3. `docs/adr/` — 過去の設計判断。矛盾する変更をしない

## 作業の進め方
- 今回のPhaseだけをやる。次のPhaseの準備を先回りしない
- 設計判断をしたら `docs/adr/NNNN-*.md` を追加してから実装する
- 各Phase完了時に必ず:
  - `cargo test --workspace` と `cargo clippy --workspace -- -D warnings` を実行し、出力の要点を報告に含める
  - `docs/PROGRESS.md` を更新（完了日、証拠コマンドと結果、未解決事項、提案）
  - `git add -A && git commit -m "phase N: <summary>"`
- 同じアプローチを3回失敗したら、`docs/PROGRESS.md` に状況を書き、報告本文で人間に質問する
- テストで外部ネットワークに出ない。LLM呼び出しを伴う確認はPhase 4/5/6で人間が行う

## 禁止
- ディスパッチャやストアにLLM呼び出しを入れる
- `unwrap()`（テスト以外）
- Web UI、リモート実行、予算管理の実装（別プロジェクト）
- `docs/DESIGN.md` の書き換え（提案は `PROGRESS.md` の「提案」節へ）

## 完了報告の書き方
受け入れ条件ごとに「条件」「実行したコマンド」「出力の要点（exit code、テスト数、差分ゼロなど）」を箇条書きで本文に書く。
評価器はトランスクリプトしか見ないので、ファイルに書いただけでは完了と判定されない。