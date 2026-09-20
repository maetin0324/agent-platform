# ADR-0049: 汎用ハーネスの供給元を固定せず、Codex の残量を直接観測する

- 日付: 2026-09-20
- 状態: Accepted（人の依頼: Claude の残量が無いと CoS が使えず、Codex の残量も見えない問題を直す）

## 決定

1. ハーネスは仕事の契約、プロバイダは実行能力の供給元、アカウントは認証と利用枠の単位。
   汎用ハーネスは `adapter` を省略し、Claude Code / Codex / ACP（と開発用 fake）から選ぶ。
   PaperQA / Local Deep Research / LangMem は専用契約なので、adapter 指定がある仕事だけに割り当てる。
   明示的な adapter 指定は固定として維持する。既存の汎用ハーネスの固定は移行ツールで外し、
   専用ハーネスやタスク自身の指定は変更しない。
2. tier・adapter・並列度・cooldown を満たす候補を決定的に選ぶ。アカウントプール同士は
   アダプタをまたいで残量スコアを比較し、同点は設定順。枯渇・未ログインのプールは飛ばす。
   非プールの供給元は既存の設定順を保ち、プール候補があればその後のフォールバックとする。
   残量不明は不明のまま扱い、値を捏造しない。
3. Codex の確認は `codex app-server` の stdio JSON-RPC を使う。
   `initialize` → `initialized` → `account/read` → `account/rateLimits/read`。
   推論ターンを起こさず、アカウント単位の `CODEX_HOME` を使う。タイムアウト・失敗時も子を終了する。
   API key 認証は ChatGPT の利用枠を持たないため、認証種別と残量取得対象外であることを返す。
4. `rateLimitsByLimitId.codex` を優先し、旧形式 `rateLimits` も読む。
   `resetsAt` / `resets_at` は Unix 絶対秒。残り秒のフィールドとは区別する。
   使用率・期間・リセット時刻が不正または欠落した窓は不明とし、期間からリセット時刻を推定しない。
5. active デーモンは Codex プールを定期確認する（5 分ごと、未ログイン・使用中・ログイン中は除外）。
   GUI の手動確認も同じ経路。Claude の定期的な推論による確認は追加しない。
6. 表示は Claude 固有の 5 時間・週次を前提にせず、短期枠・長期枠とする。

## 検証

外部サービスを呼ばない JSON-RPC スタブで、初期化順、残量、認証、異常応答、期限切れを検証する。
ルーティングは Claude 枯渇 / Codex 利用可能、両方利用可能、専用アダプタ混在、明示固定を検証する。
実機の結果と移行・適用の状況は PROGRESS に残す。

## 出典

[OpenAI Codex App Server](https://learn.chatgpt.com/docs/app-server) の account/rateLimits/read と
ローカルの codex-cli 0.154.0 の `app-server generate-json-schema` で契約を確認。
Codex の任意のモデルを任意の他社専用ハーネスへ注入する変更はこの修正には含めない。

## 実機移行で確認した実行契約

CoS の一時作業領域は Git リポジトリではない。Codex の全 worker 起動に
`--skip-git-repo-check` を渡し、管理された作業領域で実行可能にする。
結果ファイルを書けるよう `-c sandbox_mode="workspace-write"` を既定値として渡す。
明示的な extra_args の sandbox 指定で上書きでき、承認設定は変更しない。

Codex は対話に最終テキストだけを返す場合もある。conversation が設定された execute のみ、
正常 exit + turn.completed + 非空の最終 agent_message + result.json 不在なら返答を Done に正規化する。
通常の仕事、失敗、壊れた結果ファイルは救済しない。結果ファイルがあれば従来契約を優先する。

## worktree の成果物保存（2026-09-20）

Codex の cwd が worktree のとき、dispatcher の artifacts_dir は cwd の外にある。
起動前にそのディレクトリを作り、`--add-dir <artifacts_dir>` を渡す。
許可はこのタスクの成果物ディレクトリだけに限定し、共有親や他タスクには広げない。
sandbox を無効化せず、誤った場所の result.json を成功結果として回収しない。
ローカル CLI 0.154.0 `exec --help` の --add-dir と実機ログで確認。
