import { type ChildProcess, execFileSync, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { request as playwrightRequest } from "@playwright/test";
import { expect, test } from "./test";

// Phase G5 の受け入れ条件 1〜3（docs/DESIGN.md §10 Phase G5、docs/adr/0008-g5-decisions.md）。
// 条件 1（パスワード認証）は `TASKD_GUI_BIND=0.0.0.0:<port>` の GUI をこのファイルが自分で起動する（数秒間、ランダムなパスワード付き）。
// 条件 2（トークン）は `scripts/taskd.sh fixture auth`（`[api] token_file`）の taskd に対して、トークンあり / 無しの GUI を起動して見る。
// 条件 3（Host / CSP）は Playwright の webServer（7700）に対して行う。CSP 違反 0 件は `./test` の auto fixture が全シナリオで assert する。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");
const TASKD_API_URL = process.env.TASKD_API_URL ?? "http://127.0.0.1:7710";
const GUI_BASE = `http://${process.env.TASKD_GUI_BIND ?? "127.0.0.1:7700"}`;
const AUTH_PORT = 7721;
const TOKEN_PORT = 7722;
const NOTOKEN_PORT = 7723;
const INSTANCE_NAMES = ["dev", "basic", "multi-account", "unroutable", "auth"] as const;

function sh(...args: string[]): string {
  return execFileSync(TASKD_SH, args, { cwd: REPO_ROOT, stdio: "pipe" }).toString();
}

function stopAll(): void {
  for (const name of INSTANCE_NAMES) {
    try {
      sh("stop", name);
    } catch {
      // 動いていなければ何もしない
    }
  }
}

/** `node server.js` を起動し、`/healthz` が答えるまで待つ。stderr は `logFile` に落とす（トークン漏えいの grep 対象）。 */
async function startGui(env: Record<string, string>, logFile: string): Promise<ChildProcess> {
  const out = fs.openSync(logFile, "w");
  const child = spawn("node", ["server.js"], {
    cwd: REPO_ROOT,
    env: { ...process.env, ...env },
    stdio: ["ignore", out, out],
  });
  fs.closeSync(out);
  const port = env.TASKD_GUI_BIND.split(":").pop();
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null)
      throw new Error(`server.js exited early (${child.exitCode}): ${fs.readFileSync(logFile, "utf8")}`);
    try {
      const status = await rawGet({ host: "127.0.0.1", port: Number(port), path: "/healthz" });
      if (status.status === 200) return child;
    } catch {
      // まだ listen していない
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`server.js did not answer /healthz within 20s: ${fs.readFileSync(logFile, "utf8")}`);
}

async function stopGui(child: ChildProcess | undefined): Promise<void> {
  if (!child || child.exitCode !== null) return;
  await new Promise<void>((resolve) => {
    child.once("exit", () => resolve());
    child.kill("SIGTERM");
    setTimeout(() => {
      child.kill("SIGKILL");
      resolve();
    }, 3_000).unref();
  });
}

/** `Host` ヘッダを任意の値にできる素の GET（`agent: false` で毎回新しいソケット。e2e/g4.spec.ts と同じ理由）。 */
function rawGet(opts: {
  host: string;
  port: number;
  path: string;
  headers?: Record<string, string>;
}): Promise<{ status: number; headers: http.IncomingHttpHeaders; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: opts.host, port: opts.port, path: opts.path, method: "GET", headers: opts.headers, agent: false },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () =>
          resolve({ status: res.statusCode ?? 0, headers: res.headers, body: Buffer.concat(chunks).toString("utf8") }),
        );
      },
    );
    req.on("error", reject);
    req.end();
  });
}

/** ディレクトリ配下の全ファイルから `needle` を含むファイルを数える（受け入れ条件 2 の `grep -r`）。 */
function grepCount(roots: string[], needle: string): string[] {
  const hits: string[] = [];
  const walk = (p: string) => {
    if (!fs.existsSync(p)) return;
    const st = fs.statSync(p);
    if (st.isDirectory()) {
      for (const e of fs.readdirSync(p)) walk(path.join(p, e));
    } else if (fs.readFileSync(p).includes(needle)) {
      hits.push(p);
    }
  };
  for (const r of roots) walk(r);
  return hits;
}

test.describe("受け入れ条件 3: Host 検査と CSP ヘッダ", () => {
  test("Host: evil.example は 400、全ページの応答に Content-Security-Policy がある", async ({ page }) => {
    const base = new URL(GUI_BASE);
    const evil = await rawGet({
      host: base.hostname,
      port: Number(base.port),
      path: "/",
      headers: { Host: "evil.example" },
    });
    expect(evil.status).toBe(400);
    // 静的アセットと未定義パスにも Host 検査と CSP / nosniff が掛かる（docs/adr/0008 D15）
    const asset = await rawGet({
      host: base.hostname,
      port: Number(base.port),
      path: "/assets/",
      headers: { Host: "evil.example" },
    });
    expect(asset.status).toBe(400);
    const notFound = await page.goto("/no-such-page");
    expect(notFound?.status()).toBe(404);
    expect(notFound?.headers()["content-security-policy"] ?? "").toContain("nonce-");
    expect(notFound?.headers()["x-content-type-options"]).toBe("nosniff");
    await expect(page.locator("main")).toContainText("404");
    for (const p of ["/", "/tasks", "/tasks/new", "/plans/new", "/daemon", "/providers", "/graph", "/healthz"]) {
      const res = await page.goto(p);
      expect(res, p).not.toBeNull();
      const csp = res?.headers()["content-security-policy"] ?? "";
      expect(csp, p).toContain("default-src 'self'");
      expect(csp, p).toContain("frame-ancestors 'none'");
      expect(res?.headers()["x-content-type-options"], p).toBe("nosniff");
    }
  });
});

test.describe("受け入れ条件 1: 非 loopback バインドのパスワード認証", () => {
  const passwordFile = path.join(REPO_ROOT, ".run", "g5-password");
  const password = `pw-${Math.random().toString(36).slice(2)}-${Date.now()}`;
  let gui: ChildProcess | undefined;
  const AUTH_BASE = `http://127.0.0.1:${AUTH_PORT}`;

  test.beforeAll(async () => {
    fs.mkdirSync(path.dirname(passwordFile), { recursive: true });
    fs.writeFileSync(passwordFile, `${password}\n`, { mode: 0o600 });
    gui = await startGui(
      { TASKD_GUI_BIND: `0.0.0.0:${AUTH_PORT}`, TASKD_GUI_PASSWORD_FILE: passwordFile, TASKD_API_URL },
      path.join(REPO_ROOT, ".run", "g5-auth-gui.log"),
    );
  });

  test.afterAll(async () => {
    await stopGui(gui);
    fs.rmSync(passwordFile, { force: true });
  });

  test("TASKD_GUI_PASSWORD_FILE 無しの 0.0.0.0 バインドは exit 2（stderr に理由）", () => {
    const r = spawnSync("node", ["server.js"], {
      cwd: REPO_ROOT,
      env: { ...process.env, TASKD_GUI_BIND: `0.0.0.0:${AUTH_PORT + 10}`, TASKD_GUI_PASSWORD_FILE: "" },
      encoding: "utf8",
      timeout: 20_000,
    });
    expect(r.status).toBe(2);
    expect(r.stderr).toContain("TASKD_GUI_PASSWORD_FILE is required");
  });

  test("未ログインの GET / は /login へ 302、/events はクッキー無しで 401", async () => {
    const ctx = await playwrightRequest.newContext({ baseURL: AUTH_BASE });
    try {
      const root = await ctx.get("/", { maxRedirects: 0 });
      expect(root.status()).toBe(302);
      expect(root.headers().location).toBe("/login");
      const tasks = await ctx.get("/tasks?limit=5", { maxRedirects: 0 });
      expect(tasks.status()).toBe(302);
      expect(tasks.headers().location).toBe(`/login?next=${encodeURIComponent("/tasks?limit=5")}`);
      expect(root.headers()["x-content-type-options"]).toBe("nosniff");
      expect(root.headers()["content-security-policy"] ?? "").toContain("default-src 'none'");
      const events = await ctx.get("/events", { maxRedirects: 0 });
      expect(events.status()).toBe(401);
      expect(await events.text()).toBe("unauthorized");
      expect(events.headers()["x-content-type-options"]).toBe("nosniff");
      const login = await ctx.get("/login");
      expect(login.status()).toBe(200);
      expect(login.headers()["content-security-policy"]).toContain("default-src 'self'");
    } finally {
      await ctx.dispose();
    }
  });

  test("誤パスワードは 401 ページ、正しいパスワードで HttpOnly; SameSite=Strict のクッキーが発行され画面に入れる", async ({
    page,
  }) => {
    await page.goto(`${AUTH_BASE}/tasks`);
    await expect(page).toHaveURL(/\/login\?next=%2Ftasks$/);
    await expect(page.locator("nav")).toHaveCount(0); // ログイン前はナビゲーションも taskd の情報も出さない
    await expect(page.locator('[data-testid="footer"]')).toHaveCount(0);

    // 誤パスワード（ブラウザのフォーム）: 1 秒待ってから 401 のログインページを再描画
    const wrongStarted = Date.now();
    await page.fill("#password", "not-the-password");
    await page.click('[data-testid="login-submit"]');
    await expect(page.locator('[data-testid="login-error"]')).toBeVisible();
    expect(Date.now() - wrongStarted).toBeGreaterThanOrEqual(1_000);
    await expect(page).toHaveURL(/\/login/);
    // 誤パスワード（document request）: status 401
    const wrong = await page.request.post(`${AUTH_BASE}/login`, { form: { password: "nope" }, maxRedirects: 0 });
    expect(wrong.status()).toBe(401);
    expect(await wrong.text()).toContain("パスワードが違います");

    // 正しいパスワード: Set-Cookie に HttpOnly; SameSite=Strict、302 で next へ
    const ok = await page.request.post(`${AUTH_BASE}/login`, {
      form: { password, next: "/tasks" },
      maxRedirects: 0,
    });
    expect(ok.status()).toBe(302);
    expect(ok.headers().location).toBe("/tasks");
    const setCookie = ok.headersArray().find((h) => h.name.toLowerCase() === "set-cookie")?.value ?? "";
    expect(setCookie).toMatch(/^__taskd_gui_session=/);
    expect(setCookie).toContain("HttpOnly");
    expect(setCookie).toContain("SameSite=Strict");
    expect(setCookie).toContain("Path=/");
    expect(setCookie).not.toContain("Secure"); // http なので

    // クッキー付きなら画面に入れる（page.request とページはクッキーを共有する）
    const res = await page.goto(`${AUTH_BASE}/`);
    expect(res?.status()).toBe(200);
    await expect(page).toHaveURL(`${AUTH_BASE}/`);
    await expect(page.locator('[data-testid="footer"]')).toContainText("taskd-gui");
    await expect(page.locator('[data-testid="logout"]')).toBeVisible();
    const events = await page.request.get(`${AUTH_BASE}/healthz`);
    expect(events.status()).toBe(200);

    // ログアウトでクッキーが消え、再び /login へ
    await page.click('[data-testid="logout"]');
    await expect(page).toHaveURL(/\/login$/);
    const after = await page.request.get(`${AUTH_BASE}/`, { maxRedirects: 0 });
    expect(after.status()).toBe(302);
  });
});

test.describe("受け入れ条件 2: BFF → taskd のトークン", () => {
  let withToken: ChildProcess | undefined;
  let withoutToken: ChildProcess | undefined;
  let token = "";
  const tokenFile = path.join(REPO_ROOT, ".run", "auth", "api.token");

  test.beforeAll(async () => {
    stopAll();
    sh("fixture", "auth");
    sh("start", "auth");
    token = fs.readFileSync(tokenFile, "utf8").trim();
    expect(token.length).toBeGreaterThanOrEqual(32);
    withToken = await startGui(
      { TASKD_GUI_BIND: `127.0.0.1:${TOKEN_PORT}`, TASKD_API_TOKEN_FILE: tokenFile, TASKD_API_URL },
      path.join(REPO_ROOT, ".run", "auth", "gui.log"),
    );
    withoutToken = await startGui(
      { TASKD_GUI_BIND: `127.0.0.1:${NOTOKEN_PORT}`, TASKD_API_TOKEN_FILE: "", TASKD_API_URL },
      path.join(REPO_ROOT, ".run", "auth", "gui-notoken.log"),
    );
  });

  test.afterAll(async () => {
    await stopGui(withToken);
    await stopGui(withoutToken);
    // この spec がスイートの最後なので、`auth` を止めてスイートを終える（残すと次のランの g0 `start dev` と各 fixture が 7710 で失敗する）
    stopAll();
  });

  test("token_file 付き taskd に TASKD_API_TOKEN_FILE を渡すと動く", async ({ page }) => {
    const health = await rawGet({
      host: "127.0.0.1",
      port: Number(new URL(TASKD_API_URL).port),
      path: "/api/v1/inbox",
    });
    expect(health.status).toBe(401); // taskd 自身がトークン無しを拒む（前提の確認）
    const res = await page.goto(`http://127.0.0.1:${TOKEN_PORT}/tasks`);
    expect(res?.status()).toBe(200);
    await expect(page.locator('[data-testid="taskd-banner"]')).toHaveCount(0);
    await expect(page.locator("main")).toContainText("Auth-A");
    await expect(page.locator('[data-testid="footer"]')).toContainText("api_version 1");
  });

  test("渡さないとバナーに unauthorized", async ({ page }) => {
    const res = await page.goto(`http://127.0.0.1:${NOTOKEN_PORT}/`);
    expect(res?.status()).toBe(401);
    const banner = page.locator('[data-testid="taskd-banner"]').first();
    await expect(banner).toBeVisible();
    await expect(banner).toContainText("unauthorized");
  });

  test("トークンが HTML・build/・GUI のログに出ない", async ({ page }) => {
    const html = await (await page.goto(`http://127.0.0.1:${TOKEN_PORT}/`))?.text();
    expect(html ?? "").not.toContain(token);
    await stopGui(withToken); // ログを flush させる
    await stopGui(withoutToken);
    const runDir = path.join(REPO_ROOT, ".run");
    const guiLogs = fs
      .readdirSync(runDir)
      .map((d) => path.join(runDir, d))
      .filter((d) => fs.statSync(d).isDirectory())
      .flatMap((d) =>
        fs
          .readdirSync(d)
          .filter((f) => /^gui.*\.log$/.test(f))
          .map((f) => path.join(d, f)),
      );
    expect(guiLogs.length).toBeGreaterThanOrEqual(2);
    const hits = grepCount([path.join(REPO_ROOT, "build"), ...guiLogs], token);
    expect(hits).toEqual([]);
  });
});
