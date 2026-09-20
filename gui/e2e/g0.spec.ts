import { execFileSync } from "node:child_process";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Health } from "~/celeris/types";
import { expect, test } from "./test";

// Phase G0 作業単位 B（結合テスト）。docs/DESIGN.md §10 Phase G0 の受け入れ条件 4・5 を実 celeris（scripts/celeris.sh start dev）に対して検証する。
// このテストは celeris を `dev` として起動したまま終える（try/finally で保証する）。
//
// 既定は運用中の 7700 / 7710 と同じ値になる。`playwright.config.ts` の注意書きどおり、実行時は必ず
// `CELERIS_GUI_BIND` / `CELERIS_API_URL` / `CELERIS_API_LISTEN` を別ポートへ上書きすること（Phase G13g）。
// `CELERIS_API_LISTEN` は `scripts/celeris.sh`（execFileSync が継承する環境変数）がそのまま読む。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const CELERIS_SH = path.join(REPO_ROOT, "scripts/celeris.sh");
const CELERIS_API_URL = process.env.CELERIS_API_URL ?? "http://127.0.0.1:7710";
const GUI_BIND = process.env.CELERIS_GUI_BIND ?? "127.0.0.1:7700";
const [GUI_HOST, GUI_PORT_STR] = GUI_BIND.split(":");
const GUI_PORT = Number(GUI_PORT_STR);

async function fetchCelerisHealth(): Promise<Health> {
  const res = await fetch(`${CELERIS_API_URL}/api/v1/health`);
  if (!res.ok) throw new Error(`celeris health responded ${res.status}`);
  return (await res.json()) as Health;
}

function stopDev(): void {
  execFileSync(CELERIS_SH, ["stop", "dev"], { cwd: REPO_ROOT, stdio: "pipe" });
}

function startDev(): void {
  execFileSync(CELERIS_SH, ["start", "dev"], { cwd: REPO_ROOT, stdio: "pipe" });
}

test.beforeAll(() => {
  // 他の spec（や前回のラン）が `basic` / `auth` 等を 7710 に残したままだと `start dev` が「別プロセスが応答中」で失敗し、
  // 受け入れ条件 4 の停止 / 復旧も検証できない。既知のインスタンスを全て止めてから `dev` を起動する。
  for (const name of ["basic", "multi-account", "unroutable", "auth", "clusters", "delegation"]) {
    try {
      execFileSync(CELERIS_SH, ["stop", name], { cwd: REPO_ROOT, stdio: "pipe" });
    } catch {
      // 動いていなければ何もしない
    }
  }
  startDev();
});

/**
 * `Host` ヘッダを任意の値にして GET する（Node の http.request なら上書きできる）。
 * Host 検査は root の middleware で全ルート共通なので、`/` である必要は無い。`/` は秘書へ 302 する
 * ようになった（Phase G13f-1）ため、ここでは 302 を挟まず判定できる `/healthz` を使う（Phase G13g）。
 */
function getWithHost(hostHeader: string): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        host: GUI_HOST,
        port: GUI_PORT,
        path: "/healthz",
        method: "GET",
        headers: { Host: hostHeader },
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve({ status: res.statusCode ?? 0, body: Buffer.concat(chunks).toString("utf8") }));
      },
    );
    req.on("error", reject);
    req.end();
  });
}

test.describe("Phase G0 受け入れ条件 4（前半）: celeris の状態表示", () => {
  test("/ に実 celeris の health が表示され、バナーは出ない", async ({ page }) => {
    const expected = await fetchCelerisHealth();

    await page.goto("/");

    await expect(page.getByTestId("celeris_version")).toHaveText(expected.celeris_version);
    await expect(page.getByTestId("celeris_version")).not.toHaveText("");
    await expect(page.getByTestId("api_version")).toHaveText("1");
    expect(expected.api_version).toBe("1");
    await expect(page.getByTestId("schema_version")).toHaveText(String(expected.schema_version));
    expect(Number.isInteger(expected.schema_version)).toBe(true);
    await expect(page.getByTestId("journal_mode")).toContainText(expected.db.journal_mode);

    const footer = page.getByTestId("footer");
    await expect(footer).toContainText("api_version 1");

    await expect(page.getByTestId("celeris-banner")).toHaveCount(0);
  });
});

test.describe("Phase G0 受け入れ条件 4（後半）: celeris 停止中のバナーと復旧", () => {
  test("celeris 停止中は /org/secretary（`/` の遷移先）が 200 でバナー表示、復旧するとリロード無しでバナーが消える", async ({
    page,
  }) => {
    try {
      stopDev();

      // `/` は秘書（`/org/secretary`）へ 302 する最初の画面（Phase G13f-1）。停止中も 200 で開く契約
      // （docs/DESIGN.md §10 Phase G0 受け入れ条件 4）は、いまはこの遷移先が引き継ぐ（Phase G13g）。
      const response = await page.goto("/org/secretary");
      expect(response?.status()).toBe(200);

      const banner = page.getByTestId("celeris-banner");
      await expect(banner).toBeVisible();
      await expect(banner).toContainText("celeris に接続できません");

      // 例外ページ（root の ErrorBoundary）になっていないことを確認する（本文全体で判定する）
      const body = page.locator("body");
      await expect(body).not.toContainText("予期しないエラーが起きました");

      startDev();

      // root は celeris 停止中 5 秒ごとに再検証する（app/root.tsx）。ページの reload はしない。
      await expect(banner).toBeHidden({ timeout: 15_000 });
    } finally {
      // 何が起きても celeris dev は起動した状態で終える
      startDev();
    }
  });
});

test.describe("Phase G0 受け入れ条件 5: Host 検査", () => {
  test("不正な Host は 400、正しい Host は 200", async () => {
    const evil = await getWithHost("evil.example");
    expect(evil.status).toBe(400);

    const ok = await getWithHost(`${GUI_HOST}:${GUI_PORT}`);
    expect(ok.status).toBe(200);
  });
});

test.describe("/healthz", () => {
  test("GUI 自身の生存確認が 200 で JSON を返す", async ({ request }) => {
    const res = await request.get("/healthz");
    expect(res.status()).toBe(200);
    const body = await res.json();
    expect(body.ok).toBe(true);
    expect(body.name).toBe("celeris-gui");
    expect(typeof body.version).toBe("string");
    // ADR-0040 D4（Phase 46）: 昇格で使う `release`。CELERIS_GUI_RELEASE が無ければ "dev"。
    expect(typeof body.release).toBe("string");
  });
});

test.describe("セキュリティヘッダと CSP", () => {
  test("/ の応答ヘッダが揃っており、CSP 違反の console エラーが無い", async ({ page }) => {
    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => pageErrors.push(err.message));

    const response = await page.goto("/");
    expect(response).not.toBeNull();
    const headers = response?.headers() ?? {};

    expect(headers["content-security-policy"] ?? "").toContain("nonce-");
    expect(headers["x-content-type-options"]).toBe("nosniff");
    expect(headers["referrer-policy"]).toBe("no-referrer");
    expect(headers["cache-control"]).toBe("no-store");

    // G1 から root が `/events` に SSE 接続を張ったまま保持するため（意図的な常時接続。docs/DESIGN.md §6.3）、
    // "networkidle" には到達しない。代わりに hydration とスクリプト実行が済むのを待つ固定の猶予を置く。
    await page.waitForTimeout(1_000);

    const cspViolations = [...consoleErrors, ...pageErrors].filter((m) => m.includes("Content Security Policy"));
    expect(cspViolations).toEqual([]);
  });
});
