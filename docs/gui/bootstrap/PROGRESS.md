# taskd-gui 進捗

設計は `docs/DESIGN.md`（§10 にフェーズと受け入れ条件）、taskd の API は `docs/taskd-api-v1.md`。各フェーズの完了時にこのファイルへ `## Phase G<N> — DONE` の節を追加する。
`run-gphases.sh` はこのファイルの `## Phase G<N> — DONE` / `BLOCKED` / `PARTIAL` を見て進む。

## 現在地

| フェーズ | 内容 | 状態 | 完了日 |
|---|---|---|---|
| G0 | 骨組みと前提の確定 | 未着手 | — |
| G1 | 読み取りとストリーム | 未着手 | — |
| G2 | 操作 | 未着手 | — |
| G3 | ログ・成果物・DAG | 未着手 | — |
| G4 | プロバイダとデーモン | 未着手 | — |
| G5 | 認証・配布・仕上げ | 未着手 | — |

前提: taskd（`$TASKD_REPO`、既定 `../agent-platform`）の Phase 9a / 9b（`docs/adr/0013`）が完了していること。G0 の受け入れ条件 2 で確認する。

## 引き継ぎ（前のフェーズから）

（なし）

## 提案（`docs/DESIGN.md` / `docs/taskd-api-v1.md` への変更提案。採否は人間）

（なし）

## taskd への依頼（`docs/taskd-requests.md` の要約）

（なし）

## 節の書式（各フェーズで使う）

```
## Phase G<N> — DONE（YYYY-MM-DD）

### 成果物
- 追加・変更したファイルと要点

### 受け入れ条件と証拠
1. **<条件>** — コマンド（または Playwright の操作）と出力の要点（exit code、テスト数、表示された文字列、差分ゼロ）
2. ...

### 共通条件
- `pnpm lint` / `pnpm typecheck` / `pnpm test`（N passed）/ `pnpm build` / `pnpm e2e`（N passed）
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ

### 監査結果
- auditor の判定と、指摘への対応

### 未解決事項
### 提案
### taskd への依頼
```
