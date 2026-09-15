// taskd-gui の Node サーバ（docs/DESIGN.md §9、docs/adr/0002 D5）。
// 環境変数を検証 → build/client を配信（/assets は immutable）→ React Router のハンドラ。失敗は exit 2。
// ログは stderr に JSON 1 行 / 要求（パス・status・所要。本文・トークンは出さない）。
import { isIP } from "node:net";
import express from "express";

const BUILD_PATH = "./build/server/index.js";
const DEVELOPMENT = process.env.NODE_ENV === "development";

/** @param {string} msg @returns {never} */
function fatal(msg) {
  process.stderr.write(`taskd-gui: ${msg}\n`);
  process.exit(2);
}

/** `host:port` / `[v6]:port` を分ける。 @param {string} bind */
function parseBind(bind) {
  const m = /^(?:\[([^\]]+)\]|([^:]+)):(\d{1,5})$/.exec(bind.trim());
  if (!m) fatal(`TASKD_GUI_BIND must be host:port (got "${bind}")`);
  const host = m[1] ?? m[2];
  const port = Number(m[3]);
  if (!(port > 0 && port < 65536)) fatal(`TASKD_GUI_BIND has an invalid port: ${bind}`);
  return { host, port };
}

/** @param {string} host */
function isLoopback(host) {
  if (host === "localhost") return true;
  const v = isIP(host);
  if (v === 4) return host.startsWith("127.");
  if (v === 6) return host === "::1" || host.toLowerCase() === "::ffff:127.0.0.1";
  return false;
}

const bind = parseBind(process.env.TASKD_GUI_BIND ?? "127.0.0.1:7700");
if (!isLoopback(bind.host)) {
  // 非 loopback のパスワード認証は Phase G5。それまでは loopback 以外への bind を拒否する。
  fatal(
    `TASKD_GUI_BIND=${bind.host}:${bind.port} is not a loopback address; non-loopback bind requires TASKD_GUI_PASSWORD_FILE (Phase G5)`,
  );
}
const taskdApiUrl = process.env.TASKD_API_URL ?? "http://127.0.0.1:7710";
try {
  new URL(taskdApiUrl);
} catch {
  fatal(`TASKD_API_URL is not a URL: ${taskdApiUrl}`);
}
if (process.env.TASKD_API_TOKEN_FILE) {
  const { readFileSync } = await import("node:fs");
  try {
    if (!readFileSync(process.env.TASKD_API_TOKEN_FILE, "utf8").trim()) fatal("TASKD_API_TOKEN_FILE is empty");
  } catch (e) {
    fatal(`cannot read TASKD_API_TOKEN_FILE: ${e instanceof Error ? e.message : String(e)}`);
  }
}

const app = express();
app.disable("x-powered-by");
app.set("trust proxy", false);

// 要求ログ（stderr、JSON 1 行）。クエリ・本文・ヘッダは出さない。
app.use((req, res, next) => {
  const start = process.hrtime.bigint();
  res.on("finish", () => {
    const ms = Number(process.hrtime.bigint() - start) / 1e6;
    process.stderr.write(
      `${JSON.stringify({ ts: new Date().toISOString(), method: req.method, path: req.originalUrl.split("?")[0], status: res.statusCode, ms: Math.round(ms * 10) / 10 })}\n`,
    );
  });
  next();
});

if (DEVELOPMENT) {
  const viteDevServer = await import("vite").then((vite) => vite.createServer({ server: { middlewareMode: true } }));
  app.use(viteDevServer.middlewares);
  app.use(async (req, res, next) => {
    try {
      const source = await viteDevServer.ssrLoadModule("./server/app.ts");
      return await source.app(req, res, next);
    } catch (error) {
      if (typeof error === "object" && error instanceof Error) viteDevServer.ssrFixStacktrace(error);
      next(error);
    }
  });
} else {
  app.use("/assets", express.static("build/client/assets", { immutable: true, maxAge: "1y" }));
  app.use(express.static("build/client", { maxAge: "1h" }));
  app.use(await import(BUILD_PATH).then((mod) => mod.app));
}

const server = app.listen(bind.port, bind.host, () => {
  process.stderr.write(`taskd-gui: listening on http://${bind.host}:${bind.port} (taskd API ${taskdApiUrl})\n`);
});
for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(0), 2000).unref();
  });
}
