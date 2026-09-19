import { index, type RouteConfig, route } from "@react-router/dev/routes";

// 明示的なルート定義（docs/DESIGN.md §6.2、docs/adr/0002 D1）。fs-routes は使わない。
export default [
  // 最初の画面は秘書（Phase G13f-1、監査 2）。受信箱は裏方の `/inbox` に残す。
  index("routes/home.tsx"),
  route("inbox", "routes/inbox.tsx"),
  route("healthz", "routes/healthz.ts"),
  route("login", "routes/login.tsx"),
  route("logout", "routes/logout.ts"),
  // SPEC §4 の画面（Phase G13a、ADR-0033 D8）。秘書・報告・認可・成果物は G13b まではプレースホルダ
  route("org/secretary", "routes/org.secretary.tsx"),
  route("org", "routes/org.tsx"),
  // 組織の木から選んだ「人」との対話（SPEC §3.4、Phase G13b-2）。静的な org/secretary を先に置く
  route("org/:id", "routes/org.$id.tsx"),
  route("projects", "routes/projects.tsx"),
  route("projects/:id", "routes/projects.$id.tsx"),
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
  // タスクの変更の取り込み（ADR-0043 D5、Phase 54 / G17）。同じく兄弟のルート
  route("tasks/:id/changes", "routes/tasks.$id.changes.tsx"),
  route("tasks/:id/runs/:runId", "routes/tasks.$id.runs.$runId.tsx"),
  route("plans/new", "routes/plans.new.tsx"),
  route("daemon", "routes/daemon.tsx"),
  route("providers", "routes/providers.tsx"),
  route("accounts", "routes/accounts.tsx"),
  route("clusters", "routes/clusters.tsx"),
  // リリース（自己改善のデプロイ。Phase G14、ADR-0040 D6）
  route("releases", "routes/releases.tsx"),
  route("graph", "routes/graph.tsx"),
  route("help", "routes/help.tsx"),
  route("events", "routes/events.ts"),
  route("files/tasks/:id/runs/:runId/:name", "routes/files.runs.ts"),
  route("files/tasks/:id/artifacts/:idx", "routes/files.artifacts.ts"),
  // 未定義パスも root middleware を通す（docs/adr/0008 D15）。必ず最後に置く
  route("*", "routes/$.tsx"),
] satisfies RouteConfig;
