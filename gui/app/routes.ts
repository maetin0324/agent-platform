import { index, type RouteConfig, route } from "@react-router/dev/routes";

// 明示的なルート定義（docs/DESIGN.md §6.2、docs/adr/0002 D1）。fs-routes は使わない。
export default [
  // 最初の画面は Console（ADR-0048 D4、GUI Phase G22）。受信箱は裏方の `/inbox` に残す。
  index("routes/home.tsx"),
  route("inbox", "routes/inbox.tsx"),
  route("healthz", "routes/healthz.ts"),
  route("login", "routes/login.tsx"),
  route("logout", "routes/logout.ts"),
  // SPEC §4 の画面（Phase G13a、ADR-0033 D8）。
  // `/org/secretary` は旧 URL（P-59-a）。302 で `/org/cos` へ（`org/:id` と同じ Console）。
  route("org/secretary", "routes/org.secretary.tsx"),
  route("org", "routes/org.tsx"),
  // 組織の木から選んだ「人」の Console（ADR-0048 D4、Phase G13b-2 → G22）。CoS もここで受ける
  // （`id === "cos"`）。静的な org/secretary を先に置く
  route("org/:id", "routes/org.$id.tsx"),
  route("projects", "routes/projects.tsx"),
  route("projects/:id", "routes/projects.$id.tsx"),
  // 案件の文書（ADR-0044 D7、Phase 57 / G20）。`tasks/:id/files` と同じ兄弟のルート
  route("projects/:id/docs", "routes/projects.$id.docs.tsx"),
  // ボード（ADR-0044 D4、Phase 53）。案件を選んで 6 列で見る。絞り込みは URL がそのまま状態
  route("board", "routes/board.tsx"),
  // 知識ベース（ADR-0047 D5、Phase 61 / G21）。候補（`_inbox`）は兄弟のルートに分ける
  route("knowledge", "routes/knowledge.tsx"),
  route("knowledge/inbox", "routes/knowledge.inbox.tsx"),
  // skills（ADR-0056 D3 続き、Phase 82 / G35）。知識の候補と同じく兄弟のルートに分ける
  route("knowledge/skills", "routes/knowledge.skills.tsx"),
  route("reports", "routes/reports.tsx"),
  // resource route（コンポーネント無し）。`/reports` の行の展開・`sources_expanded` の追い掛けに使う
  route("reports/:id", "routes/reports.$id.tsx"),
  route("approvals", "routes/approvals.tsx"),
  route("artifacts", "routes/artifacts.tsx"),
  route("tasks", "routes/tasks.tsx"),
  route("tasks/new", "routes/tasks.new.tsx"),
  route("tasks/:id", "routes/tasks.$id.tsx"),
  // タスクの作業ツリー（ADR-0043 D6、Phase 52 / G16）。`runs/:runId` と同じ兄弟のルート
  route("tasks/:id/files", "routes/tasks.$id.files.tsx"),
  // タスクの変更の取り込み（ADR-0043 D5、Phase 54 / G18）。「変更」タブと同じ部品を出す兄弟のルート
  route("tasks/:id/changes", "routes/tasks.$id.changes.tsx"),
  route("tasks/:id/runs/:runId", "routes/tasks.$id.runs.$runId.tsx"),
  // run の全行（ADR-0048 D1、GUI Phase G22）。Console の progress ブロックの「すべて見る」が
  // 開いたときだけ取りに行く（`tasks/:id/runs/:runId` の兄弟の resource route）
  route("tasks/:id/runs/:runId/events", "routes/tasks.$id.runs.$runId.events.ts"),
  route("plans/new", "routes/plans.new.tsx"),
  route("daemon", "routes/daemon.tsx"),
  route("providers", "routes/providers.tsx"),
  route("accounts", "routes/accounts.tsx"),
  // MCP クライアントの直近の呼び出し（ADR-0056 D4、Phase 80）。「MCP クライアント」節のカードを
  // 開いたときだけ取りに行く resource route（`tasks/:id/runs/:runId/events` と同じ作り）
  route("mcp/clients/:id/calls", "routes/mcp.clients.$id.calls.ts"),
  route("clusters", "routes/clusters.tsx"),
  // リリース（自己改善のデプロイ。Phase G14、ADR-0040 D6）
  route("releases", "routes/releases.tsx"),
  route("graph", "routes/graph.tsx"),
  route("help", "routes/help.tsx"),
  route("events", "routes/events.ts"),
  // Console の SSE 中継（ADR-0048 D1、GUI Phase G22）。`~/routes/events.ts` と同じ作り
  route("console/stream", "routes/console.stream.ts"),
  route("console/new-conversation", "routes/console.new-conversation.ts"),
  route("files/tasks/:id/runs/:runId/:name", "routes/files.runs.ts"),
  route("files/tasks/:id/artifacts/:idx", "routes/files.artifacts.ts"),
  // 未定義パスも root middleware を通す（docs/adr/0008 D15）。必ず最後に置く
  route("*", "routes/$.tsx"),
] satisfies RouteConfig;
