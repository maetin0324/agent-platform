import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { RouterContextProvider } from "react-router";
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import {
  type AuthConfig,
  AuthConfigError,
  authCheck,
  clearSessionCookie,
  hasValidSession,
  isLoopbackHost,
  issueSessionCookie,
  readAuthConfig,
  SESSION_MAX_AGE_SECONDS,
  safeNextPath,
  sessionContext,
  setAuthConfigForTest,
  verifyPassword,
} from "~/auth.server";

// docs/DESIGN.md §8.2（非 loopback のパスワード認証とセッションクッキー）、docs/adr/0008 D1〜D4。

let dir: string;
let passwordFile: string;
let secretFile: string;

beforeAll(() => {
  dir = mkdtempSync(path.join(tmpdir(), "taskd-gui-auth-"));
  passwordFile = path.join(dir, "password");
  secretFile = path.join(dir, "secret");
  writeFileSync(passwordFile, "  hunter2\n");
  writeFileSync(secretFile, "0123456789abcdef0123456789abcdef\n");
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

afterEach(() => {
  setAuthConfigForTest(undefined);
});

function cfg(overrides: Partial<AuthConfig> = {}): AuthConfig {
  return {
    ...readAuthConfig({ TASKD_GUI_PASSWORD_FILE: passwordFile, TASKD_GUI_SESSION_SECRET_FILE: secretFile }),
    ...overrides,
  };
}

describe("readAuthConfig", () => {
  it("is disabled on the default loopback bind without a password file", () => {
    const c = readAuthConfig({});
    expect(c.enabled).toBe(false);
    expect(c.nonLoopback).toBe(false);
    expect(c.secretFromFile).toBe(false);
  });

  it("rejects a non-loopback bind without TASKD_GUI_PASSWORD_FILE", () => {
    expect(() => readAuthConfig({ TASKD_GUI_BIND: "0.0.0.0:7700" })).toThrow(AuthConfigError);
    expect(() => readAuthConfig({ TASKD_GUI_BIND: "0.0.0.0:7700" })).toThrow(/TASKD_GUI_PASSWORD_FILE is required/);
  });

  it("enables auth on a non-loopback bind with a password file, and on loopback when the file is explicit", () => {
    expect(readAuthConfig({ TASKD_GUI_BIND: "0.0.0.0:7700", TASKD_GUI_PASSWORD_FILE: passwordFile }).enabled).toBe(
      true,
    );
    expect(readAuthConfig({ TASKD_GUI_PASSWORD_FILE: passwordFile }).enabled).toBe(true);
  });

  it("rejects an unreadable or empty password file", () => {
    expect(() => readAuthConfig({ TASKD_GUI_PASSWORD_FILE: path.join(dir, "missing") })).toThrow(/cannot read/);
    const empty = path.join(dir, "empty");
    writeFileSync(empty, " \n");
    expect(() => readAuthConfig({ TASKD_GUI_PASSWORD_FILE: empty })).toThrow(/is empty/);
  });

  it("reads the session secret from a file when given", () => {
    expect(cfg().secret).toBe("0123456789abcdef0123456789abcdef");
    expect(cfg().secretFromFile).toBe(true);
  });
});

describe("isLoopbackHost", () => {
  it("classifies hosts", () => {
    for (const h of ["localhost", "127.0.0.1", "127.1.2.3", "::1", "[::1]"]) expect(isLoopbackHost(h)).toBe(true);
    for (const h of ["0.0.0.0", "192.168.1.5", "example.com", "::"]) expect(isLoopbackHost(h)).toBe(false);
  });
});

describe("verifyPassword", () => {
  it("accepts the trimmed file content and rejects anything else", () => {
    const c = cfg();
    expect(verifyPassword(c, "hunter2")).toBe(true);
    expect(verifyPassword(c, "hunter2\n")).toBe(true);
    expect(verifyPassword(c, "hunter")).toBe(false);
    expect(verifyPassword(c, "hunter22")).toBe(false);
    expect(verifyPassword(c, "")).toBe(false);
    expect(verifyPassword(cfg({ passwordDigest: null }), "hunter2")).toBe(false);
  });
});

describe("session cookie", () => {
  const req = (cookie?: string, url = "http://127.0.0.1:7700/") =>
    new Request(url, { headers: cookie ? { cookie } : {} });

  it("issues HttpOnly; SameSite=Strict; Path=/ (Secure only on https) and accepts its own cookie", async () => {
    const c = cfg();
    const set = await issueSessionCookie(c, req());
    expect(set).toMatch(/^__taskd_gui_session=/);
    expect(set).toMatch(/HttpOnly/);
    expect(set).toMatch(/SameSite=Strict/);
    expect(set).toMatch(/Path=\//);
    expect(set).not.toMatch(/Secure/);
    expect(await issueSessionCookie(c, req(undefined, "https://gui.example/"))).toMatch(/Secure/);
    const cookie = set.split(";")[0];
    expect(await hasValidSession(c, req(cookie))).toBe(true);
  });

  it("rejects a missing, tampered, foreign-secret or expired cookie", async () => {
    const c = cfg();
    const cookie = (await issueSessionCookie(c, req())).split(";")[0];
    expect(await hasValidSession(c, req())).toBe(false);
    expect(await hasValidSession(c, req(`${cookie}x`))).toBe(false);
    expect(await hasValidSession(c, req("__taskd_gui_session=garbage"))).toBe(false);
    expect(await hasValidSession(cfg({ secret: "another-secret" }), req(cookie))).toBe(false);
    const old = (await issueSessionCookie(c, req(), Date.now() - (SESSION_MAX_AGE_SECONDS + 5) * 1000)).split(";")[0];
    expect(await hasValidSession(c, req(old))).toBe(false);
  });

  it("clears the cookie with Max-Age=0", async () => {
    const set = await clearSessionCookie(cfg(), req());
    expect(set).toMatch(/^__taskd_gui_session=/);
    expect(set).toMatch(/Max-Age=0/);
  });
});

describe("safeNextPath", () => {
  it("only accepts same-origin absolute paths", () => {
    expect(safeNextPath("/tasks?limit=5")).toBe("/tasks?limit=5");
    expect(safeNextPath(null)).toBe("/");
    expect(safeNextPath("")).toBe("/");
    expect(safeNextPath("//evil.example/x")).toBe("/");
    expect(safeNextPath("/\\evil.example")).toBe("/");
    expect(safeNextPath("http://evil.example/")).toBe("/");
    expect(safeNextPath("/login?next=/x")).toBe("/");
  });
});

describe("authCheck middleware", () => {
  async function run(
    url: string,
    cookie?: string,
  ): Promise<{ response: Response | null; context: RouterContextProvider }> {
    const context = new RouterContextProvider();
    const request = new Request(url, { headers: cookie ? { cookie } : {} });
    try {
      await authCheck(
        { request, context, params: {}, url: new URL(url), pattern: "/" },
        async () => new Response("ok"),
      );
      return { response: null, context };
    } catch (e) {
      if (e instanceof Response) return { response: e, context };
      throw e;
    }
  }

  it("passes everything through when auth is disabled", async () => {
    setAuthConfigForTest(readAuthConfig({}));
    const { response, context } = await run("http://127.0.0.1:7700/tasks");
    expect(response).toBeNull();
    expect(context.get(sessionContext)).toEqual({ enabled: false, authenticated: true });
  });

  it("redirects unauthenticated document requests to /login?next=… and lets /login, /logout, /healthz through", async () => {
    setAuthConfigForTest(cfg());
    const { response, context } = await run("http://127.0.0.1:7700/tasks?limit=5");
    expect(response?.status).toBe(302);
    expect(response?.headers.get("location")).toBe(`/login?next=${encodeURIComponent("/tasks?limit=5")}`);
    expect(context.get(sessionContext)).toEqual({ enabled: true, authenticated: false });
    expect((await run("http://127.0.0.1:7700/")).response?.headers.get("location")).toBe("/login");
    expect((await run("http://127.0.0.1:7700/tasks.data?limit=5")).response?.status).toBe(302);
    for (const p of ["/login", "/login?next=%2Ftasks", "/logout", "/healthz"]) {
      expect((await run(`http://127.0.0.1:7700${p}`)).response).toBeNull();
    }
  });

  it("answers 401 (not a redirect) for /events and /files/* without a session", async () => {
    setAuthConfigForTest(cfg());
    for (const p of ["/events", "/events?task_id=x", "/files/tasks/1/runs/2/log"]) {
      const { response } = await run(`http://127.0.0.1:7700${p}`);
      expect(response?.status).toBe(401);
      expect(await response?.text()).toBe("unauthorized");
    }
  });

  it("lets a request with a valid session through", async () => {
    const c = cfg();
    setAuthConfigForTest(c);
    const cookie = (await issueSessionCookie(c, new Request("http://127.0.0.1:7700/"))).split(";")[0];
    const { response, context } = await run("http://127.0.0.1:7700/tasks", cookie);
    expect(response).toBeNull();
    expect(context.get(sessionContext)).toEqual({ enabled: true, authenticated: true });
  });
});
