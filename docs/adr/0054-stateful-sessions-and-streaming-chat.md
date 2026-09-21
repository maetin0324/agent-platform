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
