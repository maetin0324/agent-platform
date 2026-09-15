import { randomBytes } from "node:crypto";
import type { MiddlewareFunction } from "react-router";
import { getGuiConfig, hostWithoutPort } from "~/config.server";
import { nonceContext } from "~/context";

/**
 * ブラウザ ↔ taskd-gui の境界（docs/DESIGN.md §8.2）。
 * 1. `Host` を許可リストで検査（DNS rebinding 対策）。外れれば 400 で止める（loader は走らない）
 * 2. 要求ごとに CSP の nonce を作り context に置く
 * 3. 応答に CSP とその他のセキュリティヘッダを付ける
 */
export const hostCheck: MiddlewareFunction<Response> = ({ request }) => {
  const host = request.headers.get("host");
  if (!host || !getGuiConfig().allowedHosts.has(hostWithoutPort(host))) {
    throw new Response("host not allowed", { status: 400, headers: { "Content-Type": "text/plain; charset=utf-8" } });
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
