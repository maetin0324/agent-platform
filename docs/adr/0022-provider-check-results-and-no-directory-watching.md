# ADR-0022: 疎通確認の結果はスナップショットに置く。監視も管理画面も作らない

- 日付: 2026-09-16
- 状態: Accepted（人間の判断。P-58 / P-59 / G6-P1 への回答）
- 関連: ADR-0017（GUI からのアカウント管理）、ADR-0013 D4（スナップショットは観測値）、GUI 側 G6-P1

## 文脈

Phase 11（ADR-0017）で管理 API は揃ったが、3 つ保留が残っていた。

- **P-58**: `POST /providers/{id}/check` の結果が HTTP 応答にしか残らず、画面を閉じると消える。
- **P-59**: `providers.d/` を手で編集したとき `POST /reload` を忘れると反映されない。
- **G6-P1**: GUI にアカウント管理の画面（追加・編集・削除・疎通確認・ログイン手順の案内）が無い。

運用の前提（人間の回答）: **一人で使い、信頼されたネットワークで localhost に閉じる。**

## 決定

### D1. GUI のアカウント管理画面は作らない（G6-P1 は「作らない」で閉じる）

- 管理操作（`POST /providers`、`PATCH`、`DELETE`、`/reload`、`/check`）は **`curl` と設定ファイルの直接編集**で行う。
  管理 API は loopback でもトークンを要求する（ADR-0017 M3）ので、この運用でも認証の抜けは無い。
- GUI の `/providers` は読み取り専用のまま（一覧・使用量・cooldown・直近の疎通確認）。
- 将来、複数人で使う・遠隔から使う必要が出たら作り直す。そのときは ADR を改めて書く。

### D2. `check` の結果は `DaemonSnapshot` にだけ持つ（P-58 を採用、保存はメモリのみ）

- `ProviderLive.last_check = {at, result}`（`result` は `ok` / `auth_failed` / `throttled` / `spawn_failed`）。
  `GET /api/v1/providers` の各行と `GET /api/v1/daemon` に出る。
- **イベントにも DB にもしない**（ADR-0017 D2 のとおり、タスクに紐づかない観測値）。**taskd を再起動すると消える**。
  もう一度知りたければもう一度 `check` を押す（叩く）。
- `reload` でプロバイダ表を差し替えても、同じ id の `last_check` は保持する（確認した事実は設定の書き換えでは古くならない）。
  id が消えたら、その行の記録も消える。

### D3. `check` は手動のときだけ走らせる

- 起動時の一括確認も、定期実行もしない。1 回の `check` は**実際の API 呼び出しを 1 ターン消費する**
  （`claude -p "ok"` 相当、30 秒 / 1 ターン。ADR-0017 D2）ので、アカウント数 × 頻度だけ枠を食うのは割に合わない。
- 認証切れは、どのみち実際の run が `auth_failed` を返した時点で cooldown と受信箱に出る（ADR-0010 / ADR-0012）。
  `check` はその原因を人が確かめるための道具であって、監視の仕組みではない。

### D4. `providers.d/` のディレクトリ監視はしない（P-59 は却下）

- 反映は今までどおり `POST /api/v1/reload` の明示操作。ADR-0017 D1 の意図（run が飛んでいる最中に設定が勝手に変わらない）を保つ。
- `notify` / `inotify` の依存も、tick ごとの mtime 走査も足さない。

## 結果

- P-58 は D2 として実装する。P-59 と G6-P1 は「やらない」で閉じる。
- 実装が増えるのは `ProviderLive.last_check` と、それを埋める経路（check の完了 → tick ループ → スナップショット）だけ。

## 実装メモ（実機確認 2026-09-16 で修正）

- **M1（結果の分類が誤っていた）**: 実機で `check` が `spawn_failed` を返したが、同じアカウントで本番の run は成功していた。
  原因は「ワーカープロトコル上のエラー（`Terminal::Error`）を `spawn_failed` に写していた」こと。`check` が見たいのは
  **このアカウントで CLI が起動して応答するか**だけなので、`Terminal::Error` は `ok` とし、理由を `detail`（一行、200 文字まで）に入れる。
  起動できない・認証切れ・枯渇は `AdapterError` 側で分かる（`spawn_failed` / `auth_failed` / `throttled`）。
- **M2（ターン数）**: `max_turns = 1` では健全なアカウントでも `error_max_turns` になる（`artifacts/result.json` を書けないため）。
  3 ターンにして、正常時は `detail` にワーカーの返答（例「Confirmed ready; no files were changed.」）が出るようにした。約 15 秒。
- `ProviderCheckResponse.detail` と `ProviderLive.last_check.detail` を追加（追加のみ。v1 のまま）。
