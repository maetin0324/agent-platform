import { index, type RouteConfig, route } from "@react-router/dev/routes";

// 明示的なルート定義（docs/DESIGN.md §6.2、docs/adr/0002 D1）。fs-routes は使わない。
export default [index("routes/inbox.tsx"), route("healthz", "routes/healthz.ts")] satisfies RouteConfig;
