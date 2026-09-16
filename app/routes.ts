import { index, type RouteConfig, route } from "@react-router/dev/routes";

// 明示的なルート定義（docs/DESIGN.md §6.2、docs/adr/0002 D1）。fs-routes は使わない。
export default [
  index("routes/inbox.tsx"),
  route("healthz", "routes/healthz.ts"),
  route("login", "routes/login.tsx"),
  route("logout", "routes/logout.ts"),
  route("tasks", "routes/tasks.tsx"),
  route("tasks/new", "routes/tasks.new.tsx"),
  route("tasks/:id", "routes/tasks.$id.tsx"),
  route("tasks/:id/runs/:runId", "routes/tasks.$id.runs.$runId.tsx"),
  route("plans/new", "routes/plans.new.tsx"),
  route("daemon", "routes/daemon.tsx"),
  route("providers", "routes/providers.tsx"),
  route("graph", "routes/graph.tsx"),
  route("help", "routes/help.tsx"),
  route("events", "routes/events.ts"),
  route("files/tasks/:id/runs/:runId/:name", "routes/files.runs.ts"),
  route("files/tasks/:id/artifacts/:idx", "routes/files.artifacts.ts"),
  // 未定義パスも root middleware を通す（docs/adr/0008 D15）。必ず最後に置く
  route("*", "routes/$.tsx"),
] satisfies RouteConfig;
