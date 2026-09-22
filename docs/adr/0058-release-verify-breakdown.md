# ADR-0058: `/releases` に検証・ゲートの内訳を出す

- 日付: 2026-09-22
- 状態: **Accepted**
- 関連: ADR-0040 D6（`GET /releases` の契約）、ADR-0041 D2（検査 2 の直列化・検査 4b）/D5（煙試験・検査 6）、
  ADR-0055（GUI の判断ロジック再実装の禁止）、`gui/docs/PROGRESS.md` Phase G38 の提案 P-G38-1

## 1. 文脈

`scripts/selfdeploy/verify.sh` は検査ごとに `record <id> <name> <ok> <detail> [task_id] [elapsed_s]` を呼び、
`verify.json` の `checks: [{id, name, ok, detail, task_id, elapsed_s}]`（id は `"1"`〜`"6"`、`"4b"`）に
既に書き出している。`release.sh` の `gate.json` も `steps: [{step, exit, secs, log}]` と `failed_step` を
既に持つ。しかし `crates/task-api/src/types.rs::ReleaseVerify` は `ok`/`live_ok`/`at` の集計値だけを運び、
`ReleaseItem` はゲートの `ok`（`gate_ok`）しか運ばない。そのため `gui/app/lib/releases.ts::
releaseVerifyCheckGroups` は「検査 1〜4・4b・6」「検査 5」の 2 グループしか作れず（Phase G38 で確認済み、
提案 P-G38-1）、失敗したときにどの検査・どのゲート段が落ちたかが GUI から分からない。

## 2. 決定

### D1. `ReleaseVerify` に `checks: Vec<ReleaseVerifyCheck>` を足す

`verify.json` の `checks[]` を**そのまま**運ぶ。GUI 側は個別の合否を再計算せず、celeris が書いた値を
表示するだけ（`gui/CLAUDE.md`「判断ロジックの再実装」の禁止を守る）。

```rust
pub struct ReleaseVerifyCheck {
    pub id: String,
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub elapsed_s: Option<f64>,
}
```

`task_id` は運ばない（GUI が使う予定が無く、タスク id を一覧 API に出す理由が無い。要れば後で足せる）。

### D2. `ReleaseItem` に `gate: Option<ReleaseGate>` を足す。既存の `gate_ok` は残す

```rust
pub struct ReleaseGate {
    pub ok: bool,
    pub failed_step: Option<String>,
    pub steps: Vec<ReleaseGateStep>,
}
pub struct ReleaseGateStep {
    pub step: String,
    pub exit: i32,
    pub secs: f64,
}
```

`gate_ok` を消さないのは、Phase 48 以前の GUI コード・他の API 消費者が `gate_ok` だけを見ていても壊れない
ようにするため（後方互換。ADR-0040 D6 の契約に対する**追加のみ**）。`gate.json` の `log`（各段のログの
ファイル名）は運ばない — celeris の外に出しても GUI はそのファイルを読めない（本番ホストのローカルパス）
ので、意味のある情報にならない。

### D3. 後方互換: どちらも欠けていれば空配列・`None`

`verify.json` に `checks` が無い（この Phase 以前に作られたリリース）ときは `checks: []` を返す
（`ReleaseVerify` 自体は `verify.json` があるときだけ `Some`。中身の欠落だけをここで吸収する）。
`gate.json` が壊れている・`steps` が無いときは `gate: None`（既存の `gate_ok` の `problem` 経由の
扱いをそのまま踏襲。壊れた JSON は一覧全体を落とさない）。

### D4. GUI は「検査ごと・段ごとの 1 行」を追加するだけ。既存の 2 グループ表示は互換のまま残す

`releaseVerifyCheckGroups`（Phase G38）は `verify.checks` が空のときのフォールバックとして残す。
`checks` があれば、そちらを使って検査ごとに 1 行（id・name・`ok` から作る 1 語バッジ・`detail`）を出す。
ゲートも同様に、`gate.steps` があれば段ごとに 1 行（`step`・`exit`・`secs`）を出し、`failed_step` と
一致する行を強調する。**celeris が既に出している `ok`/`exit` の値をそのまま表示するだけ**で、GUI 側での
真偽判定の再計算はしない（ADR-0055 の方針のまま）。

## 3. 採らない

- `verify.json`/`gate.json` の**形そのもの**を変える（`scripts/selfdeploy/*.sh` は触らない。既存の形を
  読むだけ）。
- 個別の検査・段を GUI で並べ替えたり、`ok` を他のフィールドから作り直したりする（celeris の判断を
  GUI で再実装しない）。
- `task_id`（検査 6 の煙試験タスク id）や `log`（ゲート各段のログファイル名）を API に載せる（本番ホスト
  ローカルの情報で、GUI から使い道が無い）。

## 4. 受け入れ条件

celeris Phase 94 / GUI Phase G45: `ReleaseVerify.checks`・`ReleaseItem.gate` を追加し、欠けていれば
空配列/`None` になることをテストで確認。`docs/celeris-api-v1.md` の `GET /releases` 節に追記
（既存の記述は書き換えない）。GUI は `releaseVerifyCheckGroups` を置き換えず、`checks`/`gate.steps` が
あるときに詳細行を追加表示する。
