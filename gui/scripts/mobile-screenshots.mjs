// gui/scripts/mobile-screenshots.mjs — ADR-0055 ラウンド 19（Phase 95）。
//
// `mobile-audit.mjs` は機械的な違反（タップ領域・コントラスト・a11y・固定要素の重なり等）を検査するが、
// 「人が見て分かる粗さ」（情報の優先順位・余白の一貫性・文字の階層・空状態の文言など）は機械では判定
// できない。このスクリプトは監査ではなく撮影専用: `MOBILE_DEVICE`（Nothing Phone 2a 相当、393×851、
// `celeris-fixture.mjs`）で 26 route × light/dark を開き、フルページのスクリーンショットを
// `gui/test/mobile-screenshots/<route>-<scheme>.png` に保存するだけ（違反判定は一切しない、exit code は
// 常に 0＝撮影自体が成功したかどうかのみ）。撮った画像を人（またはエージェント）が Read で開いて目視する。
//
// 起動・偽の celeris・ビルド省略の流儀は `mobile-audit.mjs` と同じもの（`celeris-fixture.mjs` を共有）。
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { getFreePort, MOBILE_DEVICE, ROUTES, setupMockCeleris, waitForHealth } from "./lib/celeris-fixture.mjs";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(path.join(GUI_DIR, "package.json"));
const { chromium } = require("@playwright/test");

const OUT_DIR = path.join(GUI_DIR, "test/mobile-screenshots");

async function main() {
  fs.mkdirSync(OUT_DIR, { recursive: true });
  for (const name of fs.readdirSync(OUT_DIR)) fs.rmSync(path.join(OUT_DIR, name), { force: true });

  // `mobile-audit.mjs` と同じ流儀: `MOBILE_SCREENSHOTS_SKIP_BUILD=1` で既存の `build/` を使い回し、
  // 撮り直しのたびに `pnpm build` からやり直さずに済ませる（直す→撮り直すのループを速くするため）。
  const skipBuild = process.env.MOBILE_SCREENSHOTS_SKIP_BUILD === "1";
  if (!skipBuild) {
    const build = spawnSync("pnpm", ["build"], { cwd: GUI_DIR, stdio: "inherit" });
    if (build.status !== 0) {
      console.error("mobile-screenshots: pnpm build failed");
      process.exit(build.status ?? 1);
    }
  }

  const mock = await setupMockCeleris();
  const guiPort = await getFreePort();
  const guiBind = `127.0.0.1:${guiPort}`;
  const guiLog = fs.openSync(path.join(OUT_DIR, "gui.log"), "w");
  const gui = spawn(process.execPath, ["server.js"], {
    cwd: GUI_DIR,
    env: { ...process.env, NODE_ENV: "production", CELERIS_GUI_BIND: guiBind, CELERIS_API_URL: mock.baseUrl },
    stdio: ["ignore", guiLog, guiLog],
  });

  let browser;
  let failed = false;
  try {
    await waitForHealth(`http://${guiBind}/healthz`);
    browser = await chromium.launch({ headless: true });
    const context = await browser.newContext(MOBILE_DEVICE);
    await context.route("**/*", (route) => {
      const url = new URL(route.request().url());
      return url.hostname === "127.0.0.1" && url.port === String(guiPort) ? route.continue() : route.abort();
    });

    const saved = [];
    for (const { route, path: routePath } of ROUTES) {
      for (const scheme of /** @type {const} */ (["light", "dark"])) {
        const page = await context.newPage();
        await page.emulateMedia({ colorScheme: scheme });
        let status = 0;
        try {
          const response = await page.goto(`http://${guiBind}${routePath}`, { waitUntil: "load" });
          status = response?.status() ?? 0;
          // ページ遷移フェードイン（`.animate-fade-in`、Phase 91/92）とハイドレーション後の非同期レイアウト
          // （WorkTreeGraph の dagre 等）が収まってから撮る。監査と違い、ここでは「実際に見える最終状態」を
          // 撮りたいので待つ。
          await page.waitForTimeout(400);
        } catch (err) {
          console.error(
            `mobile-screenshots: ${route} (${scheme}) failed to load: ${/** @type {Error} */ (err).message}`,
          );
          failed = true;
        }
        const shotName = `${route}-${scheme}.png`;
        const shotPath = path.join(OUT_DIR, shotName);
        try {
          await page.screenshot({ path: shotPath, fullPage: true });
          saved.push({ route, scheme, status });
        } catch (err) {
          console.error(
            `mobile-screenshots: ${route} (${scheme}) failed to screenshot: ${/** @type {Error} */ (err).message}`,
          );
          failed = true;
        }
        await page.close();
      }
    }
    console.error(
      JSON.stringify({ ok: !failed, routes: ROUTES.length, schemes: 2, saved: saved.length, out_dir: OUT_DIR }),
    );
  } finally {
    await browser?.close();
    gui.kill("SIGTERM");
    await mock.close();
  }
  process.exit(failed ? 1 : 0);
}

const isMainModule = process.argv[1] != null && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMainModule) {
  await main();
}
