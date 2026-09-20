---
tasks: [01M305NG9QRX7HE59VF6K3FZ7H]
---
# 認証アカウントの分離と階層・残量によるモデル実行

## 実装と反映範囲

作業ブランチ `celeris/01M305NG9QRX7HE59VF6K3FZ7H` に実装。バックエンドのコミットは `042d024`。運用中の設定・認証ファイル・サービス、元リポジトリ、リモート作業ツリーは変更していない。取り込みとリリースへの反映は未実施。

- アカウント画面: 既存のClaude／GPTログイン、利用枠、APIキーの秘密ストアを認証情報の管理場所として維持。
- プロバイダー画面: Claude／GPTを明記し、階層ごとの希望名・実行ID・未対応理由、認証参照IDを編集・保存。秘密の値を入力する欄を削除。
- `tier_models` を設定・管理API・スナップショット・生成スキーマ・GUIに追加。名前と実行IDを分離し、保存されたIDをClaude Code／Codexの `--model` に渡す。
- `account_id` は同じadapterの認証プール内のID。省略時は従来の自動選択、指定時は他アカウントに置換しない。`account_pool=true` が必要。
- `credential_refs` はアカウント画面のAPIキーIDへの参照。管理APIで許す環境キーは `ANTHROPIC_API_KEY`、`ANTHROPIC_AUTH_TOKEN`、`OPENAI_API_KEY`、`CODEX_API_KEY` のみ。既存の `env_from_secrets` として保存し、値は応答しない。参照先が欠損・読み取り不能なら実行を止める。

## 希望名とモデルID

| 種類 | frontier | standard | cheap |
|---|---|---|---|
| Claude | fable | opus | sonnet |
| GPT | astra | sol | luna |

これらはGUIの名称初期値。リポジトリには、この6名称と現在利用可能な実行IDを保証するカタログがない。既存CLIアダプターは `--model` に指定IDを渡す構造であり、希望名を実行IDと同一視しない。

[設定例](../config/celeris.model-tiers.example.toml)は全6件を「実行モデルID未確認」として記載。利用可能なIDを確認後、該当する `model_id` を設定し `unavailable_reason` を解除する。未設定・階層欠落・無効書式・明示的未対応は起動前に停止し、理由をイベントに残す。停止したタスクは設定修正後に再開する。外部サービスで拒否されるIDの完全な事前検証は行わず、CLIのエラーを既存経路で報告する。他モデルへ置き換えない。

## 互換性と移行

- `tier_models` が無い旧設定は空として読み、従来の `provider.model → adapter.model` を保持する。階層別設定を有効にした後は、欠落階層を共通モデルへ戻さない。
- `providers.d` の管理APIで追加・保存する際、秘密ストアが有効なら旧 `env` の上記認証キーを0600の秘密ファイルへコピーし、プロバイダーは参照だけにする。先に秘密を書き、その後プロバイダーファイルを既存の原子的保存で更新する。失敗時に元ファイルを消さない。既存参照が優先される場合も、隠れていた旧値は秘密ストアに保持する。
- 秘密ストアが未設定なら旧値を保持する。メインTOMLやアダプター共通の認証設定を起動時に書き換える移行はしない。旧アカウントディレクトリ、provider ID、並列度、専用アダプター設定、一般環境変数、既存秘密参照は保持。
- APIキー利用ではプールを無効にしてキーIDを参照。サブスクリプション利用ではプールを有効にしてアカウントIDを参照する。秘密の編集・モデル対応の保存後はGUIがreloadを呼び、次の実行から反映する。

## 難易度と予算

CoSの `create_task` に `tier` を追加し、既存の部署管理者の `delegate.tasks[].tier` と同じタスクの `worker_hint.tier` に接続。指示文には cheap=定型、standard=通常実装、frontier=難しい設計・調査を記載。指定が無ければ既存の役割・分野の既定を保持する。

実行直前に選択アカウントの `AccountBook` を参照。Claude stream／Codex利用枠確認の `RateLimitObservation` の5時間・7日利用率が取得元。単位は利用枠に対する割合で、金額・トークン残数ではない。タスクの `Budget.max_turns`、`max_wall_secs`、`max_retries` は別の実行制限であり、課金残高に換算しない。

| 観測した残量 | 階層の扱い |
|---|---|
| 不明 | 指定階層を保持し、不明と記録 |
| 30%超 | 指定階層を保持 |
| 10%超〜30% | frontierをstandardへ制限 |
| 3%超〜10% | cheapへ制限 |
| 3%以下 | 実行待機。既存プール選択も利用率97%以上を除外 |

両方の枠が存在し、未リセットかつ観測から300秒以内の場合だけ、`1 - max(利用率)` を使用。片方欠落・期限切れ・古い観測・不正値は不明。残量から階層を変えるのは明示的な階層別対応があるプロバイダーのみ。新たな階層が未対応なら停止する。APIキー等で利用枠が観測できない場合も、残高を推定しない。

選択理由・実行階層は `WorkerProgress`、実モデル・provider・認証アカウントは `WorkerStarted` に保存。DBからワーカー起動用タスクを再読込する際にも、選択済み階層を引き継ぐ。DBの要求階層は保持し、再試行時は残量を再評価する。

## 検証

- `cargo test --workspace`: 1,522件成功、3件ignore（SSH・手動／doc検査）。既存委譲、アカウントプール、API、専用ハーネス等の回帰を含む。
- `cargo clippy --workspace -- -D warnings`: 成功。
- GUI型検査・本番ビルド、設定・アカウント関連Vitest: 42件成功。
- 追加テスト: 各階層のID解決、未対応・無効ID、旧設定と秘密の移行・0600・失敗時の保持、API往復・非公開値の非露出、CoSの指定、観測欠落・期限切れ、難易度と残量の組み合わせ、起動前停止。
- 実行経路: CLIスタブの実引数でClaude／GPT双方の全階層の `--model` と認証環境を確認。ディスパッチャーでは残量90%・20%・5%・不明と固定アカウントを使い、ワーカーが受け取ったモデルと実行イベントの一致を検証。実LLM呼び出しは行っていない。

GUI再現: `cd gui && ./node_modules/.bin/react-router build` の後、リポジトリ直下で `node scripts/check-model-routing.mjs`。一時ポートのモックAPI・GUIのみを使用し、運用サービスには接続しない。既定の測定JSONは `/tmp/celeris-model-routing-measurements.json`。PNGは `docs/gui/model-routing/`。

[スマホのプロバイダー画面](gui/model-routing/providers-393.png)／[PCのプロバイダー画面](gui/model-routing/providers-1440.png)／[スマホのアカウント画面](gui/model-routing/accounts-393.png)。360／393／412／1440pxで確認。プロバイダー入力欄は44px以上、画面の横はみ出しなし。保存したIDがAPIに届くこととJSエラーなしを確認。

このベースには記憶にある `docs/gui/mobile-gui-investigation-2026-09-20.md` は無かった。現在のナビ・Console実装を維持し、設定画面だけ変更した。Nothing 2a実機・IME・実アカウントでの6モデル利用可否は未検証。運用への取り込み後、実行IDの確認・設定と実機確認が必要。
