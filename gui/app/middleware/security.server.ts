import { randomBytes } from "node:crypto";
import type { MiddlewareFunction } from "react-router";
import { getGuiConfig, hostWithoutPort } from "~/config.server";
import { nonceContext } from "~/context";

/**
 * ブラウザ ↔ celeris-gui の境界（docs/DESIGN.md §8.2）。
 * 1. `Host` を許可リストで検査（DNS rebinding 対策）。外れれば 400 で止める（loader は走らない）
 * 2. 変更系の要求を CSRF 検査（`Origin` / `Sec-Fetch-Site`）。違えば 403（action は走らない）
 * 3. 要求ごとに CSP の nonce を作り context に置く
 * 4. 応答に CSP とその他のセキュリティヘッダを付ける
 */
export const hostCheck: MiddlewareFunction<Response> = ({ request }) => {
  const host = request.headers.get("host");
  if (!host || !getGuiConfig().allowedHosts.has(hostWithoutPort(host))) {
    throw new Response("host not allowed", { status: 400, headers: { "Content-Type": "text/plain; charset=utf-8" } });
  }
};

const SAFE_METHODS = new Set(["GET", "HEAD", "OPTIONS"]);

/**
 * CSRF 検査（docs/DESIGN.md §8.2、docs/adr/0005 D1）。変更系（GET / HEAD / OPTIONS 以外）の要求について、
 * - `Origin` があれば自分のオリジン（`request.url` の origin = 受けた `Host` から組み立てたもの）と一致すること
 * - `Sec-Fetch-Site` があれば `same-origin` / `none` であること
 * を要求する。違反なら理由の文字列を返す（純粋関数。単体テスト用）。どちらのヘッダも無い要求（curl 等）は通す
 * （celeris 自身も `Origin` 付きの POST を 403 にするので、ブラウザ経由の偽装は二重に止まる）。
 */
export function csrfViolation(request: Request): string | null {
  if (SAFE_METHODS.has(request.method.toUpperCase())) return null;
  const origin = request.headers.get("origin");
  if (origin !== null) {
    const self = new URL(request.url).origin;
    if (origin.trim().toLowerCase() !== self.toLowerCase()) return `origin ${origin} is not ${self}`;
  }
  const site = request.headers.get("sec-fetch-site");
  if (site !== null) {
    const v = site.trim().toLowerCase();
    if (v !== "same-origin" && v !== "none") return `sec-fetch-site ${site}`;
  }
  return null;
}

/**
 * Express 層の CSRF 検査（docs/adr/0005 D1）。React Router 8 は document request の変更系に対して独自の Origin 検査を
 * root middleware より**前**に行い、不一致なら 400 `Bad Request` を返す（`throwIfPotentialCSRFAttack`）。docs/DESIGN.md §8.2 /
 * §10 Phase G2 の受け入れ条件 7 は 403 を要求するので、React Router に渡す前にここで同じ `csrfViolation` を評価して 403 にする。
 * `.data` request（クライアント遷移後のフォーム送信）には React Router の検査が無いので、root middleware の `csrfCheck` も残す。
 */
export function expressCsrfGuard(
  req: {
    method: string;
    protocol: string;
    originalUrl: string;
    headers: Record<string, string | string[] | undefined>;
  },
  res: { status(code: number): { type(t: string): { send(body: string): unknown } } },
  next: () => void,
): void {
  if (SAFE_METHODS.has(req.method.toUpperCase())) {
    next();
    return;
  }
  const headers = new Headers();
  for (const [name, value] of Object.entries(req.headers)) {
    if (typeof value === "string") headers.set(name, value);
  }
  const host = typeof req.headers.host === "string" ? req.headers.host : "localhost";
  let violation: string | null;
  try {
    violation = csrfViolation(
      new Request(`${req.protocol}://${host}${req.originalUrl}`, { method: req.method, headers }),
    );
  } catch {
    // Host が URL として不正など。React Router 側（Host 検査 / 400）に任せる
    next();
    return;
  }
  if (violation) {
    res.status(403).type("text/plain; charset=utf-8").send(`forbidden: ${violation}`);
    return;
  }
  next();
}

/** 変更系の要求を CSRF 検査で止める（403）。action は走らない。 */
export const csrfCheck: MiddlewareFunction<Response> = ({ request }) => {
  const violation = csrfViolation(request);
  if (violation) {
    throw new Response(`forbidden: ${violation}`, {
      status: 403,
      headers: { "Content-Type": "text/plain; charset=utf-8" },
    });
  }
};

export function buildCsp(nonce: string, dev: boolean): string {
  // 開発時（Vite の React Refresh が nonce 無しの inline script を入れる）だけ 'unsafe-inline' を許す。
  const script = dev ? "'self' 'unsafe-inline'" : `'self' 'nonce-${nonce}'`;
  return [
    "default-src 'self'",
    `script-src ${script}`,
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data:",
    "connect-src 'self'",
    "frame-ancestors 'none'",
    "base-uri 'none'",
    "form-action 'self'",
  ].join("; ");
}

export const securityHeaders: MiddlewareFunction<Response> = async ({ context }, next) => {
  const nonce = randomBytes(16).toString("base64");
  context.set(nonceContext, nonce);
  const response = await next();
  const h = response.headers;
  h.set("Content-Security-Policy", buildCsp(nonce, import.meta.env.DEV));
  h.set("X-Content-Type-Options", "nosniff");
  h.set("Referrer-Policy", "no-referrer");
  h.set("X-Frame-Options", "DENY");
  if (!h.has("Cache-Control")) h.set("Cache-Control", "no-store");
  return response;
};
