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

## Phase 67c 追記（継続セッションは同じアダプタ・アカウントに留まる。2026-09-21）

- **観測（本番、2026-09-21 14:27 UTC、release `5b3b0a649bfa`）**: CoS に連続で 2 回指示した。1 回目
  claude-code で `--session-id <UUID>`・`resume:false`・正常終了（usage in 4 / out 191 tokens。Claude
  のキャッシュで全量前置きが安く済んだ）。2 回目、ADR-0049 の残量ランキングが **codex /
  chatgpt_plus_personal** を最上位に選んだため、Claude のセッションは `AccountChanged` として retire
  され、codex の新規セッションが `resume:false`・**全量前置き（input 52,629 tokens）**で走った。
  `node_sessions` は「旧 ULID 行（自己修復で retire 済み）」「Claude の UUID 行（アカウント変更で
  retire）」「codex の新規行（`turns=1`）」の 3 本になった。D1 は「同じアカウントで続け、そのアカウントが
  枯渇したときだけ新しいセッションを作る」と決めていたのに、**毎 run 素の ADR-0049 ランキングを先に
  走らせていた**ため、スコアがわずかに逆転しただけでアダプタごと変わり、5 万トークン級の前置きが
  毎回再送される状態になっていた。
- **決定 1: sticky 選択**（受け入れ条件 1）: `Dispatcher::select_provider` の入口で、この run に対応する
  現役セッション（`retired_at IS NULL` かつ `approx_tokens < rollover_tokens`）があれば、まずその
  `(adapter, account_id)` に留まれるかを試す。使えると判れば ADR-0049 のランキングを一切走らせずそれを
  返す。使えなければ（アカウントが cooldown・枯渇・未ログイン、または設定からそのプロバイダが消えた）
  これまでどおりのランキングへフォールバックし、選ばれたアカウント・アダプタが元のセッションと違えば
  既存の `crate::sessions::decide` が `AccountChanged`/`AdapterChanged` で retire する（この経路は
  Phase 67 のまま。新しく作ってはいない）。
  - 判断そのものは純粋関数 `crate::sessions::decide_sticky(active, rollover_tokens, account_usable,
    provider_offers_tier) -> StickyDecision { Stick | FallBack }`（`crates/task-dispatch/src/sessions.rs`）。
    `account_usable`/`provider_offers_tier` の 2 つの bool は呼び出し側が集める（I/O）。テスト:
    `a_usable_session_sticks`（使える → stick）、`an_account_in_cooldown_falls_back`（cooldown →
    フォールバック）、`a_provider_removed_from_config_falls_back`（設定から消えた → フォールバック）、
    `no_active_session_never_sticks`、`a_session_past_rollover_falls_back_even_if_otherwise_usable`、
    `a_poolless_session_sticks_without_an_account_check`。
  - I/O は `Dispatcher::sticky_provider`（新規）が行う: `account_usable` は
    `crate::accounts::evaluate` の除外判定（ログイン・cooldown・上限・枯渇。既存の `pick_account` と
    同じ材料を、ベストスコアを探す代わりに特定の 1 件だけ見る。`Dispatcher::account_usable` 新規）。
    `provider_offers_tier` は「同じ tier かそれ以上」（`crate::sessions::tier_rank`。`Frontier` が
    最上位、`Cheap` が最下位という決定的な順位付けだけ。値の意味を捏造しない）を、要求された tier から
    順に、`hint.adapter` をセッションのアダプタ 1 つに固定した `ProviderPolicy::select` に確かめさせる
    （`Dispatcher::matching_provider_for_adapter` 新規。cooldown・並列度上限の扱いは通常のランキングと
    同じ規則になる）。
  - 呼び出し箇所は 2 つ（受け入れ条件どおり）: CoS の対話 run（`dispatch_ready` が
    `Dispatcher::cos_conversation_session` 新規でこの run が CoS 宛てか判定してから
    `select_provider` を呼ぶ）と `pick_reviewer`（部署が決まった時点で
    `node_session_active(department, Lead, None)` を読んでから `select_provider` を呼ぶ）。
    `select_provider` のシグネチャに `sticky_session: Option<&NodeSession>` を足した（テストの呼び出し
    元はすべて `None` に更新）。
  - 統合テスト `dispatcher::tests::select_provider_sticks_to_the_sessions_account_over_a_better_scoring_one`:
    2 アカウント（`a`/`b`）で `b` の方が高スコアになる観測値を仕込み、sticky 無しなら `b` が選ばれる
    ことをまず確認した上で、`a` を指すセッションを渡すと `b` の方が勝っていても `a` に留まること、`a`
    が認証失敗で cooldown に落ちたら通常のランキング（`b`）へフォールバックすることを確認した。
- **決定 2: Codex のセッション id はアダプタ自身が確定させた id しか resume に使わない**（受け入れ条件
  2）: 産物の `node_sessions` 行 `01a0c45d-7612-…`（celeris が発行した UUID 風の id）は Phase 67 のまま
  celeris がセッション作成時に**先取りしていない**（`crate::sessions::new_session_id("codex")` は空文字
  を返す設計。実際に走っていたのは 1 回目 run のみ・`resume:false` で `session_established` が
  `thread.started` の `thread_id` を書いた結果であり、celeris が id を作って `resume` に使っていたわけ
  ではなかった。念のため `codex.rs` を読み直して確認: `resume_id` は
  `codex_session.filter(|s| s.resume).map(|s| s.session_id.clone())` で、`resume` は
  `crate::sessions::decide` が `Resume` を返した run にしか立たず、初回は必ず `Fresh(NoActive)`
  （`resume:false`）なので初回に `resume` が送られることは元から無い）。
  - 見つかった実際の穴: **id を確定できなかった（`thread.started`/`session_configured` に想定した
    フィールド名が無い等）まま `node_sessions` の行が空文字の `session_id` で残ると、次の run が
    「現役セッションがある」と誤認して `resume:true` で空文字を渡してしまう**
    （`codex exec resume ""` / ACP の `sessionId: ""`）。`crate::sessions::session_id_is_valid_for_adapter`
    は Phase 67b では `claude-code` の UUID しか見ておらず、空文字はどのアダプタでも「形式は問わない」
    として通っていた。Phase 67c でこれを塞いだ: **空文字はどのアダプタでも無効**とし、既存の自己修復
    経路（`FreshReason::InvalidSessionId`。retire → 新規セッション、要約付き）にそのまま乗せた。
    テスト `sessions::tests::an_empty_session_id_self_heals_for_every_adapter`（`claude-code`/`codex`/
    `acp` の 3 アダプタとも）。
  - あわせて `crates/task-worker/src/codex.rs` の id 捕捉を広げた: `thread.started` に加えて
    `session_configured`（実装によってはこちらの type 名で報告することがある）も見る。フィールド名は
    `thread_id`/`threadId`/`session_id` のどれでも拾う（実機のフィールド名は依然未検証。誤って読めなくて
    も上の自己修復が効くので run 自体は失敗しない）。テスト
    `codex::tests::session_configured_with_a_session_id_reports_session_established`、
    `codex::tests::thread_started_without_an_id_does_not_report_session_established`（id が無ければ
    `session_established` を呼ばないこと自体を明示的に確認）。
  - `codex exec resume <id>` を celeris が先取りした id では送らない、という受け入れ条件の性質は
    Phase 67 の設計どおり最初から満たされていた。今回の変更は「id を確定できなかったときに空文字の
    まま resume されてしまう」抜け穴を塞いだことが実質的な修正。
- **決定 3: ACP は変更不要**（受け入れ条件 3）: `crates/task-worker/src/acp.rs` はもともと `session/new`
  が返した `sessionId`（無ければ `session/load` は渡した id をそのまま）でしか `session_established` を
  呼ばず、`sessionId` が無ければ run 自体を失敗させる（celeris が id を先取りすることは無い）。ただし
  ACP にも codex と同じクラスの穴があった: `session/new` が失敗して `sessionId` を得られなかった run の
  後、`node_sessions` の行が空文字の `session_id` のまま残ると、次の run が空文字を `session/load` に
  渡してしまう。これは ACP 固有のコードではなく決定 2 の空文字チェック（`session_id_is_valid_for_adapter`）
  で共通に塞がれる（`acp` も `an_empty_session_id_self_heals_for_every_adapter` の対象）ので、`acp.rs`
  自体への変更は無し。
- **ゲート**: 各コマンドの結果は `docs/PROGRESS.md` の「Phase 67c」節に記録。
- **本番での確認**（未実施。デプロイ後に人 or エージェントが実施）: `docs/PROGRESS.md` の
  「Phase 67c」節「本番で確認すること」を参照。

## Phase 68b 追記（`codex exec resume` の argv — Phase 68 の read-only サンドボックスの回帰。2026-09-21）

- **観測（本番障害、2026-09-21 15:04 UTC、release `b24bae9a796a`）**: CoS の対話 run が **codex**
  （67c 未配備なので ADR-0049 の残量スコアで codex が選ばれた）に割り当たり、`session.resume=true` の
  run が 2 回とも exit 2 で失敗した:
  ```
  error: unexpected argument '--add-dir' found
    tip: to pass '--add-dir' as a value, use '-- --add-dir'
  Usage: codex exec resume --json --skip-git-repo-check --config <key=value> <SESSION_ID> [PROMPT]
  ```
  Phase 68（D2、CoS の対話 run を read-only サンドボックスにする）が `codex exec` の argv に
  `sandbox_mode="read-only"`（`-c`）と `--add-dir <artifacts_dir>` を無条件に足していたが、
  `codex exec resume <id>` は別の clap サブコマンドで `--add-dir` を受け付けない。
- **原因の確認（実機の CLI ヘルプ。codex-cli 0.155.1。ADR-0009 P-34 のとおり、認証・ネットワークを
  使わない read-only な `--help` 呼び出し 2 本だけをこの Phase で実行して確認した）**:
  ```
  $ ~/.local/bin/codex exec --help
  Usage: codex exec [OPTIONS] [PROMPT]
         codex exec [OPTIONS] <COMMAND> [ARGS]
  OPTIONS（抜粋）: -c/--config <key=value>, -m/--model <MODEL>, --add-dir <DIR>, --json,
  --skip-git-repo-check, -s/--sandbox <SANDBOX_MODE> ...

  $ ~/.local/bin/codex exec resume --help
  Usage: codex exec resume [OPTIONS] [SESSION_ID] [PROMPT]
  OPTIONS（抜粋）: -c/--config <key=value>, --last, --all, -m/--model <MODEL>, --json,
  --skip-git-repo-check ...
  ```
  `exec resume` の OPTIONS 一覧には `--add-dir` も `-s/--sandbox` も無い（`exec` にはどちらもある）。
  一方 `-c/--config` はどちらの usage 行にもある。つまり **`sandbox_mode` の指定（`-c
  sandbox_mode="read-only"`）は fresh でも resume でもそのまま通るが、`--add-dir` は resume では拒否
  される**。
- **変更**: `crates/task-worker/src/codex.rs::run_codex` に `is_exec_resume_subcommand`
  （`resume_id.is_some() && resume_mode == ExecResume` のときだけ true。`codex exec resume <id>` の
  形で呼ぶ run）を導入し、**`--add-dir` はこの形のときだけ付けない**ようにした。`-c
  sandbox_mode="..."` は変更なし（fresh・resume どちらも従来どおり付く。CoS の対話 run は resume でも
  read-only の意図を保つ）。`-c experimental_resume=<id>`（`CodexResumeMode::ExperimentalResume`）は
  そもそも `exec resume` サブコマンドを使わず普通の `codex exec` のままなので `--add-dir` は従来どおり
  付く（変更なし）。
  - **fresh run が artifacts dir に書ける手段**: 引き続き `--add-dir <artifacts_dir>`（`exec` の
    usage 行にある正規のオプション）。`-c sandbox_workspace_write.writable_roots=[...]` のような
    config 差し替えは、fresh run では `--add-dir` がそのまま通るため不要と判断した（採らない）。
  - **resume run が artifacts dir に書ける手段**: `exec resume` には writable-roots を指定する
    フラグが usage 行に一つも無い（`--add-dir` も `-C/--cd` も無く、`-c/--config` はあるが
    `sandbox_workspace_write.writable_roots` を resume 時に個別スレッドへ再適用できるかは
    `--help` からは確認できない）。celeris 側の対処は、**resume 先のスレッドは最初の（非 resume の）
    `exec` 呼び出しで受け取った `--add-dir` の grant をそのまま引き継ぐ前提**で `--add-dir` を単に
    落とすことにした（このセッションが生まれた最初の run は必ず `resume:false` で始まり、
    `crate::sessions::decide` の設計どおり fresh run は常に plain `codex exec` を通るため、
    `--add-dir` を受け取っている）。**この前提（resume 先が fresh 時の writable-roots を保持する
    こと）は `--help` からは確認できず、実機の codex CLI では未検証**（下記「未解決事項」参照）。
- **テスト**（`crates/task-worker/src/codex.rs`。3 本、いずれも argv を完全一致で検査し、usage 行を
  コメントとして貼った）:
  - `phase_68b_fresh_cos_run_argv_has_readonly_sandbox_and_add_dir`: (a) CoS の fresh run。
    `["exec", "--json", "--skip-git-repo-check", "-c", "sandbox_mode=\"read-only\"", "--add-dir",
    <artifacts_dir>, <prompt>]`。
  - `phase_68b_resume_cos_run_argv_drops_add_dir_keeps_readonly_sandbox`: (b) CoS の resume run
    （production の再現）。`["exec", "resume", <id>, "--json", "--skip-git-repo-check", "-c",
    "sandbox_mode=\"read-only\"", <prompt>]`。`--add-dir` が argv のどこにも無いことを明示的に確認。
  - `phase_68b_normal_run_argv_unchanged`: (c) 通常（非 CoS）の run。`["exec", "--json",
    "--skip-git-repo-check", "-c", "sandbox_mode=\"workspace-write\"", "--add-dir", <artifacts_dir>,
    <prompt>]`（Phase 68b の変更が対話以外の run に影響しないことの回帰）。
  - 既存の `the_cos_conversation_run_gets_a_readonly_sandbox` / `non_cos_runs_keep_the_workspace_write_sandbox`
    / `a_continuing_session_uses_exec_resume_by_default` / `a_continuing_session_uses_experimental_resume_when_configured`
    / `command_line_has_exec_json_model_then_prompt_as_last_arg` は変更なしで green のまま（fresh・
    experimental_resume・非対話 run の argv は変えていないことの回帰）。
- **ゲート**:
  - `cargo test --workspace --no-fail-fast` → **exit 0。1710 passed / 0 failed**（73 個の
    `test result:` ブロックを合計。`grep -c "test result: FAILED"` = 0。`task-worker --lib codex::`
    だけで 38 passed、うち新規 3 本は上記テスト節のとおり）。
  - `cargo clippy --workspace --all-targets -- -D warnings` → **exit 0、警告 0**。
  - `unwrap()`: 今回の diff（`crates/task-worker/src/codex.rs`）で追加した `unwrap()` は無い
    （`is_exec_resume_subcommand` の判定は既存の `if let` パターンのみで、テスト以外のコードに
    `unwrap()` を足していない）。
  - ディスパッチャ・ストアに LLM 呼び出しを入れていない（今回の変更は `task-worker` の argv 組み立て
    のみ。純粋な文字列・分岐操作）。
  - schema 変更なし。
- **実機での確認（未実施。ADR-0009 P-34。デプロイ後に人 or エージェントが実施）**:
  1. `release.sh` → `verify.sh` → `promote.sh` でこの修正をデプロイする。
  2. CoS の対話セッションが codex（アカウント枯渇や 67c の sticky 選択のフォールバック等で）に
     割り当たった状態で 2 回連続で指示を送り、1 回目（`resume:false`）・2 回目（`resume:true`）とも
     exit 2 にならず正常終了することを確認する（`runs/<id>/stderr.log` に `unexpected argument
     '--add-dir'` が出ないこと）。
  3. resume 側の run が `artifacts/result.json` を実際に書けること（fresh 時の `--add-dir` の grant が
     resume でも有効という上記の未検証の前提の裏取り）。書けなければ、resume 側にも writable-roots を
     渡す別の手段（`-c sandbox_workspace_write.writable_roots=[...]` が resume で解釈されるか、
     等）を別 Phase で調べる。
- **未解決事項**:
  - resume 先のスレッドが fresh 時に付与した `--add-dir` の writable-roots を引き継ぐという前提は
    `--help` の出力からは確認できず、実機で未検証（上記「実機での確認」2・3 参照）。引き継がないと
    分かった場合、CoS の resume run は read-only サンドボックスのまま `artifacts/result.json` を
    書けずに失敗し続ける可能性がある。
  - `codex exec resume --help` の Usage 行が `<SESSION_ID> [PROMPT]` を `--config` の後ろに置いている
    （celeris の実装は `resume <id>` を `exec` の直後、`--json` 等より前に置く）。clap は通常オプション
    と位置引数の順序を混在させても解釈できるため動作上は問題ないと考えているが、これも実機の
    `codex exec resume` 呼び出しでの確認はできていない（`--help` の表示だけでは呼び出し順の厳密さまでは
    分からない）。

## Phase 68c 追記（`codex exec resume` の argv をホワイトリスト方式に変更。2026-09-21）

Phase 68b の本番反映後、`--add-dir` は直った一方で別のフラグが resume で拒否される事象が出た。
「`exec` が受け付けて `exec resume` が受け付けないフラグを都度 1 個ずつ引き算する」やり方は同じ穴を
繰り返すと判断し、resume は**ホワイトリスト方式**に変えた。

### 観測（本番障害、2026-09-21 15:44 UTC、release `a2942d5d8a94`。Phase 68b が本番に乗った状態）

CoS の対話が codex に割り当たった。fresh run は成功（Codex 自身が確定させた thread id
`01a0c4a2-ce4e-7751-8726-fc35845322a9` を捕捉、`done`）。しかし続く resume run が 2 回とも exit 2:

```
error: unexpected argument '--approve-for-me' found
  tip: to pass '--approve-for-me' as a value, use '-- --approve-for-me'
Usage: codex exec resume --json --skip-git-repo-check --config <key=value> <SESSION_ID> [PROMPT]
```

`--approve-for-me` は celeris 自身が付けているフラグではなく `[adapters.codex] extra_args`（運用側の
設定。承認モードを自動化する codex 側のフラグ）から来ている。Phase 68b の変更は `--add-dir` だけを
resume で落としていたが、`extra_args` は無条件に付け続けていたため、`--approve-for-me` がそのまま
resume に渡って拒否された。

### 原因の確認（`~/.local/bin/codex exec resume --help` を再実行。codex-cli 0.155.1。Phase 68b の確認と
テキストは同一だった）

```
Usage: codex exec resume [OPTIONS] [SESSION_ID] [PROMPT]
Options（全量）: -c/--config <key=value>, --last, --all, --enable <FEATURE>, --disable <FEATURE>,
-i/--image <FILE>, --strict-config, -m/--model <MODEL>, --dangerously-bypass-approvals-and-sandbox,
--dangerously-bypass-hook-trust, --worktree, --thread-source <SOURCE>, --skip-git-repo-check,
--ephemeral, --ignore-user-config, --ignore-rules, --output-schema <FILE>, --json,
-o/--output-last-message <FILE>, -h/--help
```

`--add-dir`・`-s/--sandbox`・`--approve-for-me` はどれも無い（Phase 68b の確認と同じ）。しかし
**`--help` の OPTIONS 一覧には `-m/--model` が載っているのに対し、本番の実際のエラーが示した usage 行は
`--json`・`--skip-git-repo-check`・`--config`（＋位置引数）だけに絞られていた**。つまり「`--help` に
載っている＝実際に受け付けられる」という Phase 68b の前提そのものが崩れた可能性がある（デプロイ済み
バイナリの実際の受理集合が `--help` の記載より狭い）。この Phase で許可されたコマンドは
`--help` の再実行だけで、`codex exec resume` 自体を実行して実際の受理集合を確かめることはできない。

### 決定: resume の argv はホワイトリスト方式

「`exec` が受け付けるが `exec resume` は受け付けない、と分かったフラグを都度落とす」引き算方式をやめ、
**`exec resume` では本番のエラーが示した usage 行が列挙する形だけを組み立てる**方式にした:
`--json`・`--skip-git-repo-check`・`-c/--config <key=value>`（複数可）・`<SESSION_ID>`・`[PROMPT]`。
`crates/task-worker/src/codex.rs::run_codex` の変更:

- `--json`・`--skip-git-repo-check`・`-c sandbox_mode="..."` は fresh・resume とも変更なし
  （whitelist に含まれる）。
- `--model` は resume のときだけ `-c model="..."` に変換する（`codex exec --help` 自身の例
  `-c model="o3"` が `-c` 経由の等価形として明記されている。whitelist の `-c` に収まる）。
- `--add-dir`（Phase 68b で resume では落とす、と決めた）は変更なし。
- **`[adapters.codex] extra_args`（運用側の任意フラグ。`--approve-for-me` 等）は resume では丸ごと
  落とす**（`git diff` 参照。個々のフラグに `-c` 等価があるかは一般には分からないため、既知でない
  フラグを一つずつ引き算する方式には戻さない）。`extra_args` が空でなければ
  `tracing::warn!("run {run_id}: dropping codex extra_args on \`exec resume\` ...")` で運用側に見える
  ようにした。resume 先のスレッドは、それを生んだ最初の（非 resume の）`exec` 呼び出しで受けた承認・
  サンドボックス設定をそのまま引き継ぐ前提（未検証。下記「未解決事項」）。

### テスト（`crates/task-worker/src/codex.rs`）

- `phase_68c_resume_argv_contains_no_flag_outside_the_whitelist`（新規）: `model` と
  `extra_args = ["--approve-for-me"]` の両方を仕込んだ resume run で、argv 中の `-` で始まるトークンが
  すべて `["--json", "--skip-git-repo-check", "-c"]` のいずれかであることを検査し、
  `--approve-for-me`・`--add-dir`・`--model` がどこにも無いこと、`model="gpt-5-codex"`・
  `sandbox_mode="read-only"` が `-c` 経由で乗っていることを確認する（production の再現・回帰）。
- 既存の `phase_68b_fresh_cos_run_argv_has_readonly_sandbox_and_add_dir` /
  `phase_68b_resume_cos_run_argv_drops_add_dir_keeps_readonly_sandbox` /
  `phase_68b_normal_run_argv_unchanged` / `the_cos_conversation_run_gets_a_readonly_sandbox` /
  `non_cos_runs_keep_the_workspace_write_sandbox` / `a_continuing_session_uses_exec_resume_by_default` /
  `a_continuing_session_uses_experimental_resume_when_configured` /
  `command_line_has_exec_json_model_then_prompt_as_last_arg` は変更なしで green のまま（model・
  extra_args を使わない従来のケースの argv は変わっていないことの回帰）。

### ゲート

- `cargo test -p task-worker --lib codex::` → **exit 0、39 passed**（Phase 68b の 38 + 新規 1）。
- `cargo test --workspace --no-fail-fast` → **exit 0。1711 passed / 0 failed**（73 個の
  `test result:` ブロックを合計。`grep -c "test result: FAILED"` = 0）。
- `cargo clippy --workspace --all-targets -- -D warnings` → **exit 0、警告 0**。
- `unwrap()`: 今回の diff（`crates/task-worker/src/codex.rs`）で追加した `unwrap()` はすべて
  `#[cfg(test)] mod tests` 内（新規テストのセットアップのみ。`git diff` で追加行を目視確認）。
- ディスパッチャ・ストアに LLM 呼び出しを入れていない（argv 組み立てとログのみの変更）。schema 変更なし。
- 変更ファイルは `crates/task-worker/src/codex.rs` のみ（`git status --short` で確認）。`gui/`・本番
  パス・ports・systemctl・credential には触れていない。

### 実機での確認（未実施。ADR-0009 P-34。デプロイ後に人 or エージェントが実施）

1. `release.sh` → `verify.sh` → `promote.sh` でこの修正をデプロイする。
2. CoS の対話セッションが codex に割り当たった状態で fresh → resume と連続で指示を送り、resume 側が
   `--approve-for-me` を含む運用の `extra_args` があっても exit 2 にならず正常終了することを確認する
   （`runs/<id>/stderr.log` に `unexpected argument` が出ないこと）。
3. resume run が `artifacts/result.json` を実際に書けること（`--add-dir` を落としても fresh 時の
   writable-roots が引き継がれるという Phase 68b からの未検証の前提の裏取り。今回も検証できていない）。
4. resume run で運用が期待する承認モード（`extra_args` の `--approve-for-me` 相当）が実際に効いている
   か（引き継がれない場合、resume 中は既定の承認モードに戻る可能性がある。動作に影響があれば別 Phase
   で対応を検討する）。

### 未解決事項

- `codex exec resume --help` の OPTIONS 一覧（`-m/--model` を含む）と、本番の実際のエラーが示した
  受理集合（`--json`・`--skip-git-repo-check`・`--config` のみ）が食い違っている理由は分かっていない
  （デプロイ済みバイナリのバージョン差・ビルド差・設定差のいずれかが疑わしいが、`--help` の再実行以外の
  実機操作がこの Phase では許されていないため特定できていない）。**`--config` 経由に倒した `-c
  model="..."` が実際に resume で受理されるかも、今回も実機未検証**（`--help` の記載上は `-c` は両方の
  Usage 行にあるが、Phase 68c 自体が「`--help` の記載と実際の受理集合は一致しないことがある」という
  教訓から生まれている）。
- resume 先のスレッドが fresh 時の `--add-dir`（writable-roots）や `extra_args`（承認・サンドボックス
  設定）を引き継ぐという前提はどちらも実機未検証のまま（Phase 68b から持ち越し、今回も解消していない）。
  上記「実機での確認」2〜4 で確かめる。
- 今後また `exec resume` で未知のフラグが拒否される場合、ホワイトリスト方式なのでその新しいフラグは
  celeris 側で追加していない限りそもそも argv に乗らない（同じクラスの障害の再発は原理的に防げている）。
  ただし `-c` の値（`sandbox_mode`・`experimental_resume`・`model`）が resume で本当に効くかどうかの
  実機確認は残っている（上記）。

## Phase 98 追記（2026-09-22）

本番（2026-09-22 00:18 UTC、task 01M337NT3QT1FR1G6WHS9G6NDA、codex-cli 0.155.1）: CoS の `resume:true`
run が **イベントを一つも出さずに** exit 1、stderr は 1 行だけ `Error: thread/resume: thread/resume
failed: list_turns is not supported yet (code -32601)`。この文言は `RESUME_REJECTION_PATTERNS`
（「セッションが見つからない」系）のどれにも一致せず、`session_resume_failed` が呼ばれないまま
`worker exited without a turn.completed/turn.failed message` の retryable エラーとして次 run に持ち越され
ていた。原因は `session not found` 系の**拒否**ではなく、このインストールの codex-cli が `exec resume` の
JSON-RPC メソッド（`thread/resume`）自体を実装していないこと（同じセッションでは何度リトライしても直らない）。

`crates/task-worker/src/codex.rs::run_codex` を、1 回分の spawn+読み取りを `run_codex_once` に切り出した
上で、resume 済みの run が「イベント 0・非 0 exit・stderr に `thread/resume`/`-32601`/`resume`」
（`provider::looks_like_resume_rpc_failure`。`looks_like_resume_rejection` とは別パターン集合）に一致した
ときだけ `sink.session_resume_failed` を呼び、**同じ run の中で** resume 無しの fresh `codex exec` として
1 回だけやり直すように直した（1 回で直らなければ通常のエラー扱い。次の run を待たない分、CoS の対話が
その場で復旧する）。stub codex によるテスト（`a_resume_rpc_failure_self_heals_within_the_same_run` /
`a_non_resuming_run_is_unaffected_by_the_resume_rpc_self_heal`）で、resume 失敗 1 回・fresh セッション id
の報告 1 回・resume していない run では何も変わらないことを確認した。実機確認は未実施（ADR-0009 P-34）。
