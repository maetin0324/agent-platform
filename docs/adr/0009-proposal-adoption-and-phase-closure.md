# ADR-0009: 提案 P-1〜P-37 の採否、Phase 4/6 受け入れの締め方、Phase 7 の追加

- 日付: 2026-09-14
- 状態: Accepted（人間の判断。Phase 6 完了後のレビューで決定）
- 関連: `docs/DESIGN.md` 全体 / `docs/PROGRESS.md` 各 Phase の「提案」節 / [ADR-0010](0010-phase7-hardening.md)

## 文脈

Phase 0〜6 の実装中に `docs/PROGRESS.md` の「提案」節へ P-1〜P-37 が積み上がり、うち多くは
「採用まで実装で使う既定」のまま人間の判断待ちになっていた。また Phase 4（claude-code）と Phase 6（codex）の
実機ドッグフードは「人間による確認待ち」のまま完了扱いになっていた。人間（プロジェクトオーナー）が
選択肢から採否を選んだので、その結果をここに記録する。

## 決定

### D1. 受け入れ条件の締め方と運用ルール

| 事項 | 決定 |
|---|---|
| Phase 4 実機ドッグフード | エージェントが認証の使える環境で実行を試み、証跡を `PROGRESS.md` に残す。拒否された場合のみ人間に `!` で実行してもらう |
| Phase 6 codex ドッグフード | 実装・stub テスト・`codex exec --json` の直接実行による形式確認をもって完了とする。`taskd` 経由の実機ドッグフードは「利用可能な ChatGPT アカウントでは対応モデルが無い」という**外部制約により未実施**と明記する（受け入れ条件の免除として記録） |
| P-34（LLM 込みの確認の担当） | **採用。** 認証が使える環境ならエージェントが実行し証跡を残してよい。CLAUDE.md と DESIGN §6/§7 の文言を改める |
| DESIGN.md の扱い | 今回に限り人間の許可のもとでエージェントが編集する。採用した提案を該当節に反映し、Phase 7（仕上げ）を追加する。以後は従来どおり DESIGN.md の変更は人間の許可が要る |

### D2. 機能に関わる提案（Phase 7 で実装）

| # | 決定 |
|---|---|
| P-10 | **採用。** `taskctl answer` の回答を `Event::Answered{question, answer}` として永続化し、次 run の `context.answers` とプロンプトに載せる |
| P-17 / P-25 | **採用（専用フラグ）。** `taskctl add --check-cmd <cmd>` / `--check-artifact <name>` / `--check-reviewer <text>`。`--accept` は `Human` のまま |
| P-18 + P-4 | **採用。** `taskctl cancel <id>` を追加し、`Cancel` は非終端状態からのみ有効にする |
| P-5 | **不採用（P-18 で代替）。** draft への `reject` はエラーのまま。取り消しは `cancel` で行う |
| P-9 | **採用（自動 cancel）。** 先行タスクが `failed`/`cancelled` になったら、終端でない後続を同一トランザクションで `cancelled`（reason `dependency_failed`）にし、推移的に伝播する |
| P-35 | **採用。** Human check の `Approval` 子はレビューの試行（attempt）ごとに新規に作る |
| P-37 | **採用。** 親が終端になったら未決の `Approval` 子を同一トランザクションで `cancelled` にする |
| P-36 | **採用。** `ready_tasks` は SQL 段階で `kind = approval` を除外する |
| P-21 / P-29 | **採用。** `Requeue` トリガ（`running → ready`、attempts 据え置き）を追加し、供給側失敗（レート制限・認証失敗・枯渇・アダプタ起動失敗）に使う。Reviewer run の供給側失敗は `ReviewFail` にせず `reviewing` のまま延期する |
| P-6 | **実装どおりに確定。** 承認待ちの子は「`ready` だが dispatch されない」。DESIGN §4.2 と Phase 6 の受け入れ文言を合わせる（コード変更なし） |
| P-3 | **採用。** リトライの指数バックオフ `min(base·2^(attempts-1), cap)` を設定に追加 |
| P-7 | **採用。** `renew_lease` を追加し、ワーカーの出力があるたびにリースを延長する |
| P-19 | **採用。** `taskctl add/plan` の `--workspace` 省略時は `<workspace_root>/<task_id>/`（相対パス `<task_id>`）にする |
| P-30 | **採用。** `[reviewer]` 設定で Reviewer run の adapter / tier を指定できるようにする |
| P-20 / P-33 | **見送り。** `ProviderPolicy` trait の変更はモデル供給層の担当 |

### D3. 技術的負債（Phase 7 で解消）

- P-26: `claude-code` / `codex` も `runs/<run_id>/result.json`（正規化済み終端メッセージ）を書く
- ETXTBSY: 本番コードの `spawn_retrying` を削除し、テスト側（スクリプトの書き込み方法）で対処する
- `store.insert` + `Event::Created` の非原子性: 1 トランザクションで行う書き込み口を追加して置き換える
- `taskctl` の出力を閉じたパイプに流したときの panic を解消する

### D4. 実装済みの挙動を DESIGN.md に追認する提案（一括採用）

P-1, P-2, P-8, P-11（`run_id`/attempt はプロンプト文面に埋め込む形で）, P-13/P-24（結果ファイル規約）, P-14,
P-15, P-16, P-22, P-23, P-27, P-28, P-31, P-32 を DESIGN.md の該当節に反映する。

P-12（`evidence[]` の各フィールドを任意に）は実装済みではない（CLI 系アダプタが寛容に読むだけで、
プロトコル上は必須のまま）ため一括採用の対象外とし、提案として残す。

## 結果

- `docs/DESIGN.md` を改訂する（採用分の反映、Phase 7 の追加）。
- `CLAUDE.md` の「LLM呼び出しを伴う確認はPhase 4/5/6で人間が行う」を P-34 に合わせて改める。
- Phase 7 の実装方針は ADR-0010 に定める。

## Phase 4 / Phase 6 の締め（実施結果）

本 ADR 作成時点で実施した内容と結果は `docs/PROGRESS.md` の「Phase 4/6 受け入れの締め（2026-09-14）」節に記す。
