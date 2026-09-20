import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { readFileSync } from "node:fs";
import { isIP } from "node:net";
import { createContext, createCookie, type MiddlewareFunction, redirect } from "react-router";
import { hostWithoutPort } from "~/config.server";

/**
 * ブラウザ ↔ celeris-gui のパスワード認証とセッションクッキー（docs/DESIGN.md §8.2、docs/adr/0008 D1〜D5）。
 * - 認証が有効になるのは、`CELERIS_GUI_BIND` が非 loopback のとき（`CELERIS_GUI_PASSWORD_FILE` 必須）か、
 *   `CELERIS_GUI_PASSWORD_FILE` が明示されているとき（loopback でも opt-in）。既定の loopback は認証無し
 * - パスワードは SHA-256 ダイジェスト同士を `timingSafeEqual` で比較（定数時間）。失敗は 1 秒待つ
 * - セッションは HMAC 署名付きクッキー（react-router の `createCookie`）。サーバ側にセッション表は持たない
 * - 署名鍵は `CELERIS_GUI_SESSION_SECRET_FILE`、無ければプロセスごとの乱数（再起動でログアウト）
 */

export const SESSION_COOKIE_NAME = "__celeris_gui_session";
export const SESSION_MAX_AGE_SECONDS = 24 * 60 * 60;
/** パスワード失敗時に待つ時間（総当たりの抑止。docs/DESIGN.md §8.2） */
export const FAILED_LOGIN_DELAY_MS = 1_000;

export interface AuthConfig {
  /** 認証を要求するか */
  enabled: boolean;
  /** バインド先が非 loopback か */
  nonLoopback: boolean;
  /** パスワードの SHA-256（hex）。enabled のときだけ */
  passwordDigest: Buffer | null;
  /** クッキーの署名鍵 */
  secret: string;
  /** 鍵がファイル由来か（false ならプロセスごとの乱数） */
  secretFromFile: boolean;
}

export class AuthConfigError extends Error {
  override readonly name = "AuthConfigError";
}

export function isLoopbackHost(host: string): boolean {
  const h = host.toLowerCase().replace(/^\[|\]$/g, "");
  if (h === "localhost") return true;
  const v = isIP(h);
  if (v === 4) return h.startsWith("127.");
  if (v === 6) return h === "::1" || h === "::ffff:127.0.0.1";
  return false;
}

function digest(s: string): Buffer {
  return createHash("sha256").update(s, "utf8").digest();
}

function readTrimmedFile(path: string, what: string): string {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    throw new AuthConfigError(`cannot read ${what} (${path}): ${e instanceof Error ? e.message : String(e)}`);
  }
  const v = text.trim();
  if (!v) throw new AuthConfigError(`${what} (${path}) is empty`);
  return v;
}

/**
 * 環境変数から認証設定を読む。`server.js` の起動時検証と同じ規則（非 loopback でパスワードファイルが無ければ例外）。
 */
export function readAuthConfig(env: NodeJS.ProcessEnv = process.env): AuthConfig {
  const bindHost = hostWithoutPort(env.CELERIS_GUI_BIND ?? "127.0.0.1:7700");
  const nonLoopback = !isLoopbackHost(bindHost);
  const passwordFile = env.CELERIS_GUI_PASSWORD_FILE;
  if (nonLoopback && !passwordFile) {
    throw new AuthConfigError(
      `CELERIS_GUI_BIND=${bindHost} is not a loopback address; CELERIS_GUI_PASSWORD_FILE is required for non-loopback bind`,
    );
  }
  const passwordDigest = passwordFile ? digest(readTrimmedFile(passwordFile, "CELERIS_GUI_PASSWORD_FILE")) : null;
  const secretFile = env.CELERIS_GUI_SESSION_SECRET_FILE;
  const secret = secretFile
    ? readTrimmedFile(secretFile, "CELERIS_GUI_SESSION_SECRET_FILE")
    : randomBytes(32).toString("base64");
  return { enabled: passwordDigest !== null, nonLoopback, passwordDigest, secret, secretFromFile: !!secretFile };
}

let cached: AuthConfig | undefined;
export function getAuthConfig(): AuthConfig {
  cached ??= readAuthConfig();
  return cached;
}
/** テスト用: キャッシュを差し替える（`undefined` で環境変数から読み直す）。 */
export function setAuthConfigForTest(config: AuthConfig | undefined): void {
  cached = config;
}

/** 定数時間のパスワード検証。 */
export function verifyPassword(config: AuthConfig, candidate: string): boolean {
  if (!config.passwordDigest) return false;
  return timingSafeEqual(config.passwordDigest, digest(candidate.trim()));
}

interface SessionPayload {
  iat: number;
  id: string;
}

function sessionCookie(config: AuthConfig, secure: boolean) {
  return createCookie(SESSION_COOKIE_NAME, {
    secrets: [config.secret],
    httpOnly: true,
    sameSite: "strict",
    path: "/",
    secure,
    maxAge: SESSION_MAX_AGE_SECONDS,
  });
}

function isSecureRequest(request: Request): boolean {
  return new URL(request.url).protocol === "https:";
}

/** ログイン成功時の `Set-Cookie` 値。 */
export async function issueSessionCookie(config: AuthConfig, request: Request, now = Date.now()): Promise<string> {
  const payload: SessionPayload = { iat: now, id: randomBytes(16).toString("hex") };
  return sessionCookie(config, isSecureRequest(request)).serialize(payload);
}

/** ログアウト時（クッキー削除）の `Set-Cookie` 値。 */
export async function clearSessionCookie(config: AuthConfig, request: Request): Promise<string> {
  return sessionCookie(config, isSecureRequest(request)).serialize("", { maxAge: 0, expires: new Date(0) });
}

/** 要求の `Cookie` から有効なセッションがあるか（署名が正しく、`iat` が `maxAge` 以内）。 */
export async function hasValidSession(config: AuthConfig, request: Request, now = Date.now()): Promise<boolean> {
  const header = request.headers.get("cookie");
  if (!header) return false;
  let parsed: unknown;
  try {
    parsed = await sessionCookie(config, isSecureRequest(request)).parse(header);
  } catch {
    return false;
  }
  if (!parsed || typeof parsed !== "object") return false;
  const { iat, id } = parsed as Partial<SessionPayload>;
  if (typeof iat !== "number" || typeof id !== "string" || !id) return false;
  const ageMs = now - iat;
  return ageMs >= 0 && ageMs <= SESSION_MAX_AGE_SECONDS * 1000;
}

export interface SessionState {
  /** 認証が有効か（false なら loopback で認証無し） */
  enabled: boolean;
  /** この要求が認証済みか（enabled=false なら常に true） */
  authenticated: boolean;
}

export const sessionContext = createContext<SessionState>({ enabled: false, authenticated: true });

/** 認証を要求しないパス。 */
const PUBLIC_PATHS = new Set(["/login", "/logout", "/healthz"]);
/** 未認証で 302 ではなく 401 を返す resource route（EventSource / <a download> / <img> から呼ばれる）。 */
const RESOURCE_PREFIXES = ["/events", "/files/"];

/** `next` として受け付けるのは同一オリジンの絶対パスだけ（`//evil` のようなスキーム相対 URL は拒否）。 */
export function safeNextPath(next: string | null | undefined): string {
  if (!next?.startsWith("/") || next.startsWith("//") || next.startsWith("/\\")) return "/";
  if (next === "/login" || next.startsWith("/login?")) return "/";
  return next;
}

/** `.data` サフィックス（single fetch）を外したパス。 */
function routePath(url: URL): string {
  return url.pathname.endsWith(".data") ? url.pathname.slice(0, -".data".length) || "/" : url.pathname;
}

/**
 * root middleware（docs/adr/0008 D1）。Host 検査の後、CSRF 検査の前に置く。
 * - 認証が無効なら context に `{ enabled: false, authenticated: true }` を置くだけ
 * - `/login` `/logout` `/healthz` は通す（context は未認証のまま）
 * - `/events` `/files/*` は 401、それ以外は 302 `/login?next=<path>`
 */
export const authCheck: MiddlewareFunction<Response> = async ({ request, context }) => {
  const config = getAuthConfig();
  if (!config.enabled) {
    context.set(sessionContext, { enabled: false, authenticated: true });
    return;
  }
  const authenticated = await hasValidSession(config, request);
  context.set(sessionContext, { enabled: true, authenticated });
  if (authenticated) return;
  const url = new URL(request.url);
  const path = routePath(url);
  if (PUBLIC_PATHS.has(path)) return;
  if (RESOURCE_PREFIXES.some((p) => path === p || path.startsWith(p))) {
    throw new Response("unauthorized", {
      status: 401,
      headers: {
        "Content-Type": "text/plain; charset=utf-8",
        "WWW-Authenticate": "Cookie",
        "Cache-Control": "no-store",
        "X-Content-Type-Options": "nosniff",
      },
    });
  }
  const next = `${path}${url.search}`;
  throw redirect(`/login${next === "/" ? "" : `?next=${encodeURIComponent(next)}`}`);
};

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
