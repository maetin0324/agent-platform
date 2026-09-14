# ADR-0012: 複数アカウント運用（プロバイダ別アダプタ・フォールバック・候補なしの区別）、evidence の任意化、`taskctl worker run`

- 日付: 2026-09-14
- 状態: Accepted（人間の判断: 「複数アカウント運用は割とすぐに始めたい。それ以外はいい感じに」）
- 関連: `docs/DESIGN.md` §5.3, §5.4, §5.5, §5.9 / [ADR-0005](0005-phase3-dispatch-and-worker.md) D6 /
  [ADR-0006](0006-phase4-claude-code-adapter.md) D6 / [ADR-0010](0010-phase7-hardening.md) / 提案 P-12, P-20, P-33

## 文脈

DESIGN §5.4 は「設定ディレクトリ（`CLAUDE_CONFIG_DIR` 等）をプロバイダごとに切替えてアカウントを分離」としているが、実装では
アダプタのインスタンスが**アダプタ種別ごとに 1 つ**（`[adapters.claude_code]` の `env` / `model` を全プロバイダが共有）で、
同じ種別のプロバイダを複数並べても同じアカウントで動く。`[[providers]].model` も `WorkerStarted` に記録されるラベルでしかない。
さらに `ProviderPolicy::pick` は候補を 1 つしか返さないため、先頭のプロバイダが並列度の上限に達すると空いている 2 つ目の
アカウントがあってもタスクは待つ（P-20）。`pick` の `None` は「設定に候補が無い」と「一時的に使えない」を区別しない（P-33）。

## 決定

### D1. アダプタのインスタンスはプロバイダごと（アカウント分離）

- `[[providers]]` に `env`（テーブル、任意）を追加する。`taskd` は `[[providers]]` の各行について、`[adapters.<種別>]` を基本設定とし、
  プロバイダの `env` を重ねた（同名キーはプロバイダが優先）アダプタを 1 つずつ作る。ディスパッチャはアダプタを
  **プロバイダ ID で引く**。
- `[[providers]].model` が空でなければ、そのプロバイダの run の `--model` に使う（空なら `[adapters.<種別>].model`）。
  `WorkerStarted.model` には実際に使うモデル名を記録する。
- `Event::WorkerStarted` に任意フィールド `provider`（プロバイダ ID）を追加し、どのアカウントで実行したかをイベントに残す
  （同じモデルを複数アカウントで使うと `model` だけでは区別できないため。導入前のイベントは `provider` 無しで読める）。
- アカウント分離の方法は環境変数で行う: claude-code は `CLAUDE_CONFIG_DIR`（アカウントごとに `CLAUDE_CONFIG_DIR=<dir> claude` で
  ログインしておく）、codex は `CODEX_HOME`。API キーを TOML に直接書くことはできるが推奨しない（設定ファイルが秘密になるため）。
- ワーカーのサブプロセスは **taskd 自身の環境を引き継ぐ**（`env_clear` しない）。実効の優先順位は「taskd の環境 < `[adapters.<種別>].env` <
  `[[providers]].env`」。taskd の環境に `ANTHROPIC_API_KEY` 等があると `CLAUDE_CONFIG_DIR` のログインより優先され、全アカウントが
  同じ認証になりうるので、複数アカウント運用では taskd の環境から外しておく（Phase 8 監査で明記）。
- `[[providers]].model` はこれまで `WorkerStarted` のラベルでしかなかったが、今後は `--model` に渡る（既存設定の挙動変更。example に注記）。
- プロバイダ ID の重複は設定エラーにする。
- 複数アカウントの自動切替や残量推定は引き続き供給層の担当（DESIGN §6 非目標）。ここで行うのは「設定表の行 = 1 アカウント」を
  正しく扱うことと、下の D2 の決定的なフォールバックだけ。

### D2. `ProviderPolicy::select`（除外集合付きの選択と、候補なしの区別）

DESIGN §5.5 の既存 3 メソッドは変えず、既定実装付きのメソッドを 1 つ追加する（供給層が実装する既存のポリシーを壊さない）:

```rust
pub enum Selection { Picked { adapter, provider }, Busy, NoMatchingProvider }
fn select(&self, hint: &WorkerHint, now: Instant, excluded: &HashSet<ProviderId>) -> Selection {
    // 既定: pick の結果が除外されていなければ Picked、それ以外は Busy（区別できないので従来どおり待つ）
}
```

- `StaticPolicy::select` は設定表の順に、adapter 指定と tier が合う行を見て、cooldown 中でも除外集合にも入っていない最初の行を返す。
  合う行はあるが全て使えなければ `Busy`、合う行が 1 つも無ければ `NoMatchingProvider`。
- ディスパッチャは、選ばれたプロバイダが並列度の上限に達していれば除外集合に入れて `select` をやり直す（次の行へフォールバック、P-20）。
  除外集合は同じ tick の中で共有する（tick 内で空きは増えない）。外部ポリシーが除外を無視しても止まるよう試行は 64 回まで。
  結果として設定表の順の「優先 + あふれ」になる（先頭のアカウントが上限まで使われ、あふれた分と cooldown 中の分が次へ行く）。
- `NoMatchingProvider` のタスクはタスクごとに 1 回 warn し、その tick の「経路なし」集合に入れる。`is_idle` は ready タスクが
  全て経路なしなら idle とみなす（設定を直さない限り進まない。P-33）。Reviewer run の選択も同じ手順を使う。
- `ready_tasks` の取得窓（`max_concurrency * 4 + 16`）は、経路なしと分かっているタスクの数だけ広げる。窓いっぱいに返ってきたときは
  `is_idle` を偽にする。経路なしタスクが窓を埋めて、後ろの実行可能なタスクが dispatch されない・`--until-idle` が仕事を残して終わる、
  を防ぐ（Phase 8 監査の指摘で修正）。

### D3. `evidence[]` の各フィールドを任意に（P-12）

`Evidence{criterion, command?, exit?, stdout_tail?}`。`command` / `exit` / `stdout_tail` は `ArtifactExists` / `Reviewer` / `Human` の
条件には存在しないため任意にする。必須 → 任意の緩和なので後方互換（`run.protocol` は 1 のまま）。Reviewer はもともと自己申告の
証拠で判定しないので判定には影響しない。

### D4. `taskctl worker run`（DESIGN §5.9 のデバッグ用コマンド）

```
taskctl [--db <db>] worker run --config <taskd.toml> --task <id> [--provider <id> | --adapter <種別>] [--workspace <dir>]
```

- 目的: デーモンとディスパッチャを通さずに、1 タスクを 1 つのプロバイダ（アカウント）のアダプタで 1 回だけ実行し、認証・モデル指定・
  プロンプト・結果ファイルの扱いを確かめる。
- **状態を変えない**: リースを取らず、遷移もイベントの追記もしない（DB は読むだけ）。レビューも行わない。
  `context.prior_review` と `context.answers` はディスパッチャと同じ関数で events から組み立てる。
- アダプタは D1 と同じ `taskd::build_adapters` で作る。`--provider` 省略時は `--adapter` に合う最初のプロバイダ、両方省略時は
  `select`（cooldown 無し）でタスクの `worker_hint` に合う最初のプロバイダ。
- 作業ディレクトリはタスクの workspace（相対なら `workspace_root` 基準）。`--workspace` で別ディレクトリ（コピー等）を指定できる。
  タスクが `running` / `reviewing` のときは、デーモンの run と作業ディレクトリを取り合うので `--workspace` 指定なしでは拒否する。
- 出力: `progress: ...` / `artifact: <name> <path> sha256=<...>` を逐次、最後に正規化した終端メッセージを `result: <json>` で出す。
  exit code は done=0、question=3、error（ワーカーの error・アダプタのエラー）=4、タスクやプロバイダの不在などの誤り=1、
  引数の構文誤り（clap）=2、SIGINT / SIGTERM による中断=130。
- 中断時はアダプタの run を drop してワーカーの子プロセスを kill してから終わる（シグナルの既定動作で死ぬと子が残るため。
  Phase 8 監査の指摘で修正）。kill されるのはアダプタが起動した直接の子で、そのさらに子（ワーカーが起動したツールのプロセス等）は
  対象外（デーモンの run の中断と同じ制約）。
- `ready` / `blocked` / `done` のタスクは `--workspace` 無しでも本来の作業ディレクトリで実行できる。`ready` はデーモンの run と、`done` は
  記録済みの成果物（sha256）と食い違いうるので、確認用途では `--workspace` にコピーを指定することを推奨する。

## 結果

- `task-dispatch`: `policy.rs`（`Selection`, `select`）、`dispatcher.rs`（プロバイダ ID で引くアダプタ、フォールバック、経路なし）。
- `taskd`: `[[providers]].env`、`build_adapters` / `effective_models`、ID 重複の検証、`config/taskd.multi-account.example.toml`。
- `task-worker`: `Evidence` の任意化、`worker-protocol.schema.json` 再生成、`docs/protocol/worker-protocol.md` の evidence の記述。
- `taskctl`: `worker run`。
- `docs/DESIGN.md` への反映（§3、§4.3 `WorkerStarted.provider`、§5.2 フォールバック、§5.3 evidence、§5.4 プロバイダ別 env/model、
  §5.5 `select` / `Selection`、§5.9 `worker run` の仕様、§6 Phase 8 の節と非目標の整理）は、人間の許可を得て 2026-09-14 に行った（P-41）。
