# ADR-0023: 接続確認の間引き、run の指示の保存、「部下待ち」の可視化

- 日付: 2026-09-16
- 状態: Accepted（人間の判断。P-49 / P-50 / P-55 への回答）
- 関連: ADR-0018 D2（クラスタの多重接続）、ADR-0013 D4（スナップショット）、ADR-0016 D2 / M5（委譲と子待ち）、ADR-0003（ワーカープロトコル）

## 文脈

Phase 12 と Phase 10 の監査で挙がった 3 つの積み残し。

- **P-49**: `ssh -O check` を**毎 tick・全クラスタ**、tick の中で同期的に実行している。1 回は unix ソケットを見るだけだが、
  ディスパッチャを止める種類の仕事で、クラスタ数に比例する。
- **P-50**: ワーカーが実際に受け取った `RunRequest`（objective + 役割の指示文 + クラスタ用の追記 + 前回の判定 + 人の回答 + 子の結果）が
  どこにも残らない。`runs/<run_id>/` には出力（`stdout.jsonl` / `stderr.log` / `result.json`）しか無く、「何を言われたか」を後から確認できない。
- **P-55**: 委譲した子を待っている親は `reviewing` のままで、「自分の判定待ち」と「部下待ち」が画面上で区別できない。

## 決定

### D1. クラスタの接続確認は 5 秒に 1 回にする（P-49）

- `Dispatcher` は前回の確認からの経過を見て、**5 秒未満なら `ssh -O check` を飛ばして前回の結果を使う**（最初の tick では必ず確認する）。
- `tick_ms` が 5 秒より長ければ毎 tick の確認になる（間引きは「tick より細かく確認しない」だけ）。
- dispatch は最大 5 秒古い判定を使うが、外れても結果は変わらない: 接続が実は切れていれば ssh が 255 を返し、
  **供給側失敗**として cooldown に入るだけ（attempts は消費しない。ADR-0018 D2）。
- 「ready な Remote タスクがあるクラスタだけ確認する」案は採らない。アイドル中に `GET /clusters` の `connected` が古くなり、
  接続切れに人が気づけなくなる（ADR-0018 D5 / P-47 の価値が下がる）。

### D2. `RunRequest` を `runs/<run_id>/request.json` に残す（P-50）

- `run_subprocess` が、アダプタの stdin に書くのと同じ `RunRequest` を**整形して**同じディレクトリに書く。
  ワーカー（claude-code / codex / fake）の別を問わず同じ経路に乗る。
- **秘密は入らない**: `RunRequest` は `task` / `workspace` / `context` だけで、`[[providers]].env` の値やトークンは含まれない（ADR-0013 D11）。
- API とGUI からも読めるようにする: `GET /api/v1/tasks/{id}/runs/{run_id}/request`（`application/json`）と `RunSummary.files.request`。
  既存の `stdout` / `stderr` / `result` と同じ扱い（パス検査・`canonicalize`・作業ディレクトリ外を返さない）。
- 同期からは外れたまま（`runs/` は `SYNC_ALWAYS_EXCLUDED`）。クラスタのタスクでも request.json は**手元の写し**にだけ残る。

### D3. 子待ちの親をスナップショットに出す（P-55）

- `DaemonSnapshot.awaiting_children: Vec<TaskId>`（id 昇順。`awaiting_human` と同じ形）。ディスパッチャが既に持っている
  `awaiting_children` の鍵をそのまま出すだけで、DB には書かない（観測値）。
- GUI はデーモン画面に一覧を出し、DAG のノードには「部下待ち」を添える。**GUI 側で状態を組み立て直さない**
  （`reviewing` かつ子が非終端、という再計算はしない。GUI の CLAUDE.md の禁止事項）。

## 結果

- tick の中の同期処理が減る（クラスタ 2 つ・tick 2 秒なら `ssh -O check` は 1 秒あたり 1 回 → 0.4 回）。
- 「このワーカーは何を言われて何をしたか」が run ごとに 1 組（`request.json` と `stdout.jsonl`）で追える。
- 木構造で走らせたとき、親が何を待っているのかが画面で分かる。
- API はエンドポイントが 1 本増える（追加のみ。v1 のまま）。
