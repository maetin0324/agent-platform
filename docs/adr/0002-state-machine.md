# ADR-0002: タスク状態機械

- 日付: 2026-09-13
- 状態: Accepted（Phase 0）。Phase 1 で `task-core::transition` として実装する
- 関連: `docs/DESIGN.md` §4.1–§4.3, §5.1, §5.7 / [ADR-0001](0001-scope-and-principles.md) / [ADR-0003](0003-worker-protocol.md)

## 文脈

DESIGN §4.2 は状態と遷移を図で示すが、実装に必要な次の点が未確定:
(a) `transition(state, event)` の「event」が何か、(b) `attempts` の数え方、(c) kind ごとの初期状態、
(d) `taskctl approve` の多義性、(e) 親 `Approval` 待ちの子の状態、(f) ワーカーの `error` 応答の扱い、
(g) `cancel` の対象。本 ADR はこれらを、DESIGN と矛盾しない範囲で確定し、矛盾する点は末尾の提案に分ける。

## 決定

### D1. 状態と終端

```
Status = draft | ready | running | blocked | reviewing | done | failed | cancelled
終端   = done | failed | cancelled
```

`lease` を持てるのは `running` だけ。`running` から出る全遷移でリースを解放する。

### D2. 遷移関数の入力は `Trigger`、出力の記録は `Event`

DESIGN の `fn transition(state, event)` の「event」は、追記ログの `Event` enum（`WorkerStarted`, `ReviewVerdict` など）とは別物として扱う。
理由: `ReviewVerdict` は受け入れ条件 1 件ごとに記録され、単体では遷移を決めない（全件 pass の集約が必要）。
`Transitioned{from,to,reason}` は遷移の**結果**であり入力ではない。

```rust
pub struct StateView { pub kind: TaskKind, pub status: Status, pub attempts: u32, pub max_retries: u32 }

pub enum Trigger {
    Accept,                       // draft → ready（人間の approve、または plan.auto_accept）
    Dispatch,                     // ready → running（リース取得成功）
    WorkerDone,                   // running → reviewing（プロトコル `done`）
    WorkerQuestion,               // running → blocked（プロトコル `question`）
    WorkerError { retryable: bool }, // running → ready | failed（プロトコル `error`、終端メッセージ無し終了、タイムアウト）
    LeaseExpired,                 // running → ready | failed
    ReviewPass,                   // reviewing → done（全 Criterion pass）
    ReviewFail,                   // reviewing → ready | failed
    Answer,                       // blocked → ready（taskctl answer）
    Approve,                      // ready → done（kind = Approval のみ）
    Reject,                       // ready → failed（kind = Approval のみ）
    Cancel,                       // 任意 → cancelled（DESIGN §4.2「any」。提案 P-4 参照）
}

pub fn transition(s: &StateView, t: &Trigger) -> Result<Outcome, InvalidTransition>;
pub struct Outcome { pub next: Status, pub attempts: u32, pub reason: &'static str }
```

- 純粋関数。I/O なし、時計なし。`StateView` に必要な最小情報だけを渡す。
- 成功した遷移は必ず `Event::Transitioned{from, to, reason}` を追記する。`reason` は `Trigger` の機械可読名（`"lease_expired"`, `"review_fail"`, `"worker_error"`, `"worker_done"` …）で、`replay` が `attempts` を再計算するときの鍵になる。
- 同一トリガに対する追加イベント（`WorkerFinished`, `ReviewVerdict`, `ApprovalDecided` …）は `Transitioned` と同一トランザクションで追記する。

### D3. `attempts` とリトライ判定

- `attempts` = **成功で終わらなかった実行の回数**。初期値 0。
- `WorkerError{retryable:true}`, `LeaseExpired`, `ReviewFail` のとき `attempts' = attempts + 1` とし、`attempts' > max_retries` なら `failed`、そうでなければ `ready`。
- `WorkerError{retryable:false}` は `attempts' = attempts + 1` の上で無条件に `failed`。
- `WorkerQuestion → blocked → Answer → ready` は `attempts` を増やさない（失敗ではない）。
- 例: `max_retries = 1` なら合計 2 回まで実行する。Phase 3 受け入れ「1 回リトライされ 2 回目で done」と一致。
- `Plan` kind は「不正なら 1 回だけ再試行」（DESIGN §5.6）を `max_retries` の既定値 1 で表現する。

### D4. kind ごとの初期状態と `taskctl approve / reject` の意味

| kind | 生成元 | 初期状態 |
|---|---|---|
| `Execute`, `Plan`, `Review` | `taskctl add`, `taskctl plan`, Planner の子タスク | `draft` |
| `Approval` | `taskctl add --kind approval`, `Human` check（Phase 6） | `ready`（承認待ち。「受理」の段階は不要） |

`taskctl approve <id>`:
- `status = draft`（kind 不問）→ `Trigger::Accept` → `ready`
- `kind = Approval` かつ `status = ready` → `Trigger::Approve` → `done`
- それ以外 → エラー（遷移しない）

`taskctl reject <id>`:
- `kind = Approval` かつ `status = ready` → `Trigger::Reject` → `failed`
- `status = draft` → `Trigger::Cancel` → `cancelled`（DESIGN は未規定。提案 P-5）
- それ以外 → エラー

### D5. 依存と承認ゲートは「状態」ではなく「ディスパッチ可否」で表す

- `depends_on` が全て `done` でないタスク、親が `Approval` で `done` でないタスクは、`status = ready` のままで **`ready_tasks()` に現れない**（DESIGN §5.1 の定義に従う）。
- 状態機械には「依存待ち」の状態を追加しない。理由: 依存の解決はストアのクエリで決定的に判定でき、状態を増やすとイベント数と遷移表が膨らむ。
- 親 `Approval` が `failed`（reject）になったときの子の `cancelled` 化は Phase 6 で `Trigger::Cancel` を使って実装する。
- DESIGN §4.2 の「子タスクは親の Approval が done になるまで ready にならない」は、上の解釈では「ready にはなるが dispatch されない」となる。文言の差を提案 P-6 に記す。

### D6. `Plan` kind の経路

`draft → ready → running → reviewing` まで他 kind と同じ。`reviewing` で行う「レビュー」は Planner 出力の JSON Schema 検証（決定的）。
pass なら子タスクを `draft` で挿入してから `ReviewPass → done`。fail なら `ReviewFail`（D3 のリトライ判定）。
子の `draft → ready` は `plan.auto_accept` が true なら親 `done` と同一トランザクションで `Accept`、false なら人間の `taskctl approve`。

### D7. リースの期限

`acquire_lease(task_id, run_id, ttl)` の `ttl` は **`budget.max_wall_secs + 猶予（設定、既定 60s）`** とする。
ワーカーは `max_wall_secs` でアダプタに強制終了されるので、生きているワーカーのリースが期限切れになることはない。
`renew_lease` は追加しない（提案 P-7 で代替案を記す）。

### D8. 遷移表

行 = 現在の `status`、列 = `Trigger`。`✗` = `InvalidTransition`。`R|F` = D3 のリトライ判定で `ready` または `failed`。
`(A)` = `kind = Approval` のときだけ有効、`(¬A)` = `Approval` 以外だけ有効。

| status \ trigger | Accept | Dispatch | WorkerDone | WorkerQuestion | WorkerError | LeaseExpired | ReviewPass | ReviewFail | Answer | Approve | Reject | Cancel |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| draft | ready | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | cancelled |
| ready | ✗ | running (¬A) | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | done (A) | failed (A) | cancelled |
| running | ✗ | ✗ | reviewing | blocked | R\|F | R\|F | ✗ | ✗ | ✗ | ✗ | ✗ | cancelled |
| blocked | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ready | ✗ | ✗ | cancelled |
| reviewing | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | done | R\|F | ✗ | ✗ | ✗ | cancelled |
| done | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | cancelled (※P-4) |
| failed | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | cancelled (※P-4) |
| cancelled | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | cancelled (※P-4) |

`Approval` kind の `Dispatch` は `✗`（`running` に入らない。DESIGN §4.2）。
`Approval` 以外の `Approve`/`Reject` は `✗`。

Phase 1 のテストは `kind (4) × status (8) × trigger (12, WorkerError は retryable 2 通り) × 予算 (attempts < max_retries / = max_retries)` を全て列挙し、この表と D3 の期待値を照合する。

### D9. `running` から `cancelled` へのとき

状態機械は遷移を許すだけ。実行中ワーカーの強制終了はディスパッチャがアダプタに指示する（ADR-0003 D4）。終了を待たずに `cancelled` に遷移してよい（ワーカーの後続メッセージは run_id 不一致で捨てる）。

## 結果

- Phase 1 は本 ADR の D2/D3/D8 をそのまま `task-core` に実装する。
- Phase 2 の `replay` は `Transitioned.to` で `status` を、`reason ∈ {worker_error, lease_expired, review_fail}` の件数で `attempts` を復元する。
- 提案 P-4〜P-7 の採否が出るまでは、表の「※P-4」セルは DESIGN の文言どおり（`any → cancelled`）に実装する。

## DESIGN.md 修正提案（本 ADR のスコープ分）

- **P-4（§4.2 cancel）** 「any ──(cancel)──▶ cancelled」を「非終端状態のみ」に改める。終端（`done/failed/cancelled`）からの `cancelled` は履歴を壊し、`replay` の検証も不自然になる。採用されるまでは文言どおり実装する。
- **P-5（§5.9 reject）** `taskctl reject` の対象が `Approval` 以外（`draft` タスク）の場合の動作が未規定。`draft → cancelled` とすることを提案。
- **P-6（§4.2 と §5.1 の不整合）** §4.2「子は親 Approval が done になるまで `ready` にならない」と §5.1「`ready_tasks` = `status=ready` かつ親 Approval 充足」が食い違う。§5.1 の解釈（`ready` だが dispatch されない）に統一し、Phase 6 の受け入れ「承認前に子が `ready` にならない」を「承認前に子が `ready_tasks()` に現れない／dispatch されない」に改めることを提案。
- **P-7（§5.1 リース）** `renew_lease` が無いため ttl を `max_wall_secs + 猶予` に固定した（D7）。長時間タスクで早期回収したい場合に備え、`progress` 受信ごとにリースを延長する `renew_lease(task_id, run_id, ttl)` の追加を提案。採用されるまで D7 で実装する。
- **P-8（§4.2 worker error）** 図に「running ──(worker error)──▶ ready | failed」の行がない。プロトコルの `error{retryable}` と終端メッセージ無しの終了は本 ADR D2/D3 のとおり扱う。図への追記を提案。
- **P-9（§5.7 依存先の失敗）** `depends_on` の先行タスクが `failed`/`cancelled` になった場合の後続の扱いが未規定。「後続を `cancelled` にする（reason = `dependency_failed`）」を提案。採用されるまでは何もしない（`ready` のまま `taskctl ls` に残る）。
