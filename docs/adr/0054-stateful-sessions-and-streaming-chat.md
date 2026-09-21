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

## Phase 67b 追記（本番障害の修正。2026-09-21）

- **観測**（本番、2026-09-21 13:53 UTC。release `6de40cb829e3`、Claude Code CLI 2.1.278）: CoS との
  最初の対話 run が `--session-id 01M323X6TJQSFEP0MKXABWVY78` で spawn され、`claude` が
  `Error: Invalid session ID. Must be a valid UUID.` で exit 1。以降の run は同じ壊れた行を resume しようと
  `--resume 01M323X6…` を渡し、`--resume requires a valid session ID or session title when used with
  --print … Provided value "01M323X6TJQSFEP0MKXABWVY78" is not a UUID and does not match any session
  title.` で失敗し続けた。`node_sessions` の該当行（`cos` / `conversation` / `claude-code` /
  `claude_max_lab`、`turns=1`）が retire されずに残り続けたため、**CoS の対話・部門長のレビュー run が
  claude-code アダプタで全滅**した。
- **根本原因**: Phase 67 は `Dispatcher::resolve_node_session`（`dispatcher.rs`）で
  `ulid::Ulid::new().to_string()` を celeris 側の `--session-id` として発行していた。ULID
  （`01ARZ3ND…` の 26 文字、Crockford Base32）は Claude Code CLI が要求する UUID
  （`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`）と形式が違う。Phase 67 のテストはすべて `claude` 本体ではなく
  シェルスタブ（`stub_claude`）を spawn していたため、**どんな文字列の id でも通ってしまい**、この不一致は
  テストでは一度も検出されなかった。
- **テストが検出できなかった理由（受け入れ条件 6 への回答）**: `crates/task-dispatch` のテストは
  `fake` アダプタ（`crates/task-worker/src/fake.rs`）や `FileAdapter`/`InstantAdapter` のような
  何もしないスタブを使う。`fake` アダプタはそもそも `crate::sessions::SUPPORTED_ADAPTERS`
  （`claude-code`/`codex`/`acp`）に入っていないのでセッションの対象外、`crates/task-worker` 側の
  `claude_code.rs` の単体テストは実プロセスの代わりにシェルスクリプト（`stub_claude`）を spawn し、
  渡された引数をログに記録するだけで**中身を検査しなかった**。つまり「`--session-id`/`--resume` に
  渡す値の形式が正しいか」を確認する層がどこにも無かった。Phase 67b はこの穴を 2 か所でふさぐ:
  `crates/task-worker/src/claude_code.rs` に spawn 前の境界検査を足し（受け入れ条件 3）、
  `crates/task-worker/src/provider.rs::is_valid_uuid` をテストする（形式の正しさを直接見るテストを
  初めて持つ）。
- **修正**:
  1. **id の発行を UUID に変更**（`crates/task-dispatch/src/sessions.rs::new_session_id(adapter_id)`。
     純粋関数）。`claude-code` は `ulid::Ulid::new()` の 128 bit をそのまま UUID v4 として組み立てる
     （`random_uuid_v4`。version/variant の 6 bit だけ RFC 4122 の規約どおり上書きする）。`uuid` crate は
     `Cargo.lock` に無いため新規依存は増やさない（受け入れ条件 1 の指示どおり）。`codex`/`acp` は
     Phase 67 のまま変更なし（celeris は id を先取りしない。空文字のまま `EventSink::session_established`
     を待つ）。`Dispatcher::resolve_node_session` は `ulid::Ulid::new().to_string()` の代わりにこの関数を
     呼ぶだけになった。
  2. **自己修復**（`crates/task-dispatch/src/sessions.rs::decide` に検査を追加。
     `session_id_is_valid_for_adapter(adapter_id, &active.session_id)`）: `resolve_node_session` が
     読み込んだ現役セッションが `claude-code` で `session_id` が UUID でなければ、`FreshReason::
     InvalidSessionId` として扱い（`resume_failed`・`adapter_id` 不一致の次、`account_id`/rollover の前）、
     既存の Fresh 経路（retire → 新規作成）にそのまま乗せた。retire 時に `tracing::warn!` でログを残す
     （`node_id` / `kind` / `adapter` / 旧 `session_id`）。データマイグレーションは選ばなかった:
     自己修復が `resolve_node_session` を呼ぶたびに効くため（CoS 対話・部門長レビューのどちらも次の
     run で必ず通る経路）、起動時に 1 回だけ走る migration より**確実**（本番 DB の内容を事前に見なくても
     直る）と判断した。
  3. **境界検査**（`crates/task-worker/src/claude_code.rs`）: `--session-id`/`--resume` に渡す直前に
     `crate::provider::is_valid_uuid` で検査し、不正なら spawn せず `AdapterError::Other` で拒否する
     （原因を含む文面）。将来の回帰がテストのスタブでは検出されずに本番でだけ壊れる、という今回と同じ
     失敗を再発させないための最後の砦。`is_valid_uuid` はハイフンの位置と 16 進数であることだけを見る
     決定的な判定（version/variant ビットの厳密な検査はしない）。
  4. **失敗経路の配線漏れを発見・修正**: `EventSink::session_resume_failed`（resume 拒否をその場で
     retire する。ADR-0054 D1 の設計どおり）は CoS の対話 run（`StoreSink`）には配線されていたが、
     **部門長のレビュー run（`ReviewerSink`）には Phase 67 で配線されていなかった**
     （`session_established`/`session_resume_failed` が `EventSink` の既定の no-op のままだった）。
     つまり Lead セッション（`kind = lead`）は resume が拒否されても一切 retire されず、同じ壊れた
     `session_id` で `--resume` を延々と再試行し続ける経路が残っていた。`ReviewerSink` に
     `session_key: Option<(String, SessionKind, Option<ProjectId>)>` を足し、`Dispatcher::pick_reviewer`
     が `(department_id, Lead, None)` を渡すようにして、`StoreSink` と同じ 2 メソッドを実装した。
- **テスト**（受け入れ条件 2・3・4 の「テストで覆う」への回答）:
  - `crates/task-worker/src/provider.rs`: `valid_uuid_accepts_hyphenated_hex_ignoring_case` /
    `valid_uuid_rejects_a_ulid_and_other_non_uuid_shapes`。
  - `crates/task-worker/src/claude_code.rs`: `a_non_uuid_resume_id_is_refused_without_spawning` /
    `a_non_uuid_fresh_session_id_is_refused_without_spawning`（spawn しなかったこと＝`args.log` が
    存在しないことまで見る）。既存の ULID 風 id（`01ARZ3NDEKTSV4RRFFQ69G5FAV`）を使っていたテストは
    すべて UUID（`550e8400-e29b-41d4-a716-446655440000`）に差し替えた。
  - `crates/task-dispatch/src/sessions.rs`: `a_claude_code_session_with_a_non_uuid_id_self_heals` /
    `a_non_uuid_session_id_is_fine_for_non_claude_code_adapters` /
    `session_id_is_valid_for_adapter_only_requires_uuid_for_claude_code` /
    `new_session_id_mints_a_uuid_only_for_claude_code` / `new_session_id_is_not_constant`。
  - `crates/task-dispatch/src/dispatcher.rs`:
    `resolve_node_session_self_heals_a_non_uuid_claude_code_session_id`（本番と同じ壊れた行を直接
    store に作り、`resolve_node_session` が retire して UUID の新規セッションに置き換えることを見る）、
    `a_rejected_lead_session_resume_retires_it_and_the_next_review_starts_fresh`（fake アダプタが
    `session_resume_failed` を報告 → その場で retire → 次の Reviewer run が新しいセッションで走ることを
    3 本の Reviewer run を通しで見る。`ReviewerSink` の配線漏れの回帰テスト）。
- **ゲート**: 各コマンドの結果は `docs/PROGRESS.md` の「Phase 67b」節に記録。
- **本番での確認**（未実施。デプロイ後に人 or エージェントが実施）: `docs/PROGRESS.md` の
  「Phase 67b」節「本番で確認すること」を参照。
## Phase 68 追記（実装時の逸脱・明確化。2026-09-21）

1. **「育つ返事」は `progress` の別ブロックではなく、`reply` の新しい状態にした**。本文は D2 で
   「その対話 run の progress を run 中に流す」としか書いていないが、実装は既存の `progress` ブロック
   （run ごとに折り畳む）を対話 run にも使うのではなく、`ConsoleBlock::Reply` に
   `state`（`streaming` | `done`、既定 `done`）・`thinking`（置き換え式の 1 行）・`steps[]`
   （`tool_use`/`tool_result` を順番どおり）を追加した。理由: `progress` の折り畳み（`first`/`last` が
   始め 3 行・終わり 3 行だけ）は「開いて見る」設計で、育つ吹き出しには使えない（本文を全部見せる必要が
   ある）。対話でない run（`task.conversation` が無い）は従来どおり `progress` のまま（`task_core::is_conversation`
   で振り分け。`crates/task-api/src/console.rs::event_blocks`）。対象は CoS の対話に限らず、**ノードとの
   対話（`@node`）も同じ扱い**にした（同じコードパスで自然にそうなる。D2 の本文は CoS に限定していない）。
   組み立ては `crates/task-ops/src/console.rs::group_conversation_progress`（thinking は置き換え、text は
   連結、tool_use/tool_result は先頭・末尾で切らずに積む。対話 run は `CONVERSATION_MAX_TURNS` で
   際限なく伸びないため）。
2. **SSE は「その接続で見た増分」を送る（`progress` と同じ実装の性質に合わせた）**。`poll_console` の
   `pending.blocks` は 1 秒ごとにその時点の値を送って**取り除く**ため、次のティックの育つ `reply` は
   增分（そのティックで新たに来た `text`/`steps`）だけを持つ（`progress` の `count`/`tool_count` も同じ
   性質で、実は「run 全体の累計」ではなく「前回送信からの増分」——コメントの「その run の合計」という
   記述と実装がずれているのは Phase 60a からの既存の状態で、今回は触っていない）。GUI 側
   （`~/lib/console.ts::appendConsoleBlock`）が `run_id`/`task_id` が同じ `reply` を見つけたら
   `text` を連結・`steps` を積み増す・`thinking` は空でなければ置き換える、という規約でクライアント側の
   積み上げを担う。`state = "done"` の `reply`（`messages` から来る、確定した本文そのもの）は増分では
   ないので置き換える。`GET /console`（履歴の初期表示）は `since` 無しなら窓の中の全イベントから
   1 回で組むので、こちらは最初から累計が乗る。
3. **CoS の対話 run に許す読み取りの道具は、アダプタごとに実現の強さが違う**（D2 は「celerisctl
   knowledge search|get、タスク・案件の一覧と詳細の read API」とだけ書いていて、道具単位で守れる保証までは
   規定していない）:
   - `claude-code`: `--allowedTools` に `Bash(celerisctl <サブコマンド>:*)` の形で 6 つ渡す
     （`knowledge search`/`knowledge get`/`ls`/`show`/`projects ls`/`projects show`）。claude-code の
     許可リストの仕組みそのものが「無いものは拒否」なので、この 6 つ以外は使えない。
   - `codex`: 道具単位の許可リストが無いため、`sandbox_mode="read-only"` にした（読み取り以外の
     ファイル書き込み・任意コマンド実行を丸ごと塞ぐ、より粗い保証）。`--add-dir <artifacts_dir>` は
     read-only でも書ける前提（celeris が「結果ファイルを書く場所」として明示している例外。**実機の
     codex CLI で確認していない**。ADR-0009 P-34 のとおり、認証・ネットワークが使える環境の人or
     エージェントに確認を依頼する）。
   - `acp`: `session/request_permission` に道具の識別子がこのコードベースが解釈できる形で乗らない
     （ACP エージェント実装依存。オープンな `toolCall` 構造で、celerisctl のサブコマンドと機械的に
     対応づける手段が無い）。**読み取りの許可リストを作る代わりに、対話 run 中は道具の許可要求を
     一律拒否（fail-closed）にした**。効果は「D2 が意図した読み取りの道具が使えることの保証」ではなく
     「D2 が禁止したかった書き込みが誤って通ることが無い保証」だけ。ACP 経由の CoS 対話は、モデルが
     許可要求を経ない組み込みの読み取りに頼るしかない（採用しない場合は次善: `celerisctl` を MCP
     サーバとして ACP エージェントに登録し道具名で識別する経路を別途設計する。今回は見送り）。
   - `celerisctl` に新しい読み取り専用コマンド `projects ls|show`（`crates/celerisctl/src/commands/projects.rs`）
     を足した（既存の `ls`/`show` はタスクだけで、案件の一覧・詳細が無かった）。
   - 新しい `[adapters.*]` 設定は増やしていない（固定の一覧・固定のフラグで決定的に決まる。
     `req.context.conversation_addressee == Some(ConversationAddressee::Secretary)` で判定。
     `RunContext` は Phase 28 から既にこの値を持っている）。
4. **入力のキューは、既存の直列化（Phase 27 監査 M-3 の `depends_on`）に 1 つバグがあった**。
   `task_ops::conversation::open_conversation_tasks` は「同じノード・**同じ `project_id`**」の未終了の
   対話タスクだけを直列化していたが、CoS の継続セッション（`node_sessions`）は D1 のとおり
   `project_id` に関わらず全体で 1 本なので、案件つきの一言（`scope=project:<id>`）と案件なしの一言
   （`scope=all`）を続けて打つと、直列化されずに 2 つの run が同じ継続セッションにぶつかる余地があった。
   `node_id == task_core::COS_ID` のときだけ `project_id` を無視して直列化するよう直した（他のノードは
   従来どおり案件ごと）。「投げると run 中でも打てて、次の run になる」という D2 の入力欄の挙動自体は
   Phase 27 からの `depends_on` の仕組みでそのまま満たされていた（`ready_tasks` は `depends_on` が
   全部 `done` のタスクしか返さない）ので、新しいキューの実装は追加していない。
5. **`ConsoleAction` 実行後の `task` ブロックは、新しいコードを足さずに自然に「返事の直下」に出る**。
   D2 の「actions を出したら run はそこで終わる」「作ったタスクは task ブロックとして返事の直下に出る」は、
   Phase 60b の `absorb_console_actions`（run の完了処理の中で 1 回だけ実行）と、通常の
   `Event::Transitioned` → `task` ブロックの経路（`crates/task-api/src/console.rs`）がそのまま満たす。
   時刻順に並べる Console の一本の流れの性質上、作成イベントの時刻が返事の確定より後ろに来るため
   「直下」になる。Phase 68 で変更した点は無い（既存テスト
   `absorb_console_actions_executes_the_declared_actions_for_the_cos_only` /
   `record_conversation_reply_runs_actions_and_attaches_the_result` が green のままなことで確認）。
6. **組織画面の「継続中のセッション」は `GET /org` に `lead_sessions[]` を足しただけ**（新しい
   エンドポイントは作らなかった）。`OrgList` に `effective_profiles[]` と同じ「`items` とは別に、対応する
   ものだけ渡す配列」の形で追加した（`NodeSessionSummary { node_id, turns, approx_tokens,
   last_used_at }`）。部門長（`OrgKind::Department`）だけを対象にし、CoS の対話セッションは対象外
   （Console のチャット欄自身が状態を見せるため。D3 の「部門長のセッションは…組織画面のノードに出す
   （会話 UI は作らない）」の記述どおり）。
7. **未解決事項**:
   - codex の `sandbox_mode="read-only"` + `--add-dir` の組み合わせで実際に `artifacts/result.json` を
     書けるかは実機で確認していない（上記 3）。書けないと分かれば、`artifacts_dir` を writable_roots に
     別途明示する codex 側の設定（`-c sandbox_workspace_write.writable_roots=[...]`）が要るかもしれない。
   - ACP の読み取り道具の許可は fail-closed（上記 3）。ACP を CoS の主アダプタとして使う運用になったら、
     MCP 経由の道具登録を検討する（別 Phase）。
   - claude-code の `--allowedTools` に渡した `Bash(celerisctl <サブコマンド>:*)` が、実際の claude-code CLI
     のグロブ構文と一致するかは実機で確認していない（`Bash(cmd:*)` は claude-code のドキュメントに
     ある書式のつもりだが、サンドボックスにはネットワーク・実 CLI が無いため fake アダプタでの
     引数検査までしかできていない）。
   - Phase 67 の未解決事項 1（要約と直近のやり取りの重複）は今回も直していない（対話の前置きに
     触れる別の Phase でまとめて直す方が良いと判断）。
