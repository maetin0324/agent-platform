# ADR-0054: CoS と部門長はセッションを継続し、Console はハーネスと同じ「考え → tool call → 投げたら一旦止まる」を流す

- 日付: 2026-09-21
- 状態: **Accepted**（人の指示: 今の CoS チャットは「投げると対話応答タスクが発生し、完了次第返答する」で見づらい。Claude Code / Codex と同様に
  「チャットを投げる → 思考内容がある程度見える → どの tool call をしたかが出る → サブエージェントに一通り投げたら一旦止まる」にする。
  CoS が dispatch したタスクが見えるのは良い。CoS との対話はステートレスでなくステートフルに継続しつつ案件の発生やタスク割り当てができる
  こと。「コンテキストが逼迫しない限りセッションを継続。CoS に限らず、レビューや詳細な仕事切り分けを担当する部門長ワーカーも
  セッションをある程度継続」。サブエージェントの流れは ADR-0048 の理解どおり）
- 関連: ADR-0048（Console。D2 の progress の正規化、D3 の actions）、ADR-0033 D4（対話の規則）、ADR-0046 D6（CoS = 根）、ADR-0051（部署のレビュー）

## 1. 決定

### D1. ノードごとの**継続セッション**（`node_sessions`）

- migration: `node_sessions(node_id, kind ('conversation'|'lead'), project_id NULL, adapter, account_id, session_id, turns, approx_tokens,
  created_at, last_used_at, retired_at NULL)`。**CoS の対話は全体で 1 本**（`project_id NULL`。案件を開いているときは前置きに案件の文脈を足す
  だけで、セッションは同じ）。部門長（部署の根ノード: engineering / research / operations）の **レビュー・切り分け run** は部署ごとに 1 本。
- 継続の手段はアダプタごと: `claude-code` は `--resume <session_id>`（初回は `--session-id <ulid>` で固定）、`codex` は `codex exec resume <id>`
  （無ければ `-c experimental_resume`）、`acp` は ACP の `session/load`。**同じアカウント**で続ける（アカウントが枯渇して別に倒れたら
  新しいセッションを作り、前置きに「これまでの要約」（ADR-0033 D4 の対話履歴の末尾 20 件）を入れて継ぎ、`retired_at` を付ける）。
- **逼迫の判定は決定的**: `approx_tokens`（run の usage の累計）が `[sessions] rollover_tokens`（既定 400k）を超えたら次の run から新セッション
  （要約を前置きに）。失敗（`resume` が拒否された・セッションが無い）も同じ経路で作り直す。人が Console の「新しい会話」を押しても同じ。
- 前置きは継続中は**差分だけ**にする（毎回 brief・記憶・組織の一覧を流し直さない。新規セッションの初回だけ全量）。差分 = 前回の run 以降に
  起きたこと（新しい人の発言、dispatch したタスクの終端と要約、認可の結果、新しい案件）。

### D2. Console は run の**進行を生で流す**（考え → tool call → 一旦止まる）

- 人の発言を送ると `human` ブロックが即時に出、続けて **その対話 run の `progress` を run 中に流す**（ADR-0048 D2 の `kind`:
  `thinking` は要約 1 行、`text` は本文をそのまま追記（部分文字列で更新）、`tool_use` は `tool` + `summary` を 1 行、`tool_result` は
  折り畳み）。返事の本文（`reply`）は run の `text` の積み上げそのもの（完了時に `messages` に確定）。
- CoS が **actions を出したら run はそこで終わる**（「サブエージェントに一通り投げたら一旦止まる」）。作ったタスクは `task` ブロックとして
  返事の直下に出、その後の進行はそのタスクの `progress` として流れる（既定は折り畳み）。終端は `task` ブロック + `report`。
- **CoS の対話 run は道具を使わない**（ADR-0033 D4 のまま）が、**読み取りの道具だけ**は許す（`celerisctl knowledge search|get`、タスク・案件の
  一覧と詳細の read API）。前置きの「一覧」を薄くして、必要なときに引かせるため（D1 の差分前置きと対になる）。書く操作は actions だけ。
- 入力欄は run 中も打てる（キューに入り、run が終わってから次の run になる。人の割り込みで run を止めるのは従来の「返信」）。

### D3. GUI

- `/`（Console）の CoS スレッドは**チャット欄**として描く: 左に流れ（ADR-0048 D4）、CoS の発言は run の進行を「考え中…」→ tool call の行 →
  本文 → 作ったタスクのカード、の順に**同じ吹き出しの中で**育つ。完了で吹き出しが確定。
- 「新しい会話」（セッションを捨てる）、「この案件の文脈で話す」（scope）。部門長のセッションは組織画面のノードに「継続中のセッション:
  turns / tokens / 最終使用」を出す（会話 UI は作らない）。
- スマホ幅（ADR-0055）で吹き出しがはみ出さないこと。

## 2. 採らない

- LLM に「そろそろ新しいセッションに」と判断させる（token 数で決める）。
- CoS が書く操作の道具（ファイル・git・API の POST）を持つ（actions だけ）。

## 3. 受け入れ条件

- **Phase 67（D1）**: `node_sessions`、3 アダプタの resume、初回全量／継続は差分の前置き、rollover と要約の継ぎ、アカウントが変わったときの作り直し、
  部門長のセッション（ADR-0051 のレビュー run）。テストは fake アダプタで session id の受け渡しと rollover。実機: CoS に 3 往復して 2 回目以降が
  `--resume` で走り、前置きが差分だけになっていること（`runs/<id>/request.json`）。
- **Phase 68（D2・D3）**: run 中の progress を Console へ（`text` の追記、`tool_use` の行、actions で止まる）、読み取り道具の許可、入力のキュー、
  GUI のチャット吹き出し、「新しい会話」。実機: スマホ幅で 1 往復して考え → tool call → タスクのカードが順に出る。

## Phase 67 追記（実装時の逸脱・明確化。2026-09-21）

- **`node_sessions`**（migration 0023、schema_version 23）: `id, node_id, kind('conversation'|'lead'),
  project_id, adapter, account_id, session_id, turns, approx_tokens, created_at, last_used_at, retired_at`。
  `crates/task-core/src/node_session.rs`。「有効なセッション」は `retired_at IS NULL` の行が高々 1 件、という
  不変条件はストア自身では強制せず（部分 UNIQUE インデックスは使わない）、`task-dispatch` 側が
  `node_session_retire` → `node_session_create` の順で呼ぶことで保つ（DESIGN 原則 1: ストアに判断を入れない）。
- **判断の純粋関数**は `crates/task-dispatch/src/sessions.rs`（`decide` / `diff_lines` / `summary_lines`）に
  切り出した。`decide(active, adapter_id, account, rollover_tokens, resume_failed) -> Resume | Fresh(reason)`
  で、`resume_failed` → `account_id` 不一致 → `adapter_id` 不一致 → `approx_tokens >= rollover_tokens` の順に見る。
  I/O は `Dispatcher::resolve_node_session`（`dispatcher.rs`）が行う。
- **アダプタごとの継続**（`RunContext.session: Option<SessionHandle{adapter, session_id, resume}>`）:
  - `claude-code`: `resume = false` なら celeris が前もって決めた ULID を `--session-id`、`resume = true`
    なら `--resume <id>`（`claude_code.rs`）。
  - `codex`: `[adapters.codex] resume_mode`（既定 `"exec_resume"`）で `codex exec resume <id>` か
    `-c experimental_resume=<path>` かを選ぶ。**受け入れ条件の「バージョンプローブ」ではなく設定フラグ**を
    選んだ（起動のたびに `codex --version` を呼ぶ副作用・キャッシュ管理を避けるため。値は
    `celeris/src/config.rs::resolved_resume_mode` が決定的に解決し、未知の値は既定にフォールバック）。
    `codex` は run の途中で `thread.started` イベントから実際のセッション id を確定させるので、
    `EventSink::session_established` で `node_sessions.session_id` を上書きする（`codex.rs`）。
  - `acp`: `resume = false` なら `session/new`、`resume = true` なら `session/load`。拒否されたら
    `EventSink::session_resume_failed` を呼ぶ（`acp.rs`）。
  - **resume 拒否の検出**は `crates/task-worker/src/provider.rs::looks_like_resume_rejection`
    （エラーメッセージの既知の言い回しを見る決定的な文字列判定。LLM は使わない）。検出したら
    `EventSink::session_resume_failed` を通じて**その場で** `node_sessions` を retire する（次の
    `resolve_node_session` を待たない）。`Dispatcher::resolve_node_session` に渡す `resume_failed` 引数は
    現状常に `false`（このイベントソースの経路で先に retire 済みのため、次回は自然に `NoActive` になる。
    下の「既知の逸脱」参照）。
- **rollover**: `[sessions] rollover_tokens`（既定 400,000。`crates/celeris/src/config.rs::SessionsConfig`）。
  `Dispatcher::on_worker_finished`（CoS の対話 run）と `Dispatcher::on_review_finished`（部門長のレビュー
  run）が、run の usage（`input_tokens + output_tokens`）が分かった時点で `node_session_touch` を呼び、
  `approx_tokens` に積む。次の `resolve_node_session` 呼び出しが `approx_tokens >= rollover_tokens` を見て
  作り直す。
- **アカウント変更**: `decide` が `active.account_id != account`（今回選ばれたアカウント）を見て
  `Fresh(AccountChanged)` にする。プールが枯渇して別アカウントに倒れたときも同じ経路。
- **前置きの差分化**（D1「前置きは継続中は差分だけ」）: `Dispatcher::run_extras` で、CoS の対話 run が
  **継続中**（`session.resume == true`）のときだけ `node`（brief）・`memory`（記憶）・`organization`
  （組織の一覧）・`conversation`（直近のやり取り）・`active_projects`（進行中の案件）を**空にする**
  （前置きに出さない）。新規セッション（`resume == false`。初回・rollover・アカウント変更・resume 失敗の
  後のすべて）ではこれまでどおり全量を渡す。差分そのもの（`session_diff`）は
  `Dispatcher::session_diff_since`（新しい人の発言・dispatch したタスクの終端と要約・認可の結果・新しい
  案件）が組み、`crate::sessions::diff_lines`（純粋関数、`since` より後だけを残す）でフィルタする。
  部門長（`kind = lead`）のレビュー run はもともと brief・記憶・組織図を渡さない設計（ADR-0033 D4: 判定は
  成果物と条件だけで決める）なので、この差分化の対象外（`session`/`session_diff` は渡すが、他は元から空）。
- **`POST /console/new-conversation`**（`crates/task-api/src/console.rs`）: CoS の継続セッションを
  `node_session_retire` で捨てるだけの薄い管理系エンドポイント。ディスパッチャに触らない。204、本文なし。
  `docs/gui/api.md` §3.109。
- **部門長のセッション（ADR-0051 のレビュー run）**: `Dispatcher::pick_reviewer` が、対象タスクの部署
  （`task_core::department_of`）が分かれば `resolve_node_session(<department>, SessionKind::Lead, None, …)`
  を呼び、`ReviewerRun.session`/`session_diff` として `review::review_task` の `RunContext` に渡す
  （`review.rs`）。部署が無い仕事（従来の独立レビュアー）はこれまでどおりセッションを持たない。

### 既知の逸脱・未解決事項

1. **「前のセッションの要約」と「直近のやり取り」の重複**: 新規セッションになった理由が `NoActive`
   以外（rollover・アカウント変更・resume 失敗）のとき、`session_diff` に「前のセッションの要約」
   （対話履歴の末尾 20 件）が乗るが、この場合 `continuing == false` なので `context.conversation`
   （同じく末尾 20 件、別の見出し）も**そのまま渡る**。同じ内容が 2 つの節に出る（誤りではないが
   冗長。数百トークン程度）。直すなら「summary が乗る run では `conversation` を空にする」という
   条件を `run_extras` に足す（今回は時間の都合で見送った。次の Phase での改善候補）。
2. **`resolve_node_session` の `resume_failed` 引数は常に `false`**: 実際の resume 拒否の検出と retire は
   `EventSink::session_resume_failed`（run の途中、アダプタが拒否を検出した時点）で先に行われるため、
   次に `resolve_node_session` を呼ぶ時点では既に `active = None`（`FreshReason::NoActive`）になっている。
   結果として resume 失敗後の最初の run は「要約なし」の全量前置きになる（`conversation` フィールドに
   直近のやり取りはそのまま乗るので、実害は小さい）。`resume_failed` 引数自体は、将来
   `resolve_node_session` を呼ぶ側が retire 前の状態を渡せるようにする拡張の余地として残した。
3. **`codex` の `resume_mode` は設定フラグ**（バージョンプローブではない）。既定 `exec_resume`。
   実機で使っている codex CLI が `codex exec resume` に対応しないバージョンなら
   `resume_mode = "experimental_resume"` に手動で切り替える必要がある（`config/celeris.codex.example.toml`
   に注記）。
4. **実機確認は未実施**（ADR-0009 P-34。本物の claude-code/codex/acp CLI も外向きネットワークも無い
   サンドボックスのため）。「CoS に 3 往復して 2 回目以降が `--resume` で走り、前置きが差分だけになって
   いること（`runs/<id>/request.json`）」は、`crates/task-worker/src/claude_code.rs` の
   `request_json_records_the_session_handle` テストと、`crates/task-dispatch/src/dispatcher.rs` の
   `a_continuing_cos_session_drops_the_full_preamble_and_a_fresh_one_keeps_it` テスト（偽アダプタ経由、
   `run_extras` の出力を直接検査）で代替した。本番での確認手順は `docs/PROGRESS.md` の Phase 67 節に書いた。
5. **GUI の「継続中のセッション」表示（D3 の一部）は今回に含めない**（Phase 67 は D1 のみが対象。D3 の
   node-page indicator は Phase 68 の GUI 作業とまとめて行う）。
