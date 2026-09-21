# ADR-0052: 知識整理 run は Qwen が使えなければ他のプロバイダの cheap モデルに倒す

- 日付: 2026-09-21
- 状態: **Accepted**（人の指示「langmem のタスクは Qwen が使えなかったら他のプロバイダーの cheap モデルにフォールバックするようにして」。
  実機: 2026-09-20 夜〜21 朝、pegasus のトンネルが落ちている間に知識整理 run が 4 件連続で `langmem runner exited with a non-zero status`
  で失敗し、その 4 タスクの知識は抽出されないまま終わった）
- 関連: ADR-0047 D4（知識整理 run。`langmem` アダプタ）、ADR-0049（汎用ハーネスの供給元を固定しない。tier で候補を決定的に選ぶ）、
  ADR-0046 D3（harness は実行契約）、ADR-0021（失敗の扱い）

## 1. 文脈

知識整理 run は専用契約（`knowledge` ハーネス = `langmem` アダプタ = LangMem + OpenAI 互換の Qwen）。Qwen はローカルではなく
pegasus 経由のトンネル越しで、トンネルは人が GUI で TOTP を通さないと復帰しない。落ちている間は run が失敗し、しかも
「1 タスクにつき 1 回」の規則で**再実行されない**ので、その間に終わった仕事の知識は永久に取り込まれない。抽出は LLM さえあれば
どのハーネスでもできる仕事なので、Qwen が使えないときは汎用ハーネス（Claude Code / Codex。ADR-0049 の tier `cheap` の候補）に
同じ入力と同じ出力契約で倒す。

## 2. 決定

### D1. 実行前の到達性の検査（決定的）

- 知識整理タスクを dispatch する直前、ディスパッチャは `[knowledge.langmem].base_url` に **`GET <base_url>/models` を 3 秒**で
  当てる（LLM は呼ばない。ADR-0043 D3 のコンテナ runtime の probe と同じ種類の検査）。200 なら従来どおり `langmem` アダプタ。
  それ以外（接続不可・タイムアウト・5xx）なら **D2 のフォールバック**へ。結果は `Event::WorkerProgress{kind: status}`
  「langmem の接続先に届かない（<理由>）。cheap のハーネスに倒す」として残す。
- 検査の結果は 60 秒キャッシュする（tick ごとに叩かない）。

### D2. フォールバック: 汎用ハーネスで同じ出力契約を満たす

- `knowledge` ハーネスに **`fallback = { tier = "cheap" }`**（組み込みの既定。`[[harnesses]]` で上書き可。`fallback = false` で無効）。
  フォールバック時は ADR-0049 の規則で **tier `cheap` を持つ汎用の供給元**（`claude-code` / `codex` / `acp` のプール。枯渇・未ログインは
  飛ばす）から決定的に 1 つ選び、その adapter で run を起こす。候補が無ければ従来どおり失敗（`retryable = true`）。
- フォールバック run の前置きは、`langmem_run.py` が LangMem に渡している指示（ADR-0047 D4 の「保存するもの・しないもの・出典」）を
  そのまま人間可読の指示文にしたもの＋ `maintenance_objective`（同じ入力）＋**出力契約**: 「`artifacts/knowledge-candidates.json` に
  `{"candidates": [{op, path, title, tags, scope, body, sources, confidence}]}` を書け。候補が無ければ空配列。それ以外のファイルは
  作らない。道具は使わない（読む必要のあるものは全部この前置きにある）」。`max_turns = 8`、`max_wall_secs = 600`。
- 適用（`apply_candidates`）は経路に関係なく同じ。`knowledge_runs.summary_json` に **`via: "langmem" | "fallback:<adapter>"`** を残し、
  Console の `knowledge` ブロックとタイムラインに「（cheap のハーネスで抽出）」と出す。

### D3. 失敗した知識整理 run は一度だけやり直す

- `knowledge_runs.state = failed` で **`retried_at` が無い**ものは、次の tick でもう 1 回だけ作り直す（新しい run タスク。`retried_at` を書く）。
  1 回目が `langmem` で落ちた場合、2 回目は D1 の検査に従う（Qwen が戻っていれば `langmem`、まだなら fallback）。2 回目も落ちたら
  そのまま `failed`（人が `celerisctl knowledge rerun <task_id>` で手動再実行できる。管理系。`knowledge_runs` の行を消して作り直す）。
- 実機で失敗した 4 件（2026-09-20/21）は配備後の最初の tick でこの規則により 1 回やり直される。

### D4. 採らない

- LangMem の中で LangChain の別モデル（Anthropic API 等）に切り替える。手元には API キーが無く、Claude / Codex は CLI のアカウント
  （利用枠）でしか使えない。汎用ハーネスに倒す方が既存の供給元の規則（ADR-0049）に乗る。
- トンネルを Celeris が自動で張る（TOTP が要る。人の操作）。

## 3. 受け入れ条件（Phase 64）

1. 到達性の検査（fake の HTTP サーバで 200 / 接続不可 / タイムアウトの 3 通り。60 秒キャッシュ）。
2. フォールバックの選択（ADR-0049 の候補から cheap を決定的に選ぶ。候補なしは失敗のまま）。フォールバック run の前置きと出力契約。
   fake アダプタで `knowledge-candidates.json` を書かせて `apply_candidates` に流れるテスト。`summary_json.via`。
3. 一度だけのやり直し（`retried_at`。migration `0021_*`）。`celerisctl knowledge rerun`。
4. Console / タイムラインの表示（「cheap のハーネスで抽出」）。`docs/knowledge.md` に運用（Qwen が落ちたときの挙動、手動再実行）。
5. `cargo test --workspace --no-fail-fast` / clippy / GUI 一式。実機: トンネルが落ちた状態で 1 タスクを終端にし、fallback で候補が
   KB か `_inbox` に入ること。失敗していた 4 件が配備後にやり直されること。

## Phase 64 追記（2026-09-21。実装したときの逸脱と、決めたこと）

1. **到達性の検査は自前の最小 HTTP/1.1**（`crates/task-worker/src/probe.rs`。`std::net::TcpStream`）。
   `task-dispatch` も `task-worker` も HTTP クライアントを持っておらず、`tick()` は tokio のワーカー
   スレッドから同期で呼ばれるので `reqwest::blocking` は使えない（ランタイムの中で panic する）。
   依存を増やさずに「接続 → `GET <path>/models` → ステータス行だけ読む」を書いた。
   結果は 3 値: `Ok` / `Unreachable{reason}` / **`Unknown{reason}`**。`https://`・`base_url` 無し・
   書き方が壊れているものは `Unknown` で、**従来どおり `langmem`** に出す（検査できないことを
   「落ちている」と決めつけない）。実機の Qwen は `http://127.0.0.1:18000/v1` なので影響しない。
2. **フォールバックの選択は ADR-0049 の経路をそのまま使う**。専用の選択コードは書かず、dispatch の
   直前に `task.worker_hint.adapter` を **`None` にして tier を `cheap` にするだけ**（`StaticPolicy` は
   adapter 指定が無い仕事に `paperqa` / `local-deep-research` / `langmem` を渡さない規則を既に持つ）。
   アカウントプールの残量比較・枯渇・未ログインの扱いも既存のまま。DB のタスクは書き換えず、
   **ワーカーに渡す写しだけ**を変える（`RunExtras::knowledge_fallback`）。
3. **前置きは「抽出の指示 ＋ 出力契約」**を役割の指示文（`RunContext.role.instructions`）に載せる。
   `maintenance_objective` は**タスクの `objective` としてそのまま**渡る（同じ入力）ので、前置きでは
   繰り返さない。抽出の指示は `langmem_run.py` の `EXTRACTION_INSTRUCTIONS` を Rust 側が
   `include_str!` から切り出して使う（出典は python のランナー 1 つだけ。写し間違いが起きない。
   `task_worker::langmem::extraction_instructions`）。
4. **`via` は適用のときに run の `WorkerStarted.adapter` から決める**（`knowledge_maint::via_of`）。
   ディスパッチャから `knowledge_runs` を書く経路を増やさずに済み、やり直した run でも同じ規則で決まる。
   値は `knowledge_runs.via`（列。API はこちらを正とする）と `summary_json.via` の両方に残す。
5. **`celerisctl knowledge rerun` は行を消さずに戻す**（`state = failed` / `retried_at = NULL` /
   `applied_at`・`summary_json`・`via` を消す）。ADR の本文は「行を消して作り直す」だったが、
   `schedule` は backfill 禁止（`not_before` = デーモンの起動時刻）なので、**行を消すと古い仕事は
   二度と拾われない**（実機で落ちた 4 件はまさにそれ）。行を残して D3 のやり直しの経路
   （`retry_failed`。`not_before` を見ない）に拾わせる方が、同じ操作で確実に 1 回やり直る。
6. **管理 API ではなく celerisctl にした**。celerisctl は HTTP の口を持たず、`org migrate-v2` を含む
   全サブコマンドが DB を直接開く形になっている。`rerun` がやるのは 1 行の書き換えだけで、
   **新しい run を作るのはデーモンの次の tick** なので、デーモンを止める必要も再起動も無い。
   `knowledge` の他のサブコマンドは今までどおり DB を開かない（コンテナの中でも動く）ままにした。
   GUI からの操作は求められていないので API は増やしていない。
7. `HarnessRegistry::new` を 1 箇所だけ変えた: `[[harnesses]]` に同じ id を書いても `fallback` を
   省いていれば**組み込みの既定を継ぐ**。書いた瞬間に黙って無効になるのを避けるため
   （消したいときは `fallback = false` と明示する）。
8. やり直しは **1 tick に 1 件**（`schedule` と同じ間引き）。`knowledge_run_retry` は
   `WHERE retried_at IS NULL` の UPDATE なので、「一度だけ」はストアが守る。
