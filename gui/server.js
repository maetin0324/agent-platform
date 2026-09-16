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
const { readFileSync } = await import("node:fs");
/** ファイルを読んで trim した 1 行を返す。読めない・空なら exit 2。 @param {string} path @param {string} what */
function readSecretFile(path, what) {
  let text;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    fatal(`cannot read ${what} (${path}): ${e instanceof Error ? e.message : String(e)}`);
  }
  if (!text.trim()) fatal(`${what} (${path}) is empty`);
}
// 非 loopback へ bind するときはパスワード認証が必須（docs/DESIGN.md §8.2、docs/adr/0008 D1/D2）。
// パスワードファイルが明示されていれば loopback でも認証を要求する。値はここでは保持しない（app/auth.server.ts が読む）。
const passwordFile = process.env.TASKD_GUI_PASSWORD_FILE;
if (!isLoopback(bind.host) && !passwordFile) {
  fatal(
    `TASKD_GUI_BIND=${bind.host}:${bind.port} is not a loopback address; TASKD_GUI_PASSWORD_FILE is required for non-loopback bind`,
  );
}
if (passwordFile) readSecretFile(passwordFile, "TASKD_GUI_PASSWORD_FILE");
if (process.env.TASKD_GUI_SESSION_SECRET_FILE) {
  readSecretFile(process.env.TASKD_GUI_SESSION_SECRET_FILE, "TASKD_GUI_SESSION_SECRET_FILE");
}
const taskdApiUrl = process.env.TASKD_API_URL ?? "http://127.0.0.1:7710";
try {
  new URL(taskdApiUrl);
} catch {
  fatal(`TASKD_API_URL is not a URL: ${taskdApiUrl}`);
}
if (process.env.TASKD_API_TOKEN_FILE) readSecretFile(process.env.TASKD_API_TOKEN_FILE, "TASKD_API_TOKEN_FILE");

const app = express();
app.disable("x-powered-by");
app.set("trust proxy", false);

// Express 層の Host 検査と既定ヘッダ（docs/DESIGN.md §8.2、docs/adr/0008 D15）。React Router の root middleware は
// 一致したルートにしか効かないので、静的アセット（`/assets`, `build/client`）とアダプタ層の応答にもここで掛ける。
// 許可リストは app/config.server.ts と同じ規則（loopback 名 + バインドのホスト + TASKD_GUI_ALLOWED_HOSTS）。
/** @param {string} h */
const hostWithoutPort = (h) => {
  const v = h.trim().toLowerCase();
  if (v.startsWith("[")) return v.slice(0, v.indexOf("]") + 1 || undefined);
  const c = v.lastIndexOf(":");
  return c === -1 ? v : v.slice(0, c);
};
const allowedHosts = new Set(["localhost", "127.0.0.1", "[::1]", "::1", bind.host.toLowerCase()]);
for (const h of (process.env.TASKD_GUI_ALLOWED_HOSTS ?? "").split(",")) {
  const v = hostWithoutPort(h);
  if (v) allowedHosts.add(v);
}
/**
 * 既定ヘッダは「応答がそのヘッダを持たないとき」だけ、ヘッダ送出の直前（`writeHead`）に付ける。React Router のアダプタは
 * 応答ヘッダを `appendHeader` で足すので、先に `setHeader` しておくと CSP が 2 本になり（複数 CSP は交差 = 最も厳しい方）画面が壊れる。
 * HTML は root middleware の nonce 付き CSP がそのまま残り、静的アセット・302 / 401 にはこの既定が付く。
 */
const DEFAULT_HEADERS = [
  ["Content-Security-Policy", "default-src 'none'; frame-ancestors 'none'; base-uri 'none'"],
  ["X-Content-Type-Options", "nosniff"],
  ["Referrer-Policy", "no-referrer"],
];
app.use((req, res, next) => {
  /** @type {(...args: any[]) => any} */
  const writeHead = res.writeHead.bind(res);
  res.writeHead = (...args) => {
    for (const [name, value] of DEFAULT_HEADERS) if (!res.hasHeader(name)) res.setHeader(name, value);
    return writeHead(...args);
  };
  const host = req.headers.host;
  if (typeof host !== "string" || !allowedHosts.has(hostWithoutPort(host))) {
    res.status(400).type("text/plain; charset=utf-8").send("host not allowed");
    return;
  }
  next();
});

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
  process.stderr.write(
    `taskd-gui: listening on http://${bind.host}:${bind.port} (taskd API ${taskdApiUrl}, auth ${passwordFile ? "password" : "none (loopback)"}, token ${process.env.TASKD_API_TOKEN_FILE ? "yes" : "no"})\n`,
  );
});
for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    server.close(() => process.exit(0));
    setTimeout(() => process.exit(0), 2000).unref();
  });
}
