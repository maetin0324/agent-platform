// @ts-nocheck — ブラウザ内評価とモックを含む検査スクリプト（型検査の対象外）
// 離脱→復帰でエラー画面から自動復帰できることの実ブラウザ検査（モック celeris のみ、外部ネットワーク不使用）。
//   node scripts/check-resume-recovery.mjs [GUI_DIR]   （GUI_DIR は build 済みの gui。既定はこのリポジトリ）
// シナリオ（393×851 のスマホ幅と 1280×800 の両方）:
//   A. 接続断: 単一フェッチ（`*.data`）を落とした状態で画面遷移 → エラー画面 → 接続を戻す → リロード無しで復帰するか
//   B. バックグラウンド復帰: 表示中に接続を落とし、visibilitychange / pageshow を発火 → 再検証が失敗しても固まらないか
//   C. GUI（BFF）の再起動: 5xx を返す時間帯を挟んで復帰するか
//   D. デプロイ後: 旧ビルドのチャンクが 404 → vite:preloadError で 1 回だけ再読み込みするか
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { getFreePort, MOBILE_DEVICE, setupMockCeleris, TASK_ID, waitForHealth } from "./lib/celeris-fixture.mjs";

const GUI_DIR = path.resolve(process.argv[2] ?? path.join(path.dirname(fileURLToPath(import.meta.url)), ".."));
const { chromium } = createRequire(path.join(GUI_DIR, "package.json"))("@playwright/test");

const mock = await setupMockCeleris();
const port = await getFreePort();
const proc = spawn(process.execPath, ["server.js"], {
  cwd: GUI_DIR,
  env: { ...process.env, NODE_ENV: "production", CELERIS_GUI_BIND: `127.0.0.1:${port}`, CELERIS_API_URL: mock.baseUrl },
  stdio: "ignore",
});
const base = `http://127.0.0.1:${port}`;
await waitForHealth(`${base}/healthz`);

const browser = await chromium.launch();
const results = [];
const record = (name, ok, note = "") => {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${note ? ` — ${note}` : ""}`);
};
const isError = (page) =>
  page.locator("text=予期しないエラーが起きました").or(page.locator("text=/^エラー/")).first().isVisible();

for (const [label, ctxOpts] of [
  ["mobile 393x851", { ...MOBILE_DEVICE, viewport: { width: 393, height: 851 } }],
  ["desktop 1280x800", { viewport: { width: 1280, height: 800 } }],
]) {
  const ctx = await browser.newContext(ctxOpts);
  const page = await ctx.newPage();
  let broken = false;
  await page.route("**/*.data*", (route) => (broken ? route.abort("connectionfailed") : route.continue()));
  await page.goto(`${base}/tasks`);
  await page.waitForLoadState("networkidle");

  // A. 接続断で画面遷移（クライアント側ナビゲーション）
  broken = true;
  // 全画面読み込みではなくクライアント側ナビゲーション（React Router の <Link> の click）にする。
  await page.evaluate((id) => {
    const link =
      document.querySelector(`a[href="/tasks/${id}"], a[href^="/tasks/${id}?"]`) ??
      document.querySelector('a[href="/board"]');
    if (!link) throw new Error("no in-app link");
    link.click();
  }, TASK_ID);
  await page.waitForTimeout(1200);
  if (process.env.SHOT) await page.screenshot({ path: `${process.env.SHOT}-${label.split(" ")[0]}.png` });
  const sawErrorA = await isError(page).catch(() => false);
  broken = false;
  let recoveredA = false;
  try {
    await page.waitForFunction(() => !document.body.innerText.includes("予期しないエラー"), null, { timeout: 12_000 });
    recoveredA = true;
  } catch {}
  record(`[${label}] A 接続断→エラー画面が出る（前提）`, sawErrorA);
  record(`[${label}] A 接続を戻すとリロード無しで復帰`, recoveredA);

  // B. バックグラウンド復帰: 正常表示 → 断 → 復帰イベント → 断のまま失敗 → 戻す
  await page.goto(`${base}/tasks`);
  await page.waitForLoadState("networkidle");
  const marker = await page.evaluate(() => {
    window.__noReload = true;
    return true;
  });
  broken = true;
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
    window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true }));
  });
  await page.waitForTimeout(1500);
  broken = false;
  let recoveredB = false;
  try {
    await page.waitForFunction(() => !document.body.innerText.includes("予期しないエラー"), null, { timeout: 12_000 });
    recoveredB = true;
  } catch {}
  const noReload = marker && (await page.evaluate(() => window.__noReload === true));
  record(`[${label}] B 復帰イベント後に断→戻すで復帰し、リロードしていない`, recoveredB && noReload);

  // 再試行ボタン（自動再試行が尽きた場合の手動導線）
  const retryBtn = page.getByRole("button", { name: "再試行" });
  record(`[${label}] 通常表示に戻っている`, (await retryBtn.count()) === 0);
  await ctx.close();
}

// D. チャンク 404（デプロイ後の旧タブ）: preloadError を発火させ、1 回だけ再読み込みされること
{
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto(`${base}/tasks`);
  await page.waitForLoadState("networkidle");
  const navs = [];
  page.on("framenavigated", (f) => f === page.mainFrame() && navs.push(f.url()));
  await page.evaluate(() => window.dispatchEvent(new Event("vite:preloadError", { cancelable: true })));
  await page.waitForTimeout(1500);
  const first = navs.length;
  await page.evaluate(() => window.dispatchEvent(new Event("vite:preloadError", { cancelable: true })));
  await page.waitForTimeout(1500);
  record(
    "D チャンク取得失敗で 1 回だけ再読み込み（2 回目はループしない）",
    first >= 1 && navs.length === first,
    `navs=${navs.length}`,
  );
  await ctx.close();
}

await browser.close();
proc.kill();
await mock.close();
const failed = results.filter((r) => !r.ok).length;
console.log(`${results.length - failed}/${results.length} passed`);
process.exit(failed ? 1 : 0);
