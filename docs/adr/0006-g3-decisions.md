# ADR-GUI-0006: Phase G3（ログ・成果物・DAG）で確定した細部

- 日付: 2026-09-15
- 状態: **Accepted**（Phase G3 の実装で確定。docs/DESIGN.md §4.2, §4.3「生ログ」「成果物」, §6.2, §6.3 の 4, §8.3, §10 Phase G3 の範囲内の細部）
- 関連: docs/DESIGN.md §4, §6, §8, §10 Phase G3 / docs/taskd-api-v1.md §3.7〜§3.9, §3.16 / docs/adr/0002, 0003, 0004, 0005

## 1. 文脈

G3 は「ログ・成果物・DAG」（light）。`GET /tasks/{id}/runs/{run_id}/{stdout,stderr,result}`・`GET /tasks/{id}/artifacts[/{idx}]`・`GET /graph` を GUI から見えるようにする。
DESIGN §6.2 のルート表に `tasks.$id.runs.$runId`（`/tasks/:id/runs/:runId`）・resource `files`（`/files/...`）・`graph`（`/graph`）が既に定義されているので、
その形に沿って実装する。新規依存は ADR-0002 D7 で選定済み（`@xyflow/react` 12.11.6、`@dagrejs/dagre` 3.1.1、`@codemirror/{view,state,lang-json,lang-markdown}`、`react-markdown` 10.1.0 + `remark-gfm` 4.0.1）。
全て公開から 7 日以上（`pnpm install` が `minimumReleaseAge` で確認済み）。

## 2. 決定

### D1. `/files/...` は 2 つの明示的な resource route（splat ではない）

- `app/routes/files.runs.ts` = `/files/tasks/:id/runs/:runId/:name`、`app/routes/files.artifacts.ts` = `/files/tasks/:id/artifacts/:idx`。
  DESIGN §6.2 の「resource `files`」の 2 パターンをそのまま 2 ルートにする。1 本の splat（`files/*`）にして自前でパスを検証する案は、
  taskd 自身が既に閉じたパス解決（docs/taskd-api-v1.md §3.8）を行っているので GUI 側の検証は二重になるうえ、`:name` が `stdout|stderr|result` 以外だと
  taskd 側がルーティングで 404 にする（axum に一致するハンドラが無い）ため、GUI 側で列挙し直す必要がない。
- どちらも `client.file(`/tasks/${id}/runs/${runId}/${name}`, {...})` / `client.file(`/tasks/${id}/artifacts/${idx}`, {...})` を呼び、応答をヘッダごと中継する
  （`Content-Type` / `Content-Disposition` / `X-Taskd-Sha256` / `X-Taskd-Sha256-Current` / `X-Taskd-Size` / `Content-Range` / `Accept-Ranges`）。
  `request.headers.get("Range")` と `?offset=` / `?length=` / `?download=` をそのまま `FileOptions` に渡す（クエリの併用検査は taskd 側に任せる。docs §3.8）。
  taskd の非 2xx（403 `path_forbidden`、404、416 等）は `events.ts` と同じ方針で `taskdErrorResponse` を**そのまま返す**（投げない。resource route に ErrorBoundary は無い）。
- `Content-Length` は taskd の応答のものをそのまま転送する（Node の `fetch` は `Content-Length` を保持するので明示コピーで足りる）。

### D2. `stdout.jsonl` の構造化表示は 1 つの純粋関数 `classifyStreamJsonLine`（`app/lib/stream-json.ts`）に集約する

- 入力は 1 行（文字列）。出力は判別共用体:
  `{kind:"utterance", text}` / `{kind:"tool", label, detail?}` / `{kind:"result", text, isError}` / `{kind:"raw", text}`（元の行をそのまま）。
- claude-code（`type:"assistant"` の `message.content[].type` が `text`/`tool_use`、`type:"result"`）と codex（`type` が `item.*` の `item.type`、`turn.completed`/`turn.failed`/`error`）を
  同じ 4 種に正規化する。JSON として不正な行、未知の `type`、fake ワーカーの行（`{"type":"progress"/"done"/"question"/"error", ...}` は taskd 独自のワーカープロトコルであり
  claude-code/codex のどちらでもない）は全て `raw` になる（DESIGN §10 Phase G3 受け入れ条件 2「fake の JSON Lines は生表示」）。
  判定ロジックは taskd の `crates/task-worker/src/{claude_code,codex}.rs` の `handle_line` を読解して型だけ揃えたもので、taskd 側の分類規則を再実装するのではなく
  **表示の見出し分けのためだけ**に使う（`Terminal` / `ResultMeta` 等の判断値は一切生成しない。CLAUDE.md「派生値の再計算」禁止に抵触しないよう、pass/fail や成否の意味づけはしない。
  `result` 種別は `is_error` をそのまま表示に反映するだけで、リトライ可否等の判断はしない）。
- fixtures: `test/fixtures/stream-json/claude-code.jsonl` / `codex.jsonl` / `fake.jsonl` に、`crates/task-worker/src/{claude_code,codex}.rs` のテストにある行をそのまま写す
  （taskd の crate に依存せず、行を手でコピーするだけなので禁止事項に抵触しない）。

### D3. ビューアは用途ごとに 3 コンポーネント、`sha256` の警告はビューアの外側

- `app/components/CodeViewer.tsx`: CodeMirror 6（`EditorView` + `EditorState`、`extensions: [EditorView.editable.of(false), lineNumbers も付けない簡素な読み取り専用]`）。
  `lang-json` は `Content-Type: application/json` のときだけ、`lang-markdown` は使わない（Markdown は D4 の `MarkdownViewer` が担当）。それ以外の text/plain は plain の CodeMirror（構文強調なし）。
  クライアント専用（`useEffect` で `new EditorView(...)` を mount し、SSR は非表示のプレースホルダ）。
- `app/components/MarkdownViewer.tsx`: `react-markdown` + `remark-gfm`（HTML パススルー無し。DESIGN §8.3 のとおり `<script>` 等はテキストとして表示される）。
- `app/components/ImageViewer.tsx`: 同一オリジンの `/files/...` を `<img src>` にそのまま渡す（png/jpeg/gif/webp のみ。taskd が返す `Content-Type` で判定）。
- 選択（CodeMirror か Markdown か `<img>` か）は `app/lib/artifact-view.ts` の純粋関数 `pickViewer(contentType, name): "code" | "markdown" | "image"` が行う。
- `sha256` 不一致の警告は `app/components/Sha256Badge.tsx`（`ArtifactView.sha256_matches === false` のときだけ「sha256 が一致しません（記録: … / 現在: …）」）。
  `sha256_matches` は taskd が計算済みの値をそのまま見せるだけで、GUI は再計算しない（`X-Taskd-Sha256-Current` をハッシュし直したりしない）。

### D4. `tasks.$id.tsx` の loader に `GET /tasks/{id}/artifacts` を追加、run の生ログは別ルート

- DESIGN §6.2 のとおり、詳細ページの loader が `artifacts` も並列取得する（`Promise.all` に 1 本足す）。成果物一覧は詳細ページに埋め込み表示し、
  各行から `/tasks/:id/runs/:runId`（生ログ）または `/files/tasks/:id/artifacts/:idx`（成果物本体。閲覧はクリックで `CodeViewer`/`MarkdownViewer`/`ImageViewer` を同じページ内に開く。
  新しいページ遷移にしないのは、成果物が複数あるときにタスク詳細との行き来を減らすため）。
- `app/routes/tasks.$id.runs.$runId.tsx`: `GET /tasks/{id}/runs` から対象 run を探して要約を出し、`stdout.jsonl`（`classifyStreamJsonLine` で整形、生テキスト切替可）・
  `stderr.log` 末尾（`?offset=` で末尾 4000 文字相当を取得する代わりに、GUI は全文を取って末尾だけ表示する。ログの全量は多くても run 1 本あたり数百 KB 想定で、
  `?offset=` の目的は追尾であって末尾取得ではないため。全文取得後にクライアント側で末尾を切り出す）・`result.json`（`CodeViewer` の JSON 表示）を出す。
  実行中の run（`finished_at` が無い）は `useEffect` で `?offset=<現在の受信バイト数>` を 1 秒ごとに叩き、返ってきた分だけ追記する（DESIGN §4.3「実行中の run は追尾」）。
  停止条件はコンポーネントの unmount、またはページの再検証で `finished_at` が付いたとき。

### D5. `/graph` はクライアント専用、SSR はプレースホルダ

- `app/routes/graph.tsx`: loader が `GET /graph`（`root`/`depth`/`include_terminal` を検索パラメータから転送）を返すだけ。描画は `@xyflow/react` の `ReactFlow` +
  `@dagrejs/dagre` の層状レイアウト（`rankdir: "LR"`）。ノードの色は `status`（`done`=緑、`failed`=赤、`cancelled`=灰、`blocked`/`ready`=黄、`running`/`reviewing`=青、`draft`=白に近い灰、の 6 分類。
  taskd の `Status` 列挙をそのまま流用し、GUI が新しい状態区分を作らない）、枠線は `kind`（`plan` は太枠）。
  dagre には `depends_on` の辺だけを渡してフラットに層状配置し（`parent_id` と `depends_on` が両方絡む dagre の `compound` レイアウトは挙動が読みにくく、
  G3（light）の範囲を超えるため使わない）、`parent_id` の親子はレイアウト後に子のバウンディングボックスから group ノードを合成する（`app/lib/graph-layout.ts`）。
  `depends_on` は `edges[]` をそのまま辺にする（`kind` は常に `"depends_on"` なので DESIGN のとおり表示ラベルは付けない）。
- SSR では `<div data-testid="graph-placeholder">読み込み中…</div>` を出し、`useEffect` 内で dagre のレイアウト計算 + `ReactFlow` の描画を行う
  （`@xyflow/react` は `window`/`ResizeObserver` に依存しクライアント専用。ADR-0002 D7 のとおり）。

### D6. fixture の追加（`test/taskd/fixtures/basic-worker.sh` / `scripts/taskd.sh`）

- 生ログ表示の受け入れ条件（Playwright 条件 1）のため、`basic-worker.sh` の `*)` 分岐はそのまま（Chain-A1 等が `stdout.jsonl` を持つのは taskd 自身が
  ワーカーの標準出力を書き出すため、GUI 側で何もしなくても既に存在する。fake ワーカーの出力は taskd 独自プロトコルなので `raw` 表示になる）。
- 追尾（受け入れ条件 7）用に `Slow-F` を追加: 2 秒おきに `progress` を 5 回出してから `done`（合計 10 秒）。
- Markdown（`<script>alert(1)</script>` を含む）・JSON・PNG の成果物を持つタスク `Artifacts-G`（`artifacts/note.md` / `artifacts/data.json` / `artifacts/image.png`）を追加。
  PNG は 1x1 の最小 PNG をシェルで `printf` する（ネットワーク取得をしない）。
- 403 `path_forbidden`（受け入れ条件 5）は実 taskd で細工しない（taskd 側のテストに任せる。CLAUDE.md/DESIGN のとおり）。`test/unit/files.route.test.ts` で
  mock-taskd に 403 を返させて確認する。

### D7. `保存`（download）はネイティブの `<a download>`

- `<a href="/files/.../artifacts/{idx}?download=1" download>`。resource route が `?download=1` を `FileOptions.download` にそのまま渡し、taskd が
  `Content-Disposition: attachment` を返す。GUI 側で Blob 生成・追加の JS は書かない。

## 3. 影響

- 新規依存: `@xyflow/react` 12.11.6、`@dagrejs/dagre` 3.1.1、`@codemirror/view` 6.43.11、`@codemirror/state` 6.7.4、`@codemirror/lang-json` 6.0.2、
  `react-markdown` 10.1.0、`remark-gfm` 4.0.1。全て ADR-0002 D7 で選定済みの版、`pnpm install` で 7 日 cooldown を通過。
  ADR-0002 D7 が挙げていた `@codemirror/lang-markdown` は入れていない（Markdown は `react-markdown` が描画し、CodeMirror 側で Markdown を表示する用途が無いため。
  PROGRESS の「提案」に ADR-0002 D7 からの差分として記載）。
- `app/routes.ts` に `tasks/:id/runs/:runId`・`files/tasks/:id/runs/:runId/:name`・`files/tasks/:id/artifacts/:idx`・`graph` を追加。
- `docs/adr/0002` D7 で「未使用」と書いた `@codemirror/lang-markdown` は実装時に不要と判明したため、次の依存整理（G4/G5）で外すか検討する（PROGRESS の提案に記載）。
