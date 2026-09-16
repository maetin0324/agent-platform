# ADR-GUI-0012: プロバイダの登録と Claude アカウント（プール・ログイン・残量）の画面

- 日付: 2026-09-16
- 状態: **Accepted**（人間の依頼「GUI からプロバイダを登録できるようにして下さい」ほか。taskd 側 ADR-0024）
- 関連: taskd 側 docs/adr/0024（設計の正）, 0017, 0022 / docs/taskd-api-v1.md §3.19, §3.24〜3.35 / docs/adr/0005（action と flash）, 0008（認証・CSRF）, 0011（見た目）

## 1. 文脈

G6-P1（アカウント管理画面）は taskd 側 ADR-0022 D1 で「作らない」と閉じていたが、人間の依頼で作ることになった（taskd 側 ADR-0024 が上書き）。
API（管理系 5 本 + アカウント 7 本）は taskd が提供する。GUI は表示と操作の中継だけを行い、選択・残量の計算はしない（CLAUDE.md の禁止事項）。

## 2. 決定

### D1. 管理 API は BFF がトークン付きで呼ぶ。トークンが無ければ画面で案内し、GUI 側で回避しない

`TaskdClient` に `patch` / `delete` を足す。管理系が 401 を返したら flash に「`TASKD_API_TOKEN_FILE`（taskd の `[api] token_file` と同じ内容）を設定して GUI を再起動してください」と出す。
トークンはブラウザに出さない（従来どおり）。

### D2. `/providers` に追加・編集・削除・疎通確認。変更の後は同じ action で `POST /reload` まで行う

- フォーム: `id` / `adapter`（select）/ `tiers`（チェックボックス）/ `concurrency` / `model` / `account_pool`（claude-code のときだけ意味がある旨の説明）/ `env`（`KEY=VALUE` の行。**値は再表示しない**。編集時は「空欄なら変更しない」）。
- 追加・変更・削除が 2xx なら続けて `POST /reload` を呼び、両方の結果を flash に出す（reload が 400 なら「設定は書き込まれたが反映に失敗」と taskd の文言をそのまま出す）。
- 削除は確認付き（`<details>` で開くフォームの中の送信ボタン。JS の `confirm()` は使わない）。

### D3. `/accounts`（ナビ「運用」）

- `GET /accounts` をそのまま描く: ログイン状態、5 時間枠・週次枠の使用率バー（`utilization` × 100% とリセット時刻）、`score` と `excluded_reason`、実行中、cooldown、最後の確認、集計。
  値の再計算（スコアやリセット判定）はしない。`observed_at` の相対時刻表示だけは表示のための変換として行う（ADR-GUI-0006 の time-delta と同じ扱い）。
- 操作: 追加（`POST /accounts`）、ログイン（`POST .../login` → URL をリンクで表示 → コード入力欄 → `POST .../login/code`）、残量を確認（`POST .../check`）、ログインの中止、削除（確認付き）。
- ログインの URL は action の戻り値（`actionData`）でだけ保持し、クッキーやストレージに置かない。コードの入力欄は `autocomplete="off"`。
- 画面に「認可コードは平文 HTTP を通るので、信頼できるネットワークでだけ使う」と明記する（taskd 側 ADR-0024 D7）。

### D4. テスト

- unit: action（成功・401・409・422・reload 失敗の flash）、loader（mock taskd）。
- e2e（`e2e/g8.spec.ts`）: `scripts/taskd.sh fixture accounts` で、トークン付き・`[accounts]` 付き・スタブの `claude`（`auth login` と `-p` を模す）の taskd を作り、
  GUI からプロバイダ追加 → reload 反映、アカウント追加 → ログイン（スタブの URL、正しいコード）→ `logged_in`、確認 → 使用率の表示、削除を行う。
