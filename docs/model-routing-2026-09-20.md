# 認証アカウントの分離と階層・残量によるモデル実行

---
tasks: [01M305NG9QRX7HE59VF6K3FZ7H]
---

## 実装と反映範囲

作業ブランチ `celeris/01M305NG9QRX7HE59VF6K3FZ7H` に実装。main `d89015802e102e89d558bb8307149bafa8997ae2` へrebase済み。既存実装は `3b75909`（バックエンド）、`e97f807`（GUI）、`36dfdd3`（スキーマ同期）。運用中の設定・認証ファイル・サービス、元リポジトリ、リモート作業ツリーは変更していない。取り込みとリリースへの反映は未実施。

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

## 再試行での修正・再検証（run 01M3083Y5XE4N3TFJH63ZE5ZF0）

前回レビューでは `schema::tests::committed_schema_matches_generated` が失敗し、受け入れ項目は未評価だった。原因は `ProviderLive` のRust説明を「認証アカウントは別参照」に変更した後、コミット済みAPIスキーマとGUI生成型の説明が旧「行 = アカウント」のまま残っていたこと。既存実装（最新rebase後 `3b75909`・`e97f807`） を保持し、両生成物の説明を再生成して同期した。

- `UPDATE_SCHEMA=1 cargo test -p task-api schema::tests::committed_schema_matches_generated` で生成後、**UPDATE_SCHEMAなし**の `cargo test --workspace` を実行。終了コード0、1,522件成功、0件失敗、3件ignore。移行、モデル解決、委譲と残量選択、CLI実引数、スキーマ整合性を含む。
- `cargo clippy --workspace -- -D warnings`: 終了コード0。
- インストール済み `json2ts` を `gui/scripts/gen-types.mjs` と同じ引数・ヘッダーで直接実行し、GUI型を再生成。pnpm経由は環境のストアDBを開けなかったため、追加インストールせず既存ツールを使用。
- GUI `react-router typegen`、`tsc -b`、`react-router build`: 終了コード0。関連4ファイルのVitest 42件成功。最初の制限環境ではモックAPIの待受けに失敗したため、ローカル待受けを許可して再実行した。
- `node scripts/check-model-routing.mjs /tmp/model-routing-retry-images /tmp/model-routing-retry-measurements.json`: 終了コード0。両設定画面を360／393／412／1440pxで再描画、全8画面で文書幅＝ビューポート幅。実行IDの保存とJSエラー0件を確認。プロバイダーの入力高44px、既存アカウント入力高は約36px。既存の画面画像は上記リンクに保持。

このrunの機械向けログと測定JSONはワークスペースの `artifacts/model-routing-retry/` に保存。実装の運用取り込み・サービス反映、実サービスの6モデルID確認、Nothing 2a実機／IME確認は未実施であり、再検証の完了には含めない。


## 再差し戻し P1/P2 の修正（2026-09-20）

`celerisctl worker run` の直接実行経路を修正した。固定 `account_id` を候補の制約として扱い、欠損・未ログイン・利用不可なら停止する。別アカウントへの代替は行わず、固定参照と異なる `--account` はエラー。同一IDの明示指定も固定参照の利用可否検査を通す。

選択したアカウントの保存済み利用枠を、デーモンと共通の `measured_remaining` / `select_tier` で評価し、実行用タスクの階層と `--model` に反映する。古い観測・残量不明は要求維持、利用枠枯渇は起動前に停止（直接CLIは再試行待ちを登録せず非ゼロ終了）。階層マッピングのない旧設定は既存モデルを維持。標準出力に実行階層・モデル・選択理由を表示する。ローカル／クラスタの実行分岐より前に共通で適用する。

`crates/celerisctl/tests/worker_run.rs` の実バイナリ統合テストは11条件を検証する。固定IDが別の健全なアカウントより優先されること、欠損固定ID・競合フラグ、残量90%/20%/5%・不明・古い観測・枯渇、無効モデル、明示アカウントでも予算判定を迂回しないことを含む。スタブが受け取る `CODEX_HOME` と `--model`、表示内容、起動前停止、タスク・イベント・利用量ファイルの不変を確認する。外部LLMへの接続はしない。

GUIは今回変更しておらず、前節のスマホ幅／PC検証を継承する。Nothing 2a実機・IMEと6モデルの実サービス利用可否は引き続き未検証。

main `4444719`（レビュー条件の修正）を取り込み、検証中に進んだ最新main `d89015802e102e89d558bb8307149bafa8997ae2`（リリーススクリプト修正）も競合なくrebaseで取り込んだ。登録元mainにある `run-phases.sh` / `run-gphases.sh` の未コミット削除はユーザーの既存変更であり、一切操作していない。cleanを確認する対象は実装worktree。登録元mainへのマージ・本番デプロイは実施していない。


今回の検証結果: `cargo test -p celerisctl --test worker_run` は7テスト成功（追加テスト内11条件）。`cargo test --workspace` は1531成功・0失敗・3ignore、`cargo clippy --workspace -- -D warnings` は成功。最初のsandbox内workspace実行は既存socket bindテストのEPERMで停止し、localhost接続を許可した再実行で全件成功した。ログはrun成果物 `artifacts/model-routing-final/` の `worker-run.log`、`workspace-test-unrestricted.log`、`clippy.log`。追加main差分は `scripts/selfdeploy/promote.sh` のみで、検証済みRustコードは同一。

最新main取り込み後も `cargo test -p celerisctl --test worker_run`（7件）、`cargo test --workspace`（1531成功・0失敗・3ignore）、`cargo clippy --workspace -- -D warnings` を再実行し、すべて終了コード0。追加ログは同成果物ディレクトリの `workspace-test-after-rebase.log` / `clippy-after-rebase.log`。`git merge-base main HEAD` は `d89015802e102e89d558bb8307149bafa8997ae2` と一致し、fast-forward可能。
