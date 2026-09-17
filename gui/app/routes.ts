import { index, type RouteConfig, route } from "@react-router/dev/routes";

// 明示的なルート定義（docs/DESIGN.md §6.2、docs/adr/0002 D1）。fs-routes は使わない。
export default [
  index("routes/inbox.tsx"),
  route("healthz", "routes/healthz.ts"),
  route("login", "routes/login.tsx"),
  route("logout", "routes/logout.ts"),
  // SPEC §4 の画面（Phase G13a、ADR-0033 D8）。秘書・報告・認可・成果物は G13b まではプレースホルダ
  route("org/secretary", "routes/org.secretary.tsx"),
  route("org", "routes/org.tsx"),
  route("projects", "routes/projects.tsx"),
  route("projects/:id", "routes/projects.$id.tsx"),
  route("reports", "routes/reports.tsx"),
  route("approvals", "routes/approvals.tsx"),
  route("artifacts", "routes/artifacts.tsx"),
  route("tasks", "routes/tasks.tsx"),
  route("tasks/new", "routes/tasks.new.tsx"),
  route("tasks/:id", "routes/tasks.$id.tsx"),
  route("tasks/:id/runs/:runId", "routes/tasks.$id.runs.$runId.tsx"),
  route("plans/new", "routes/plans.new.tsx"),
  route("daemon", "routes/daemon.tsx"),
  route("providers", "routes/providers.tsx"),
  route("accounts", "routes/accounts.tsx"),
  route("clusters", "routes/clusters.tsx"),
  route("graph", "routes/graph.tsx"),
  route("help", "routes/help.tsx"),
  route("events", "routes/events.ts"),
  route("files/tasks/:id/runs/:runId/:name", "routes/files.runs.ts"),
  route("files/tasks/:id/artifacts/:idx", "routes/files.artifacts.ts"),
  // 未定義パスも root middleware を通す（docs/adr/0008 D15）。必ず最後に置く
  route("*", "routes/$.tsx"),
] satisfies RouteConfig;
