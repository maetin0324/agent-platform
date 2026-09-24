# ADR-0068: Knowledge GC と Repository Documentation Maintenance

---
tasks: [01M38FXZZVDNY2VQS2R2ZDWYX0]
---

日付: 2026-09-24。状態: 採用。

## 境界

Knowledge は組織横断の再利用可能な意味的知識であり、Git 管理 Markdown を正本とする。
repository docs はそのリポジトリの人間 contributor に役立つ現在の仕様・手順の正本である。
task artifacts は監査、判断材料、実験、作業履歴の証拠である。場所が docs/ であるだけでは
正しさや authority の証明にならない。agent 内部の大量 context や一回限りの報告は自動で docs に昇格しない。

## Knowledge GC

既存 task-triggered maintenance と独立した、既定無効の周期処理とする。
Rust scanner が低 confidence、review 経過、巨大ページ、同 scope の title/tags 類似、
同 source の重複疑いを決定的に検出し、局所近傍の小 batch を選ぶ。
ページ数と文字数の両方を制限し、全 title の先頭 N 件や全 KB の巨大 prompt に依存しない。
skills/、_inbox/、_retired/ は対象外。

デーモンは support task の作成と結果適用のみを行い、LLM は既存 adapter/harness で実行する。
入力した KB の整理のみを許し、外部サイト・cluster の調査、新規知識の創作、create は禁止する。
既存 Candidate validation を通し、GC update/merge/retire は confidence に関係なく inbox に送る。
人間の accept/reject を必須とし、既存 task-triggered high-confidence create/update は維持する。

content hash と review 時刻、周期・実行中情報は再生成可能な derived state に置く。
review だけで canonical の updated を変えず、commit を作らない。失敗を review 成功扱いにしない。
同 hash の recent review を除外し、変更後は再対象とする。GC の失敗は通常 dispatch を止めない。
将来の外部事実 verification は別 task とし、GC の情報整理権限を暗黙に拡張しない。

## Repository lifecycle

audit は Git snapshot を読み取り、README/CONTRIBUTING/AGENTS/docs 等の inventory と
conventions を観測する。作業ツリーの dirty/untracked ファイルを変更しない。
normalized view は canonical/current、architecture、reference、decision、active plan、historical、
generated、residue、duplicate/superseded candidate、unknown を根拠と confidence 付きで表す。
分類は変更許可ではなく、authority が不明なものは unknown のままにする。

reconcile は exact な入力 snapshot と具体的操作を持つ plan である。人が内容を確認・承認した plan
だけを隔離 worktree で適用し、既存レビュー・merge 経路へ渡す。承認後に入力が変わった場合は拒否する。
削除、移動、複数 canonical の決定を未承認で行わない。default branch へ直接適用しない。
履歴は Git に残し、不要な文書を一律 docs/archive に移すことはしない。

adopt は Celeris 側 overlay に repo ごとの policy を保存する。物理 layout を強制しない。
observe は読み取りと overlay のみ、conservative は既存 convention 内で承認された変更のみ、
managed はこれらに定期 scanner と候補限定 semantic review を追加する。
managed も自動 main 更新の許可を意味しない。
canonical/category、generated、historical、plans、agent instructions、authority mapping を保持する。
AGENTS.md/map の opt-in publish と applies_to による change-driven freshness は将来拡張とする。

Doc Gardener は broken link、巨大文書、duplicate title、stale active plan を決定的に検査する。
generated drift は生成元等の根拠がある場合のみ判定し、不明な場合は未確認とする。
semantic review は候補の限定 context のみを読み、成果を artifact に返す。
Knowledge GC と設定・state・正本・適用 policy を共有しない。

## 実行と失敗の取り扱い

GC は DB に隣接する `*.knowledge-gc.json` へ起票前に interval を予約する。
起票失敗・抽出失敗も interval を消費し、同 tick で繰り返さない。
実行中 task ID に加え永続 task label を調べるため、再起動や derived state 消失でも
残っている実行中タスクを重複起票しない。起票後 snapshot 保存前に落ちた orphan の結果は適用しない。
有効な成功出力だけを review 履歴へ反映し、不正/入力外候補がある run は確認済みにしない。

Doc Gardener は `[docs_maintenance]` と repo policy の managed opt-in の双方を要求する。
state は `CELERIS_STATE_DIR/repository-docs/`（未指定時 `~/.local/celeris/repository-docs/`）。
周期内重複は scan marker と永続 task label で防ぎ、実行中があれば周期を越えても起票しない。
semantic run は repo を継承しない scratch workspace とし、明示的に承認した exact plan の
apply だけが repo-bound な検証 task の worktree を作る。draft の間に marker を接続してから
既存の起動・review・merge 経路へ渡し、通常 dispatcher はその branch/base を保持する。

承認は CLI/API の人間操作で行う。初期版では全 reconcile 操作で承認を必須にし、
小変更と破壊的変更の閾値誤判定を避ける。authority の選択は policy overlay に記録し、
その判断に伴う文書変更は concrete な rewrite/move/delete 等として別途承認する。
