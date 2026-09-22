// gui/scripts/mobile-audit.mjs — ADR-0055 D1 の機械検査。
//
// 偽の celeris（node:http、`test/mock-celeris/fixtures.ts` の値を使う）と `pnpm build` した GUI の
// `server.js` を、どちらも 127.0.0.1 の空きポートで起動し、Playwright Chromium（393×851、
// `deviceScaleFactor 2.75`、Nothing Phone 2a の Chrome UA）で ADR-0055 D1 の一覧にある画面を開き、
// D1 の 1〜6 を検査する。1 件でも落ちたら非 0 で終わる（`gui/scripts/check-delivery.mjs` と同じ
// 「Build GUI first。実 celeris は起動しない。外部ネットワークに出ない」作り）。
//
// Phase 76（ADR-0055 D1 拡張、ラウンド 8）: D1 の 1〜6 に加えて 3 つのルールを足した。
// `a11y-name`（操作できる要素のアクセシブルな名前）・`a11y-structure`（h1 の数・見出しの階層・
// img/svg の代替情報・ランドマーク）は `runChecks` の一部として `page.evaluate` の中で純粋に判定する。
// `focus-order`（Tab キーでの到達性・罠の検知）だけは実際のキー入力が要るので Node 側の
// `checkFocusOrder(page, route)` として別枠で呼ぶ。
//
// Phase 83 / G36: 偽の celeris（`setupMockCeleris`）と画面一覧（`ROUTES`）は `scripts/e2e-check.mjs`
// （staging に対して読み取り専用の e2e を回す新しい検査）とここで共有するため
// `scripts/lib/celeris-fixture.mjs` に切り出した（同じ画面一覧・同じ偽データを 2 か所に書くと
// 片方だけ更新し忘れる事故が起きるため）。
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { getFreePort, MOBILE_DEVICE, ROUTES, setupMockCeleris, waitForHealth } from "./lib/celeris-fixture.mjs";

const GUI_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(path.join(GUI_DIR, "package.json"));
const { chromium } = require("@playwright/test");

// Phase 91（ADR-0055 ラウンド 15）: 寸法（`window.__MOBILE_AUDIT_WIDTH__`/`HEIGHT__` に使う）だけここで
// 取り出す。コンテキスト自体は `MOBILE_DEVICE`（`devices["Pixel 7"]` を土台にした本物のモバイル記述子。
// `celeris-fixture.mjs`）をそのまま渡す（下記 `browser.newContext(MOBILE_DEVICE)`）。値そのもの
// （393×851、dpr 2.75、Nothing UA）は Phase 69 から変わっていない。
const VIEWPORT = MOBILE_DEVICE.viewport;

const OUT_DIR = path.join(GUI_DIR, "test/mobile-audit");
const REPORT_PATH = path.join(OUT_DIR, "report.json");

// Phase 77（ADR-0055 D1 拡張、`perf`）: 性能予算。CDP の CPU x4 スロットリング（ミッドレンジ機の近似）の下で、
// 初回ナビゲーションの JS/CSS 転送量・DOM ノード数・LCP を計る。light scheme だけで計る（D1 の各ラウンドの
// 慣例どおり、実行時間を抑えるため。ダークモードは色だけが変わるので転送量・DOM 数・LCP はほぼ同じと見なす）。
const CPU_THROTTLING_RATE = 4;
// 計測後、スロットリング下でのハイドレーション・LCP 確定を待つ猶予（x4 なので `load` 直後だとまだ描画中のことがある）。
const PERF_SETTLE_MS = 1000;
const PERF_BUDGET = {
  // Phase 77 の当初案は 350KB だったが、実測すると `entry.client`（React 19 + React Router のクライアント
  // ランタイム。182KB）・`components`（共有 UI チャンク。77KB）・`Icon`（ほぼ全画面が使う手書き SVG。47KB）・
  // `jsx-runtime`（34KB）・`root`（18KB）だけで約 360KB あり、これはどの画面でも必ず要る土台なので
  // 350KB は最適化後も届かない（`docs/PROGRESS.md` Phase 77 参照）。実測した最重量ルート
  // （`/tasks/:id` の各タブ、React.lazy 適用後で 483.6KB）の 10% 増しに設定した
  // （CLAUDE.md「予算が達成不能なら実測値の 10% 増しにして PROGRESS に理由を書く」）。
  jsBytes: 532 * 1024,
  cssBytes: 120 * 1024,
  domNodes: 1500,
  lcpMs: 2500,
};

// ---------------------------------------------------------------------------
// D1 の検査本体。ページ内で評価する純関数は `page.evaluate` にそのまま渡す（DOM が要るので Node 側では書けない）。
// ---------------------------------------------------------------------------

// Phase 87（P-G38-2）: 要素の識別に `node.id`（IDL 属性）を使わず、`node.getAttribute("id")`（content
// 属性を直接読む）を使う。`<form>` が `<input type="hidden" name="id">` のような「name/id が "id" の
// named form control」を持つと、HTML の named-property 機構（named getter）により `form.id` が文字列では
// なくその control 要素自身を返す（ブラウザの仕様上の挙動）。`part += "#" + node.id` はこれを
// `"[object HTMLInputElement]"` に化けさせ、**構造が同じ 2 つのフォーム**（例: `/clusters` の複数の接続
// フォーム）で同じ壊れた文字列に collapse し、cssPathRef が本来ここで打ち切らずに祖先を辿り続けていれば
// 区別できたはずの違い（親要素内での位置など）を握りつぶして衝突する（Phase 86 で実際に踏んだ
// `focus-order` の誤検知。`docs/PROGRESS.md` Phase 86 / `gui/docs/PROGRESS.md` Phase G38 参照）。
// `getAttribute("id")` は content 属性をそのまま読むだけなので、この named-property の影も受けない。
export function cssPathRef(el) {
  if (!(el instanceof Element)) return "";
  const parts = [];
  let node = el;
  let depth = 0;
  while (node && node.nodeType === 1 && depth < 6) {
    let part = node.tagName.toLowerCase();
    const idAttr = node.getAttribute("id");
    if (idAttr) {
      part += `#${idAttr}`;
      parts.unshift(part);
      break;
    }
    const testid = node.getAttribute?.("data-testid");
    if (testid) part += `[data-testid="${testid}"]`;
    const cls = typeof node.className === "string" ? node.className.trim().split(/\s+/).slice(0, 2).join(".") : "";
    if (cls) part += `.${cls}`;
    const parent = node.parentElement;
    if (parent) {
      const idx = Array.from(parent.children).indexOf(node);
      part += `:nth-child(${idx + 1})`;
    }
    parts.unshift(part);
    node = node.parentElement;
    depth += 1;
  }
  return parts.join(" > ");
}

/**
 * 要素自身だけでなく、祖先も含めて実際には描かれていない（`display:none` / `visibility:hidden`）かを見る。
 * `getComputedStyle` は `display`/`font-size` 等を要素自身の値で返す（`display` は継承しないので、
 * 祖先が `display:none` でも子要素自身の値は変わらない）。一方 `getBoundingClientRect()` は祖先が
 * 非表示ならボックスを持たず 0 になる。この非対称のせいで「自分の `style.display` だけ見る」判定は
 * デスクトップ専用の `<aside class="hidden lg:block">` の中身を見落とす。
 *
 * 加えて、閉じた `<details>` の中身（`<summary>` 以外の直接の子とその子孫）は Chromium では
 * `getComputedStyle` 上は `display:none` に**ならない**（実測で確認済み。ラウンド 2 の
 * `task-timeline` 監査で発見。D1-5 の固定要素チェックが誤検知した）。UA の既定の見せ方
 * （`details:not([open]) > *:not(summary)` は描かれない）を構造で判定する。
 */
function isNotVisible(el) {
  let node = el;
  while (node && node.nodeType === 1) {
    const style = getComputedStyle(node);
    if (style.display === "none" || style.visibility === "hidden") return true;
    const parent = node.parentElement;
    if (parent && parent.tagName === "DETAILS" && !parent.open && node.tagName !== "SUMMARY") return true;
    node = parent;
  }
  return false;
}

/** D1-1: 横はみ出し。overflow-x が auto/scroll なコンテナの中身は対象外（D1-6 と両立させるため）。 */
function checkOverflow() {
  const violations = [];
  const width = window.__MOBILE_AUDIT_WIDTH__;
  const docWidth = document.documentElement.scrollWidth;
  if (docWidth > width) {
    violations.push({
      rule: "overflow",
      selector: "html",
      box: { scrollWidth: docWidth },
      detail: `documentElement.scrollWidth=${docWidth} > ${width}`,
    });
  }
  const insideScroller = (el) => {
    let node = el.parentElement;
    while (node) {
      const style = getComputedStyle(node);
      if (style.overflowX === "auto" || style.overflowX === "scroll") return true;
      node = node.parentElement;
    }
    return false;
  };
  for (const el of document.querySelectorAll("body *")) {
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    if (rect.right > width + 0.5 && !insideScroller(el)) {
      violations.push({
        rule: "overflow",
        selector: cssPathRef(el),
        box: { top: rect.top, left: rect.left, right: rect.right, bottom: rect.bottom },
        detail: `right=${rect.right.toFixed(1)} > ${width}`,
      });
    }
  }
  return violations;
}

/** D1-2: タップ領域（44x44）。`data-touch-ok` を付けた要素・その子孫は対象外。 */
function checkTapTargets() {
  const violations = [];
  const selector = "button, a[href], input:not([type=hidden]), select";
  for (const el of document.querySelectorAll(selector)) {
    if (el.closest("[data-touch-ok]")) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    let rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    // checkbox/radio は `<label>` で包んで見た目より大きく押せるようにするのが通常の作り
    // （`~/components/ui/form.ts` の `chipLabelClass`）。実際に押せる範囲は包んでいる `<label>` の方なので、
    // そちらの大きさで判定する（要素自身が小さいこと自体は違反にしない）。
    if (el.tagName === "INPUT" && (el.type === "checkbox" || el.type === "radio")) {
      const label = el.closest("label");
      if (label) rect = label.getBoundingClientRect();
    }
    if (rect.width < 44 - 0.5 || rect.height < 44 - 0.5) {
      violations.push({
        rule: "tap-target",
        selector: cssPathRef(el),
        box: { width: rect.width, height: rect.height },
        detail: `${rect.width.toFixed(1)}x${rect.height.toFixed(1)} < 44x44`,
      });
    }
  }
  return violations;
}

/** D1-3: 状態バッジは 1 語。`data-status-badge` を付けた要素だけを対象にする。 */
function checkStatusBadges() {
  const violations = [];
  for (const el of document.querySelectorAll("[data-status-badge]")) {
    const text = (el.textContent ?? "").trim();
    if (text.length === 0) continue;
    if (/\s/.test(text) || text.length > 12) {
      violations.push({
        rule: "status-badge",
        selector: cssPathRef(el),
        box: {},
        detail: `badge text is not one word: ${JSON.stringify(text)}`,
      });
    }
  }
  return violations;
}

/** D1-4: 本文 14px 以上。`font-mono`（id / sha / パス）は対象外（D2）。 */
function checkFontSize() {
  const violations = [];
  const seen = new Set();
  for (const el of document.querySelectorAll("body *")) {
    if (el.children.length > 0) continue; // 直接テキストを持つ末端要素だけ
    const text = (el.textContent ?? "").trim();
    if (text.length === 0) continue;
    if (el.closest(".font-mono")) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    const size = Number.parseFloat(style.fontSize);
    if (!Number.isFinite(size)) continue;
    if (size < 14 - 0.1) {
      const key = cssPathRef(el);
      if (seen.has(key)) continue;
      seen.add(key);
      violations.push({
        rule: "font-size",
        selector: key,
        box: {},
        detail: `font-size=${size}px < 14px (text=${JSON.stringify(text.slice(0, 24))})`,
      });
    }
  }
  return violations;
}

/**
 * D1-5: 固定要素が内容を隠さない（一番下までスクロールしてから判定）。
 *
 * P-G28-1（Phase 68 の未解決事項 U-G28-2 の解消）: 文書全体の `window.scrollTo` だけでは、
 * `console-stream`（Console の `overflow-y-auto` な内側スクロール領域）のように**それ自身がスクロール
 * する箱**の中身までは末尾に送れない。そのため、育つ返事のような内側スクロール領域の中の要素が
 * 実際にはスクロールすれば読める位置にあるのに、「固定の入力欄より下＝隠れている」という偽陽性を生む
 * （Phase 68 で実際に踏んだので、育つ返事の既定モックへの混入を見送っていた）。ここでは判定の前に、
 * `overflow-y: auto/scroll` かつ実際にスクロールできる（`scrollHeight > clientHeight`）祖先をすべて
 * 一時的に末尾までスクロールし、判定が終わったら元の位置に戻す（スクリーンショットへの影響を避ける）。
 * ルールそのもの（「固定要素より下に来てはいけない」）は緩めていない。
 */
function checkFixedOverlays() {
  const violations = [];
  const height = window.__MOBILE_AUDIT_HEIGHT__;
  window.scrollTo(0, document.body.scrollHeight);
  const innerScrollers = [];
  for (const el of document.querySelectorAll("body *")) {
    const style = getComputedStyle(el);
    if (style.overflowY !== "auto" && style.overflowY !== "scroll") continue;
    if (el.scrollHeight <= el.clientHeight) continue;
    innerScrollers.push({ el, prevTop: el.scrollTop });
    el.scrollTop = el.scrollHeight;
  }
  function restoreInnerScrollers() {
    for (const { el, prevTop } of innerScrollers) el.scrollTop = prevTop;
  }
  const fixed = [];
  for (const el of document.querySelectorAll("body *")) {
    const style = getComputedStyle(el);
    if (style.position !== "fixed") continue;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    fixed.push({ el, rect });
  }
  const bottomBars = fixed.filter((f) => f.rect.top > height / 2);
  if (bottomBars.length === 0) {
    restoreInnerScrollers();
    return violations;
  }
  const minTop = Math.min(...bottomBars.map((f) => f.rect.top));
  let maxContentBottom = 0;
  let worst = null;
  for (const el of document.querySelectorAll("body *")) {
    if (fixed.some((f) => f.el === el || f.el.contains(el))) continue;
    const text = (el.textContent ?? "").trim();
    if (el.children.length > 0 && text.length > 0) continue; // 末端だけ見る（親の重複を避ける）
    if (text.length === 0) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    const rect = el.getBoundingClientRect();
    if (rect.bottom > maxContentBottom) {
      maxContentBottom = rect.bottom;
      worst = el;
    }
  }
  if (worst && maxContentBottom > minTop + 1) {
    violations.push({
      rule: "fixed-overlay",
      selector: cssPathRef(worst),
      box: { contentBottom: maxContentBottom, overlayTop: minTop },
      detail: `content bottom=${maxContentBottom.toFixed(1)} is below the fixed bar top=${minTop.toFixed(1)} even after scrolling to the end`,
    });
  }
  restoreInnerScrollers();
  return violations;
}

/**
 * コントラスト（ADR-0055 D1、Phase 75 追加）。`getComputedStyle` の `color`/`backgroundColor` を、
 * 要素自身から祖先へたどりながら合成する（背景が半透明な場合があるため。例: ダークモードの
 * バッジ背景 `rgb(.. / 0.14)` は下地と混ざって初めて実際の色になる）。全て透明なまま `<html>` まで
 * 抜けたら、キャンバス（描画面）の既定色として白を仮定する。WCAG AA: 18px 未満の文字は 4.5:1、
 * 18px 以上は 3:1。
 */
function parseColor(str) {
  if (!str) return null;
  const m = str.match(/rgba?\(([^)]+)\)/);
  if (!m) return null;
  const parts = m[1].split(",").map((s) => Number.parseFloat(s.trim()));
  const [r, g, b, a = 1] = parts;
  if (![r, g, b].every(Number.isFinite)) return null;
  return { r, g, b, a: Number.isFinite(a) ? a : 1 };
}

function compositeOver(top, bottomRgb) {
  return {
    r: top.r * top.a + bottomRgb.r * (1 - top.a),
    g: top.g * top.a + bottomRgb.g * (1 - top.a),
    b: top.b * top.a + bottomRgb.b * (1 - top.a),
  };
}

function relativeLuminance({ r, g, b }) {
  const [rs, gs, bs] = [r, g, b].map((c) => {
    const cs = c / 255;
    return cs <= 0.03928 ? cs / 12.92 : ((cs + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * rs + 0.7152 * gs + 0.0722 * bs;
}

function contrastRatio(c1, c2) {
  const l1 = relativeLuminance(c1);
  const l2 = relativeLuminance(c2);
  const lighter = Math.max(l1, l2);
  const darker = Math.min(l1, l2);
  return (lighter + 0.05) / (darker + 0.05);
}

/** 要素自身から `<html>` まで、背景色レイヤーを集めてから下地（白）に向かって合成する。 */
function findEffectiveBackground(el) {
  const layers = [];
  let node = el;
  while (node) {
    const style = getComputedStyle(node);
    const bg = parseColor(style.backgroundColor);
    if (bg && bg.a > 0) {
      layers.push(bg);
      if (bg.a >= 0.999) break;
    }
    node = node.parentElement;
  }
  let result = { r: 255, g: 255, b: 255 };
  for (let i = layers.length - 1; i >= 0; i -= 1) result = compositeOver(layers[i], result);
  return result;
}

function checkContrast() {
  const violations = [];
  const seen = new Set();
  for (const el of document.querySelectorAll("body *")) {
    if (el.children.length > 0) continue; // 直接テキストを持つ末端要素だけ
    const text = (el.textContent ?? "").trim();
    if (text.length === 0) continue;
    const style = getComputedStyle(el);
    if (style.display === "none" || style.visibility === "hidden" || isNotVisible(el)) continue;
    const rawFg = parseColor(style.color);
    if (!rawFg) continue;
    const bg = findEffectiveBackground(el);
    const fg = rawFg.a < 1 ? compositeOver(rawFg, bg) : rawFg;
    const ratio = contrastRatio(fg, bg);
    const size = Number.parseFloat(style.fontSize);
    if (!Number.isFinite(size)) continue;
    const threshold = size >= 18 ? 3 : 4.5;
    if (ratio < threshold - 0.02) {
      const key = cssPathRef(el);
      if (seen.has(key)) continue;
      seen.add(key);
      const round = (c) => `${Math.round(c.r)},${Math.round(c.g)},${Math.round(c.b)}`;
      violations.push({
        rule: "contrast",
        selector: key,
        box: {},
        detail:
          `contrast=${ratio.toFixed(2)}:1 < ${threshold}:1 (font-size=${size}px, ` +
          `fg=rgb(${round(fg)}), bg=rgb(${round(bg)}), text=${JSON.stringify(text.slice(0, 24))})`,
      });
    }
  }
  return violations;
}

/** D1-6: 横スクロールが要る表は overflow-x-auto の箱に入っている。 */
function checkTables() {
  const violations = [];
  for (const table of document.querySelectorAll("table")) {
    let node = table.parentElement;
    let wrapped = false;
    while (node) {
      const style = getComputedStyle(node);
      if (style.overflowX === "auto" || style.overflowX === "scroll") {
        wrapped = true;
        break;
      }
      node = node.parentElement;
    }
    if (!wrapped) {
      violations.push({
        rule: "table-wrap",
        selector: cssPathRef(table),
        box: {},
        detail: "no overflow-x-auto ancestor",
      });
    }
  }
  return violations;
}

/**
 * D1 拡張その 5（Phase 91、ADR-0055 ラウンド 15、受け入れ条件 3「ビューポート単位」）: `isMobile: true`
 * の下でも、固定の入力欄（composer、`[data-testid="console-text"]` の祖先 `[data-testid="console-input"]`）と
 * 下部固定タブバー（`[data-testid="mobile-tabbar"]`）が `100dvh`/`env(safe-area-inset-bottom)` を正しく
 * 尊重していることを、composer の下端がタブバーの上端より下に出ない（＝重ならない）ことと、両方とも
 * ビューポートの中に収まっている（`top >= 0` かつ `bottom <= height`）ことで確かめる。どちらか一方でも
 * 画面に無い（Console 以外の画面には composer が無い）場合は対象外（違反にしない）。
 *
 * **`runChecks`（`load` 直後）には入れず、別枠の Node 側関数（`checkViewportUnitsSettled`、下記）から
 * 少し待ってから呼ぶ**。`app/app.css` の `.animate-fade-in`（`<Outlet/>` を包む、各画面共通のページ遷移
 * アニメーション）は、実行中（0.25 秒間）は Chromium がこの要素を fixed/absolute な子孫の containing
 * block に差し替えてしまう（アニメーション対象が `transform` ではなく `translate` でも起きる。実測で
 * 確認した Chromium の実装依存の挙動で、CSS の仕様上 `translate` 自体は containing block を作らないため
 * 完全に避けるにはこの `.animate-fade-in` を fixed な要素の祖先にしないアーキテクチャ変更が要るが、
 * 実害は「ページを開いた直後 0.25 秒だけ composer の位置がわずかにずれる」という、実際に触る前に消える
 * 一過性のもの。`load` 直後に測るとこの一過性の状態を毎回むだに検出してしまうため、アニメーションが
 * 収まるのを待ってから測る）。
 *
 * 注: このサンドボックス（ヘッドレス Chromium）は `env(safe-area-inset-bottom)` を実機のように 0 より
 * 大きい値へ解決しない（`~/root.tsx` のコメントに既にある既知の制約）ため、この検査は「両者が数値上
 * 重ならない・ビューポート内に収まる」という構造の健全性までしか確かめられない。実機でホームインジ
 * ケータの帯がある場合の見た目は実機確認が要る（docs/PROGRESS.md の未解決事項）。
 */
function checkViewportUnits() {
  const violations = [];
  const composer = document.querySelector('[data-testid="console-input"]');
  const tabbar = document.querySelector('[data-testid="mobile-tabbar"]');
  if (!composer || !tabbar) return violations;
  if (isNotVisible(composer) || isNotVisible(tabbar)) return violations;
  // `runChecks` の中で `checkFixedOverlays`（D1-5）がこれより先に文書全体を末尾までスクロールし、内側
  // スクロール領域だけ戻して文書自体のスクロール位置は戻さない。ここで見たいのは「画面を開いた最初の
  // 状態で、固定要素どうしが重ならずビューポートに収まっているか」なので、先頭に戻してから測る
  // （さもないと、本当は `position: fixed` が壊れていて実際には通常フローの要素のように動く composer
  // が、たまたまスクロール位置によってビューポート外に出て「重ならない」と誤って合格してしまう）。
  window.scrollTo(0, 0);
  const height = window.__MOBILE_AUDIT_HEIGHT__;
  const cRect = composer.getBoundingClientRect();
  const tRect = tabbar.getBoundingClientRect();
  if (cRect.bottom > tRect.top + 0.5) {
    violations.push({
      rule: "viewport-units",
      selector: cssPathRef(composer),
      box: { composerBottom: cRect.bottom, tabbarTop: tRect.top },
      detail: `composer bottom=${cRect.bottom.toFixed(1)} is below the tab bar top=${tRect.top.toFixed(1)}`,
    });
  }
  if (cRect.top < -0.5 || cRect.bottom > height + 0.5) {
    violations.push({
      rule: "viewport-units",
      selector: cssPathRef(composer),
      box: { top: cRect.top, bottom: cRect.bottom },
      detail: `composer not within the viewport (height=${height}, top=${cRect.top.toFixed(1)}, bottom=${cRect.bottom.toFixed(1)})`,
    });
  }
  if (tRect.top < -0.5 || tRect.bottom > height + 0.5) {
    violations.push({
      rule: "viewport-units",
      selector: cssPathRef(tabbar),
      box: { top: tRect.top, bottom: tRect.bottom },
      detail: `tab bar not within the viewport (height=${height}, top=${tRect.top.toFixed(1)}, bottom=${tRect.bottom.toFixed(1)})`,
    });
  }
  return violations;
}

/**
 * アクセシブルな名前（Phase 76、ADR-0055 D1 拡張）。WAI-ARIA の accessible name 算出を厳密に実装は
 * しない（そこまでの精度は要らない）が、仕様が挙げる代表的な情報源を優先順位どおりに見る:
 * `aria-label` → `aria-labelledby`（参照先の textContent）→ `<label for>` / 包む `<label>` →
 * `<input type=submit|button|reset>` の `value` → 自身の textContent → 最後の手段として `title`。
 * `placeholder` は仕様上アクセシブルな名前にならないので対象に入れない（プレースホルダだけの入力欄を
 * 見落とさないため、意図して外す）。
 */
function computeAccessibleName(el) {
  const ariaLabel = el.getAttribute("aria-label");
  if (ariaLabel?.trim()) return ariaLabel.trim();
  const labelledby = el.getAttribute("aria-labelledby");
  if (labelledby) {
    const text = labelledby
      .split(/\s+/)
      .map((id) => document.getElementById(id)?.textContent ?? "")
      .join(" ")
      .trim();
    if (text) return text;
  }
  if (el.id) {
    const label = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
    const text = (label?.textContent ?? "").trim();
    if (text) return text;
  }
  const wrappingLabel = el.closest("label");
  if (wrappingLabel) {
    const text = (wrappingLabel.textContent ?? "").trim();
    if (text) return text;
  }
  if (el.tagName === "INPUT" && ["submit", "button", "reset"].includes(el.type) && el.value?.trim()) {
    return el.value.trim();
  }
  const text = (el.textContent ?? "").trim();
  if (text) return text;
  const title = el.getAttribute("title");
  if (title?.trim()) return title.trim();
  return "";
}

/**
 * D1 拡張その 1（Phase 76）: 操作できる要素（button / a[href] / input・select・textarea /
 * role=button|tab|menuitem）は非空のアクセシブルな名前を持つ。アイコンだけのボタン（`~/components/ui/Icon.tsx`
 * は常に `aria-hidden` なので、囲む button/a 自身に `aria-label` が無いと名前が空になる）を主な標的にする。
 */
function checkA11yNames() {
  const violations = [];
  const selector =
    'button, a[href], input:not([type="hidden"]), select, textarea, [role="button"], [role="tab"], [role="menuitem"]';
  for (const el of document.querySelectorAll(selector)) {
    if (isNotVisible(el)) continue;
    const rect = el.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) continue;
    const name = computeAccessibleName(el);
    if (!name) {
      violations.push({
        rule: "a11y-name",
        selector: cssPathRef(el),
        box: {},
        detail: `no accessible name on <${el.tagName.toLowerCase()}${el.getAttribute("role") ? ` role=${el.getAttribute("role")}` : ""}>`,
      });
    }
  }
  return violations;
}

/**
 * D1 拡張その 2（Phase 76）: 画面の骨格。
 * - 可視な `h1` がちょうど 1 個（0 個も 2 個以上も違反）。
 * - 可視な見出しが並び順でレベルを飛ばさない（例: h1 の次に h3。axe-core の heading-order と同じ、
 *   直前の見出しとの比較）。
 * - `img` は `alt` 属性を持つ（空文字 `alt=""` は装飾として許容。属性そのものが無いのが違反）。
 * - `svg` は `role="img"`（`aria-label`/`aria-labelledby`/`<title>` のいずれかで名前を持つ）か、
 *   装飾なら `aria-hidden="true"`（`~/components/ui/Icon.tsx` は既にそう）。どちらでもない宙ぶらりんが違反。
 * - ランドマーク: 可視な `main`（または `role=main`）と `nav`（または `role=navigation`）が画面に 1 つ以上ある
 *   （`~/root.tsx` は `<main>` は常時、`<nav>` はデスクトップの `Sidebar` かモバイルの `MobileTabBar` の
 *   どちらか一方だけが可視になる）。
 */
function checkA11yStructure() {
  const violations = [];
  const headings = Array.from(document.querySelectorAll("h1, h2, h3, h4, h5, h6")).filter((h) => !isNotVisible(h));
  const h1Count = headings.filter((h) => h.tagName === "H1").length;
  if (h1Count !== 1) {
    violations.push({
      rule: "a11y-structure",
      selector: "h1",
      box: {},
      detail: `expected exactly one visible h1, found ${h1Count}`,
    });
  }
  let prevLevel = null;
  for (const h of headings) {
    const level = Number(h.tagName[1]);
    if (prevLevel !== null && level > prevLevel + 1) {
      violations.push({
        rule: "a11y-structure",
        selector: cssPathRef(h),
        box: {},
        detail: `heading level skips from h${prevLevel} to h${level}`,
      });
    }
    prevLevel = level;
  }
  for (const img of document.querySelectorAll("img")) {
    if (isNotVisible(img)) continue;
    if (!img.hasAttribute("alt")) {
      violations.push({
        rule: "a11y-structure",
        selector: cssPathRef(img),
        box: {},
        detail: "img missing alt attribute",
      });
    }
  }
  for (const svg of document.querySelectorAll("svg")) {
    if (isNotVisible(svg)) continue;
    const role = svg.getAttribute("role");
    const hidden = svg.getAttribute("aria-hidden") === "true";
    if (role === "img") {
      const label = svg.getAttribute("aria-label");
      const labelledby = svg.getAttribute("aria-labelledby");
      const titleText = svg.querySelector("title")?.textContent?.trim();
      if (!label?.trim() && !labelledby && !titleText) {
        violations.push({
          rule: "a11y-structure",
          selector: cssPathRef(svg),
          box: {},
          detail: "svg[role=img] missing accessible name",
        });
      }
    } else if (!hidden) {
      violations.push({
        rule: "a11y-structure",
        selector: cssPathRef(svg),
        box: {},
        detail: "decorative svg missing aria-hidden",
      });
    }
  }
  const visible = (els) => Array.from(els).some((el) => !isNotVisible(el));
  if (!visible(document.querySelectorAll('main, [role="main"]'))) {
    violations.push({ rule: "a11y-structure", selector: "main", box: {}, detail: "no visible main landmark" });
  }
  if (!visible(document.querySelectorAll('nav, [role="navigation"]'))) {
    violations.push({ rule: "a11y-structure", selector: "nav", box: {}, detail: "no visible nav landmark" });
  }
  return violations;
}

function runChecks() {
  return [
    ...checkOverflow(),
    ...checkTapTargets(),
    ...checkStatusBadges(),
    ...checkFontSize(),
    ...checkFixedOverlays(),
    ...checkTables(),
    ...checkContrast(),
    ...checkA11yNames(),
    ...checkA11yStructure(),
  ];
}

/**
 * Phase 88（`focus-order` のフレーク解消）: `load` イベント直後はまだ CSR のハイドレーションや、
 * サーバでは出せない値をクライアントだけで確定させる描画（例: hydration mismatch を避けるための
 * 相対時刻表示、`app/lib/reports.ts::relativeTimeLabel` 参照）が終わっていないことがある。この間に
 * focusable な要素が増減すると、`checkFocusOrder` が数えた要素数（Tab 予算の元）と、実際に Tab キーで
 * 辿る時点の要素数がずれ、`composer` に予算内で届かなくなる（Phase 87 merge 後に 1 回だけ踏んだ
 * `home` light のフレーク。`docs/PROGRESS.md` Phase 87 参照。再実行では通っていた = タイミング依存で
 * あることの傍証）。
 *
 * フォント読み込み（`document.fonts.ready`）を待ち、加えて DOM のノード総数が一定回数連続で変化しなく
 * なる（= ハイドレーション後の再レンダーが収まった）ことを確認してから検査を始める。SSE（`/events`）は
 * 接続を張ったまま意図的に閉じない作りなので `page.waitForLoadState("networkidle")` はここでは使えない
 * （`e2e/g0.spec.ts` が同じ理由で固定の猶予に頼っているのと同じ制約）。DOM 安定待ちは `timeoutMs` で
 * 打ち切るので、何らかの理由で本当に揺れ続けるページでもハングしない（その場合は従来どおり計測時点の
 * 値で進む）。
 */
async function waitForPageIdle(page, { timeoutMs = 5000, intervalMs = 100, stableRounds = 3 } = {}) {
  await page.evaluate(() => document.fonts.ready).catch(() => {});
  const start = Date.now();
  let lastCount = -1;
  let stable = 0;
  while (Date.now() - start < timeoutMs) {
    const count = await page.evaluate(() => document.querySelectorAll("*").length);
    if (count === lastCount) {
      stable += 1;
      if (stable >= stableRounds) return;
    } else {
      stable = 0;
      lastCount = count;
    }
    await page.waitForTimeout(intervalMs);
  }
}

/**
 * D1 拡張その 3（Phase 76、`focus-order`）: 文書の先頭から実際に Tab キーを送り、フォーカスが
 * 罠にはまらず（= 同じ要素から動かなくなったら罠）進むかを見る。Console 画面（`console-text` を持つ画面。
 * `/`・`/org/:id`）だけは、下部固定の入力欄（composer）まで、画面上の操作可能な要素数を上回らない歩数で
 * 辿り着けることまで確かめる（辿り着けない＝ DOM 順が入力欄より手前で行き止まっている）。
 * それ以外の画面は「罠が無い」ことだけを見る（`console-text` が無いので composer の到達は対象外）。
 *
 * Phase 88: 罠の判定・到達判定は、この関数の冒頭で全ての focusable 要素に割り振った一意な連番
 * （`data-mobile-audit-focus-id`）を主に使う。以前は `window.__cssPathRef(document.activeElement)` の
 * 再構築だけに頼っていたが、これは「タグ名 + class 名の先頭 2 語 + 兄弟内の位置」から組み立てる**構造的な
 * 署名**なので、skip link（フォーカス時だけ見た目が変わる `sr-only focus:not-sr-only`）・下部固定タブバー
 * （選択中タブに `aria-current` が付いて class が変わる）・disclosure トグル（`aria-expanded` の
 * 開閉で兄弟の構成が変わる）のように、**同じ論理要素でもフォーカス時の状態で class 構成が変わりうる**
 * ものを踏むと、たまたま別の要素と同じ署名になったり、同じ要素が別の署名に見えたりしうる。連番は
 * DOM ノードの同一性に直接紐づく属性なので、こうした揺れの影響を受けない（歩いている途中で新しく
 * 現れた要素だけは連番を持たないので、その場合に限り従来の構造的な署名にフォールバックする）。
 * また、実際に辿った経路（`data-testid` があればそれ、無ければ構造的な署名）を `path` として集め、
 * 予算内に composer へ届かなかったときの違反 `detail` に含める（将来のフレーク調査を高速化する）。
 *
 * 罠の判定: Tab を押しても署名が直前と全く同じままなら、そのキー入力はフォーカスを動かせていない
 * （=罠）。フォーカスがドキュメント外（ブラウザ chrome 等）へ抜けたら `null` が返るので、単に
 * 「その画面の残りの要素を辿り終えた」として歩みを止める（罠ではない）。
 */
async function checkFocusOrder(page, route) {
  const violations = [];
  const hasComposer = await page.evaluate(() => document.querySelector('[data-testid="console-text"]') !== null);
  // Phase 88: `runChecks`（D1-1〜D1-6 等）を読み終えたあとにここで初めて待つ。他の画面のチェックは
  // `load` 直後の DOM のままにして、`focus-order` の予算計算と実際に歩く時点の DOM だけをそろえる。
  await waitForPageIdle(page);
  const focusableCount = await page.evaluate(() => {
    const selector = 'a[href], button, input:not([type="hidden"]), select, textarea, [tabindex]:not([tabindex="-1"])';
    const els = Array.from(document.querySelectorAll(selector)).filter((el) => {
      const style = getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden") return false;
      const rect = el.getBoundingClientRect();
      return !(rect.width === 0 && rect.height === 0);
    });
    // Phase 88: この時点で見えている focusable 要素に一意な連番を振り、以後の Tab 追跡はこれで
    // 要素の同一性を判定する（cssPathRef の構造的な署名の揺れに左右されないように）。
    els.forEach((el, i) => {
      el.setAttribute("data-mobile-audit-focus-id", String(i));
    });
    return els.length;
  });
  // Phase 88: 焦点可能要素数 + 余裕（12。以前は 10）を基本に、最低 24 歩は許す。
  const maxSteps = Math.max(focusableCount + 12, 24);
  const path = [];
  let prevSig = null;
  let reachedComposer = false;
  for (let i = 0; i < maxSteps; i += 1) {
    await page.keyboard.press("Tab");
    const info = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return null;
      const markerId = el.getAttribute("data-mobile-audit-focus-id");
      return {
        markerId,
        testid: el.getAttribute("data-testid") ?? "",
        structural: window.__cssPathRef(el),
      };
    });
    if (info === null) break; // ドキュメント外へ出た = 辿り終えた
    // 連番があればそれを同一性の判定に使う（無ければ＝歩いている途中で新しく現れた要素。従来どおりの
    // 構造的な署名にフォールバック）。
    const sig = info.markerId !== null ? `id:${info.markerId}` : `struct:${info.testid}::${info.structural}`;
    path.push(info.testid ? `${info.testid}(${info.structural})` : info.structural);
    if (sig === prevSig) {
      violations.push({
        rule: "focus-order",
        selector: info.structural,
        box: {},
        detail: `Tab did not move focus away from this element after step ${i + 1} (trap). path=${JSON.stringify(path)}`,
      });
      break;
    }
    prevSig = sig;
    if (info.testid === "console-text" || info.testid === "console-send") {
      reachedComposer = true;
      break;
    }
  }
  if (hasComposer && !reachedComposer && violations.length === 0) {
    violations.push({
      rule: "focus-order",
      selector: route,
      box: {},
      detail:
        `composer (console-text) not reached from document start within ${maxSteps} Tab presses ` +
        `(${focusableCount} focusable elements on page). focus sequence: ${JSON.stringify(path)}`,
    });
  }
  return violations;
}

/**
 * `checkViewportUnits`（上記）を、`app/app.css` の `.animate-fade-in`（ページ遷移の入り口アニメーション、
 * 0.25 秒）が収まってから呼ぶ。安く済ませるため、まず composer とタブバーが両方とも画面にあるかだけを
 * 待ち無しで確かめ（Console 以外の画面は待つ意味が無いので早期に諦める）、両方ある画面だけアニメーション
 * 時間分（0.25 秒 + 余裕）待ってから測る。両方のスキーム（light/dark）で呼ぶ（`perf`/タッチ系と違い、
 * 対象画面が少ない〈Console がある画面だけ〉ので実行時間への影響は軽い）。
 */
async function checkViewportUnitsSettled(page) {
  const hasBoth = await page.evaluate(() => {
    return (
      document.querySelector('[data-testid="console-input"]') !== null &&
      document.querySelector('[data-testid="mobile-tabbar"]') !== null
    );
  });
  if (!hasBoth) return [];
  await page.waitForTimeout(400);
  return page.evaluate(() => window.__checkViewportUnits());
}

// ---------------------------------------------------------------------------
// D2 拡張（Phase 91、ADR-0055 ラウンド 15、受け入れ条件 2「タッチ操作」）: 実際のタッチ入力（`page.mouse`
// の合成マウスイベントではなく、CDP `Input.dispatchTouchEvent` — Chromium の compositor が実機のタッチ
// ジェスチャーと同じ経路で扱う入力）で操作する。light scheme だけで行う（dark は色だけなのでスクロール
// 可能性・タップ結果は変わらない、`perf` と同じ慣例）。
// ---------------------------------------------------------------------------

/**
 * `touch-scroll`: 横スクロールが要るコンテナ（`overflow-x: auto/scroll` かつ `scrollWidth > clientWidth`。
 * D1-6 の `checkTables` が見る「表を包む箱」と同種のもの全般が対象）を、指でスワイプしたときと同じ
 * タッチ入力で実際にスクロールできることを確かめる。JS の合成イベント（`dispatchEvent(new TouchEvent(...))`）
 * ではブラウザのネイティブなオーバーフロー・スクロール（compositor が担う）は動かないため、CDP の
 * 低レベル入力を直接叩く（Playwright の `touchscreen.tap()` が内部でしていることの、スワイプ版）。
 * スクロールできなかった場合は `touch-action` 等を疑えるよう検出時点の値を `detail` に残す。
 * 判定後は元の `scrollLeft` に戻す（後続のスクリーンショット・他画面の判定に影響しないように）。
 */
async function checkTouchScroll(page, cdpSession, route) {
  const violations = [];
  if (!cdpSession) return violations;
  // ここに来るまでに `checkFixedOverlays`（D1-5、`runChecks` の一部）が文書全体を末尾までスクロールした
  // まま戻していない（内側スクロール領域だけ戻す作り）ことがあり、`checkFocusOrder` の Tab 歩行も
  // フォーカス先要素をブラウザがネイティブに可視領域へスクロールすることがある。CDP の
  // `Input.dispatchTouchEvent` はビューポート相対座標を取るので、既知の基準（先頭）に戻してから
  // 個々のコンテナを `scrollIntoViewIfNeeded` する方が、途中のスクロール量に依存せず安定する。
  await page.evaluate(() => window.scrollTo(0, 0)).catch(() => {});
  const scrollers = await page.evaluate(() => {
    const nodes = Array.from(document.querySelectorAll("body *")).filter((el) => {
      const style = getComputedStyle(el);
      if (style.overflowX !== "auto" && style.overflowX !== "scroll") return false;
      if (el.scrollWidth <= el.clientWidth) return false;
      const rect = el.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    });
    nodes.forEach((el, i) => {
      el.setAttribute("data-mobile-audit-scroller", String(i));
    });
    return nodes.map((el, i) => ({
      idx: i,
      selector: window.__cssPathRef(el),
      touchAction: getComputedStyle(el).touchAction,
    }));
  });
  for (const { idx, selector, touchAction } of scrollers) {
    const handle = page.locator(`[data-mobile-audit-scroller="${idx}"]`);
    // CDP の `Input.dispatchTouchEvent` はビューポート相対座標を取る。画面の下の方（縦スクロールが要る位置）
    // にあるコンテナだと `boundingBox()` の座標がビューポート外を指し、タッチが要素に届かない
    // （的外れな場所をタップしたのと同じになる）ので、先にビューポート内へスクロールしておく。
    await handle.scrollIntoViewIfNeeded().catch(() => {});
    await page.waitForTimeout(50); // スクロールが実際に落ち着くのを待つ（座標を取る前に）。
    const box = await handle.boundingBox();
    if (!box) continue;
    const before = await handle.evaluate((el) => el.scrollLeft);
    const y = Math.round(box.y + Math.min(box.height / 2, Math.max(box.height - 1, 0)));
    const startX = Math.round(box.x + Math.max(box.width - 8, box.width / 2));
    const endX = Math.round(box.x + Math.min(8, box.width / 2));
    const steps = 10;
    await cdpSession.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: startX, y }] });
    for (let s = 1; s <= steps; s += 1) {
      const x = Math.round(startX + ((endX - startX) * s) / steps);
      await cdpSession.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x, y }] });
      await page.waitForTimeout(16);
    }
    await cdpSession.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await page.waitForTimeout(80);
    const after = await handle.evaluate((el) => el.scrollLeft);
    await handle.evaluate((el, v) => {
      el.scrollLeft = v;
    }, before);
    if (after <= before + 1) {
      violations.push({
        rule: "touch-scroll",
        selector,
        box: { before, after },
        detail:
          `touch swipe did not scroll this horizontally-scrollable container ` +
          `(scrollLeft ${before} -> ${after}, touch-action=${touchAction}); route=${route}`,
      });
    }
  }
  return violations;
}

/**
 * `tap`: 各画面の「主な操作」— `[data-primary-action]` があればそれ、無ければ最初の
 * `button[type="submit"]`、それも無ければ primary variant の `Button`（`~/components/ui/button.tsx` の
 * `bg-primary`+`text-primary-fg`）— を `click()` ではなく実際のタッチ（`locator.tap()`、`hasTouch: true`
 * のコンテキストで CDP のタッチ入力を発行する）で操作し、その結果コンソールエラー・例外が出ないことを
 * 確かめる。無効化されている（`disabled`/`aria-disabled`）ボタンや、そもそも対象が無い画面は対象外
 * （タップ自体をスキップする。違反にしない）。`<a>` は対象外（タップでナビゲーションが起きると
 * この監査の前提〈同じページに留まる〉が崩れるため、意図して `button` だけを見る）。
 */
async function checkPrimaryActionTap(page, route) {
  const violations = [];
  const target = await page.evaluate(() => {
    // `isNotVisible`（`addInitScript` でこのページに既に注入済み。閉じた `<details>` の中身が
    // Chromium の `getComputedStyle` 上は `display:none` に**ならない**という既知の落とし穴を扱う。
    // `runChecks` の各検査と同じ判定を再利用する。`releases`/`knowledge/skills` の主操作は
    // `<details>` の中（`app/routes/releases.tsx`・`skill-detail`）にあるので、これを使わないと
    // 閉じたまま見えない要素を「見える」と誤判定してタップを試み、`locator.tap()` がタイムアウトする。
    const visible = (el) => {
      if (!el) return false;
      if (window.__isNotVisible(el)) return false;
      const rect = el.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    };
    let el = document.querySelector("[data-primary-action]");
    let marker = "data-primary-action";
    if (!el || !visible(el)) {
      el = Array.from(document.querySelectorAll('button[type="submit"]')).find(visible) ?? null;
      marker = "submit";
    }
    if (!el) {
      el =
        Array.from(document.querySelectorAll("button")).find(
          (b) => visible(b) && b.classList.contains("bg-primary") && b.classList.contains("text-primary-fg"),
        ) ?? null;
      marker = "primary-button";
    }
    if (!el) return { found: false };
    el.setAttribute("data-mobile-audit-tap-target", "1");
    return {
      found: true,
      marker,
      disabled: el.disabled === true || el.getAttribute("aria-disabled") === "true",
      selector: window.__cssPathRef(el),
    };
  });
  if (!target.found || target.disabled) return violations;
  const consoleErrors = [];
  const onConsole = (msg) => {
    // 偽の celeris（`celeris-fixture.mjs::setupMockCeleris`）は GET しか実装しない（このリポジトリの
    // e2e/監査の慣例: 書き込みを試さない）ので、primary action が変更系の POST を発行すると、GUI の
    // action は celeris からの 404（"not_found"）をそのまま HTTP ステータスとして返す（`clusters.tsx`
    // の action が celeris のエラーの status をそのまま反映する、等）。Chromium はこの種の非 2xx な
    // 応答を、アプリの JS が実際に `console.error` を呼んだかどうかに関わらず「Failed to load resource:
    // the server responded with a status of NNN」として自動的にコンソールへ出す（ブラウザ自身のネット
    // ワークログで、アプリのバグの兆候ではない）。この検査が見たいのは「タップの結果アプリが例外を
    // 投げる／処理し損ねる」ことなので、この定型メッセージだけは対象から除く。
    if (
      msg.type() === "error" &&
      !/^Failed to load resource: the server responded with a status of \d+/.test(msg.text())
    ) {
      consoleErrors.push(msg.text());
    }
  };
  const onPageError = (err) => consoleErrors.push(err.message);
  page.on("console", onConsole);
  page.on("pageerror", onPageError);
  try {
    await page.locator("[data-mobile-audit-tap-target]").tap({ timeout: 5000 });
    await page.waitForTimeout(300);
  } catch (e) {
    violations.push({
      rule: "tap",
      selector: target.selector,
      box: {},
      detail: `tap on ${target.marker} (route=${route}) failed: ${/** @type {Error} */ (e).message}`,
    });
  } finally {
    page.off("console", onConsole);
    page.off("pageerror", onPageError);
  }
  if (consoleErrors.length > 0) {
    violations.push({
      rule: "tap",
      selector: target.selector,
      box: {},
      detail: `console error after tapping ${target.marker} (route=${route}): ${consoleErrors.join(" | ").slice(0, 300)}`,
    });
  }
  return violations;
}

// ---------------------------------------------------------------------------
// D1 拡張その 4（Phase 77、`perf`）: 性能予算。
// ---------------------------------------------------------------------------

/**
 * ページ内の `window.__perfEntries`（`PERF_OBSERVER_SOURCE`、`addInitScript` で全ページに配線）から
 * FCP・LCP・DOM ノード数を読む。LCP は「バッファ済みの候補のうち最後（= 最大の startTime）」を最終値とみなす
 * （spec の考え方どおり、ユーザー操作や visibility change が無いヘッドレス計測ではそのまま安定する）。
 */
async function readPerfMetrics(page) {
  return page.evaluate(() => {
    const entries = window.__perfEntries ?? { paint: [], lcp: [] };
    const fcpEntry = entries.paint.find((e) => e.name === "first-contentful-paint");
    const lcpTimes = entries.lcp.map((e) => e.startTime);
    return {
      fcpMs: fcpEntry ? fcpEntry.startTime : null,
      lcpMs: lcpTimes.length > 0 ? Math.max(...lcpTimes) : null,
      domNodes: document.querySelectorAll("*").length,
    };
  });
}

/** 予算超過を `perf` ルールの違反として返す。1 指標につき最大 1 件。 */
function checkPerfBudget(record) {
  const violations = [];
  const over = (value, budget) => typeof value === "number" && Number.isFinite(value) && value > budget;
  if (over(record.js_bytes, PERF_BUDGET.jsBytes)) {
    violations.push({
      rule: "perf",
      selector: record.route,
      box: {},
      detail:
        `initial JS ${(record.js_bytes / 1024).toFixed(1)}KB > ${(PERF_BUDGET.jsBytes / 1024).toFixed(0)}KB budget ` +
        `(${record.js_chunks} chunks)`,
    });
  }
  if (over(record.css_bytes, PERF_BUDGET.cssBytes)) {
    violations.push({
      rule: "perf",
      selector: record.route,
      box: {},
      detail: `initial CSS ${(record.css_bytes / 1024).toFixed(1)}KB > ${(PERF_BUDGET.cssBytes / 1024).toFixed(0)}KB budget`,
    });
  }
  if (over(record.dom_nodes, PERF_BUDGET.domNodes)) {
    violations.push({
      rule: "perf",
      selector: record.route,
      box: {},
      detail: `DOM nodes ${record.dom_nodes} > ${PERF_BUDGET.domNodes} budget`,
    });
  }
  if (over(record.lcp_ms, PERF_BUDGET.lcpMs)) {
    violations.push({
      rule: "perf",
      selector: record.route,
      box: {},
      detail: `LCP ${record.lcp_ms.toFixed(0)}ms > ${PERF_BUDGET.lcpMs}ms budget (CPU x${CPU_THROTTLING_RATE})`,
    });
  }
  return violations;
}

/** `perf` の結果を固定幅の表（`console.error` 1 回。`by_rule` の JSON の前に出す）にする。 */
function formatPerfTable(records) {
  const cols = [
    ["route", 20],
    ["js_kb", 8],
    ["chunks", 7],
    ["css_kb", 8],
    ["dom", 6],
    ["fcp_ms", 7],
    ["lcp_ms", 7],
  ];
  const pad = (s, w) => String(s).slice(0, w).padEnd(w);
  const lines = [cols.map(([name, w]) => pad(name, w)).join(" | ")];
  lines.push(cols.map(([, w]) => "-".repeat(w)).join("-+-"));
  for (const r of records) {
    const row = [
      r.route,
      (r.js_bytes / 1024).toFixed(1),
      String(r.js_chunks),
      (r.css_bytes / 1024).toFixed(1),
      String(r.dom_nodes),
      r.fcp_ms == null ? "-" : r.fcp_ms.toFixed(0),
      r.lcp_ms == null ? "-" : r.lcp_ms.toFixed(0),
    ];
    lines.push(row.map((v, i) => pad(v, cols[i][1])).join(" | "));
  }
  return lines.join("\n");
}

/**
 * 現在の HEAD の短い sha（Phase 87、監査レポートの証跡強化）。celeris 本体の運用（`celeris@<sha12>`
 * ユニット名）と同じ桁数に合わせる。取れなければ（`.git` が無い配布物など）レポートを壊さず `"unknown"`。
 */
function gitShortSha() {
  const res = spawnSync("git", ["rev-parse", "--short=12", "HEAD"], { cwd: GUI_DIR, encoding: "utf8" });
  if (res.status !== 0) return "unknown";
  return res.stdout.trim() || "unknown";
}

/**
 * Phase 88（受け入れ条件 3、任意）: `--routes <glob>` と `--repeat <N>` を読む。どちらも既定値のままなら
 * これまでどおり全 26 route を 1 回だけ回る（既存の `pnpm mobile-audit` の挙動に影響しない）。
 * `--routes` はカンマ区切りで複数指定でき、各要素は `route` id（`scripts/lib/celeris-fixture.mjs::buildRoutes`
 * の `route`。例: `home`、`task-overview`）に対する `*` ワイルドカード付きの前方一致もどき（単純な
 * `RegExp` 変換）。1 画面だけ直しているときに 26 route × light/dark をフルで回さずに済むようにする
 * （反復のたびに `pnpm build` からやり直す必要はないので、`MOBILE_AUDIT_SKIP_BUILD=1` と組み合わせて使う
 * 想定）。
 */
function parseCliArgs(argv) {
  let routes = null;
  let repeat = 1;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--routes" || arg === "--repeat") {
      const value = argv[i + 1];
      if (value === undefined) throw new Error(`mobile-audit: ${arg} には値が要ります`);
      if (arg === "--routes") routes = value;
      else repeat = Number.parseInt(value, 10);
      i += 1;
    } else if (arg.startsWith("--routes=")) {
      routes = arg.slice("--routes=".length);
    } else if (arg.startsWith("--repeat=")) {
      repeat = Number.parseInt(arg.slice("--repeat=".length), 10);
    }
  }
  if (!Number.isFinite(repeat) || repeat < 1) throw new Error("mobile-audit: --repeat は 1 以上の整数にしてください");
  return { routes, repeat };
}

/** `--routes` のカンマ区切りパターン（`*` だけをワイルドカードとして扱う）に一致する route を選ぶ。 */
function filterRoutes(routes, pattern) {
  if (!pattern) return routes;
  const patterns = pattern
    .split(",")
    .map((p) => p.trim())
    .filter((p) => p.length > 0);
  const regexes = patterns.map((p) => new RegExp(`^${p.split("*").map(escapeRegExp).join(".*")}$`));
  const selected = routes.filter((r) => regexes.some((re) => re.test(r.route)));
  if (selected.length === 0) {
    throw new Error(
      `mobile-audit: --routes "${pattern}" に一致する route が無い（${routes.map((r) => r.route).join(", ")}）`,
    );
  }
  return selected;
}

function escapeRegExp(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

async function main() {
  const { routes: routesPattern, repeat } = parseCliArgs(process.argv.slice(2));
  const selectedRoutes = filterRoutes(ROUTES, routesPattern);

  fs.mkdirSync(OUT_DIR, { recursive: true });
  for (const name of fs.readdirSync(OUT_DIR)) fs.rmSync(path.join(OUT_DIR, name), { force: true });

  const gitSha = gitShortSha();

  const skipBuild = process.env.MOBILE_AUDIT_SKIP_BUILD === "1";
  if (!skipBuild) {
    const build = spawnSync("pnpm", ["build"], { cwd: GUI_DIR, stdio: "inherit" });
    if (build.status !== 0) {
      console.error("mobile-audit: pnpm build failed");
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

  // Phase 88（受け入れ条件 3、任意）: `--repeat` はブラウザ・偽の celeris・ビルドした GUI サーバを
  // 使い回したまま（下で 1 回だけ起動する）、route/scheme の歩みだけを N 回繰り返す（`focus-order` の
  // ようなフレークを手元で再現・確認するとき、毎回 `pnpm build` からやり直すより速い）。既定の
  // `repeat=1` ではこれまでの「1 回だけ回る」挙動と 1 バイトも変わらない。
  let anyFailed = false;
  let browser;
  try {
    await waitForHealth(`http://${guiBind}/healthz`);
    browser = await chromium.launch({ headless: true });
    // Phase 91: `MOBILE_DEVICE`（`devices["Pixel 7"]` 土台）をそのまま渡す。`isMobile`/`hasTouch` は
    // Phase 69 から変わらず true のままだが、記述子を土台にすることで touch-scroll/tap（下記）が使う
    // タッチ入力の前提が実機によく似た構成であることを明示する。
    const context = await browser.newContext(MOBILE_DEVICE);
    // celeris へは fetch/EventSource で直接出ない構成だが、念のため GUI 以外への要求は塞ぐ（外部ネットワーク不使用）。
    await context.route("**/*", (route) => {
      const url = new URL(route.request().url());
      return url.hostname === "127.0.0.1" && url.port === String(guiPort) ? route.continue() : route.abort();
    });
    // 検査本体はブラウザ内で走る必要があるので、各関数の `toString()` を組み立てて `addInitScript` で
    // 毎ページに注入する（`page.evaluate(fn)` は `fn` 単体しか送れず、参照している他の関数までは
    // 持って行けないため）。
    const auditSource = [
      `window.__MOBILE_AUDIT_WIDTH__ = ${VIEWPORT.width};`,
      `window.__MOBILE_AUDIT_HEIGHT__ = ${VIEWPORT.height};`,
      cssPathRef.toString(),
      isNotVisible.toString(),
      checkOverflow.toString(),
      checkTapTargets.toString(),
      checkStatusBadges.toString(),
      checkFontSize.toString(),
      checkFixedOverlays.toString(),
      checkTables.toString(),
      parseColor.toString(),
      compositeOver.toString(),
      relativeLuminance.toString(),
      contrastRatio.toString(),
      findEffectiveBackground.toString(),
      checkContrast.toString(),
      computeAccessibleName.toString(),
      checkA11yNames.toString(),
      checkA11yStructure.toString(),
      checkViewportUnits.toString(),
      runChecks.toString(),
      "window.__runMobileAudit = runChecks;",
      // Phase 91: `checkViewportUnitsSettled`（Node 側の別枠の `page.evaluate`）が使う。
      "window.__checkViewportUnits = checkViewportUnits;",
      // Phase 76: `checkFocusOrder`（Node 側、実際に Tab キーを送る）が要素を突き合わせるのに使う。
      "window.__cssPathRef = cssPathRef;",
      // Phase 91: `checkPrimaryActionTap`（Node 側の別枠の `page.evaluate`）が使う。
      "window.__isNotVisible = isNotVisible;",
      // Phase 77（`perf`）: FCP/LCP を `PerformanceObserver`（`buffered: true`）で拾う。`addInitScript` は
      // 文書の最初のスクリプトより前に評価されるので、最初のペイントから取りこぼさない。どちらの
      // entry type も未対応のブラウザでは黙って諦める（try/catch。Chromium では両方とも実装済み）。
      `window.__perfEntries = { paint: [], lcp: [] };
      try {
        new PerformanceObserver((list) => {
          for (const e of list.getEntries()) window.__perfEntries.paint.push({ name: e.name, startTime: e.startTime });
        }).observe({ type: "paint", buffered: true });
      } catch {}
      try {
        new PerformanceObserver((list) => {
          for (const e of list.getEntries()) window.__perfEntries.lcp.push({ startTime: e.startTime, size: e.size });
        }).observe({ type: "largest-contentful-paint", buffered: true });
      } catch {}`,
    ].join("\n");
    await context.addInitScript(auditSource);

    // Phase 88（受け入れ条件 3、任意）: `--repeat` の回数だけ、この route/scheme の歩みをまるごと
    // 繰り返す（ブラウザ・偽の celeris・GUI サーバは上で 1 回起動したものを使い回す）。各回の集計は
    // 独立させる（`allViolations` 等をループの内側で作り直す）。既定の `repeat=1` なら 1 回だけ回る。
    for (let rep = 1; rep <= repeat; rep += 1) {
      const allViolations = [];
      const routeReports = [];
      const perfRecords = [];
      // Phase 75（ADR-0055 D1、P-G30-1 の一環でダークモードも監査対象に）: 各画面を light / dark の
      // 両方の `prefers-color-scheme` で開く。`page.emulateMedia` は `goto` 前に設定すれば初回描画から
      // 反映される（`app/app.css` の `@media (prefers-color-scheme: dark)` がトークンを切り替える作り）。
      for (const { route, path: routePath } of selectedRoutes) {
        for (const scheme of /** @type {const} */ (["light", "dark"])) {
          // Phase 87（監査レポートの証跡強化）: どのルートが監査の実行時間を食っているかを見えるようにする。
          const routeStartedAt = Date.now();
          const page = await context.newPage();
          await page.emulateMedia({ colorScheme: scheme });
          const pageErrors = [];
          page.on("pageerror", (err) => pageErrors.push(err.message));

          // Phase 77（`perf`）: light だけ CPU x4 スロットリング（ミッドレンジ機の近似）を掛け、この
          // ページが読み込む JS/CSS の転送量を数える。dark はここを飛ばす（色だけが変わるので、転送量・
          // DOM 数・LCP は light とほぼ同じと見なして、実行時間を抑える。D1 のこれまでのラウンドと同じ判断）。
          const perfResponses = [];
          let cdpSession;
          /** @param {import("@playwright/test").Response} res */
          function onPerfResponse(res) {
            const resourceType = res.request().resourceType();
            if (resourceType !== "script" && resourceType !== "stylesheet") return;
            const lenHeader = res.headers()["content-length"];
            const bytes = lenHeader ? Number.parseInt(lenHeader, 10) : Number.NaN;
            if (!Number.isFinite(bytes)) return; // Content-Length が無い応答（SSE 等）は数えない
            perfResponses.push({ resourceType, bytes });
          }
          if (scheme === "light") {
            cdpSession = await context.newCDPSession(page);
            await cdpSession.send("Emulation.setCPUThrottlingRate", { rate: CPU_THROTTLING_RATE });
            page.on("response", onPerfResponse);
          }

          const response = await page.goto(`http://${guiBind}${routePath}`, { waitUntil: "load" });
          // `load` が発火した時点で数え終える（「初回ナビゲーションの転送量」の境界。`load` は寄稿元 HTML が
          // 参照する静的なリソースの完了を待つが、ハイドレーション中に React.lazy が発行する動的 import() の
          // 応答は待たない。ここで listener を外さないと、あとの `checkFocusOrder`/`PERF_SETTLE_MS` の待ちの
          // 間に届くその種の遅延チャンクまで数えてしまい、遅延読み込みで減らしたはずの初回転送量が見かけ上
          // 減らない）。
          page.off("response", onPerfResponse);
          const status = response?.status() ?? 0;
          if (status !== 200) {
            allViolations.push({
              route,
              scheme,
              rule: "http-status",
              selector: routePath,
              box: {},
              detail: `GET ${routePath} -> ${status}`,
            });
          } else {
            // `runChecks`（D1-1〜D1-6、a11y-name/structure）は Phase 76 以来、`load` 直後の DOM をそのまま見る
            // 作り（他のラウンドの既存の挙動）。ここに `waitForPageIdle` を挟むと、`focus-order` とは無関係な
            // 別の画面（例: `/projects/:id` の `WorkTreeGraph`、`@xyflow/react` の dagre レイアウトが
            // `useEffect` で非同期に確定する）まで待たせてしまい、Phase 88 のスコープ外の潜在バグ
            // （レイアウト確定後にグラフが 393px を飛び出す、ズームボタンが 44×44 未満）を新たに検出・
            // 破壊してしまう（実際に試して確認した）。`focus-order` の修正は `checkFocusOrder` の中だけに
            // 閉じる（下記）。
            const violations = await page.evaluate(() => window.__runMobileAudit());
            for (const v of violations) allViolations.push({ route, scheme, ...v });

            // Phase 91（受け入れ条件 3「ビューポート単位」）: `runChecks` の外（`.animate-fade-in` の
            // 0.25 秒アニメーションが収まるのを待ってから）で行う。両スキームで呼ぶ（Console がある画面
            // だけ待つので軽い）。
            const viewportUnitViolations = await checkViewportUnitsSettled(page);
            for (const v of viewportUnitViolations) allViolations.push({ route, scheme, ...v });

            if (scheme === "light") {
              // スロットリング下のハイドレーション・LCP 確定を待つ（`load` の時点ではまだのことがある）。
              await page.waitForTimeout(PERF_SETTLE_MS);
              const metrics = await readPerfMetrics(page);
              const jsBytes = perfResponses.filter((r) => r.resourceType === "script").reduce((a, r) => a + r.bytes, 0);
              const cssBytes = perfResponses
                .filter((r) => r.resourceType === "stylesheet")
                .reduce((a, r) => a + r.bytes, 0);
              const jsChunks = perfResponses.filter((r) => r.resourceType === "script").length;
              const record = {
                route,
                js_bytes: jsBytes,
                css_bytes: cssBytes,
                js_chunks: jsChunks,
                dom_nodes: metrics.domNodes,
                fcp_ms: metrics.fcpMs,
                lcp_ms: metrics.lcpMs,
              };
              perfRecords.push(record);
              for (const v of checkPerfBudget(record)) allViolations.push({ route, scheme: "light", ...v });
              // Phase 88（`focus-order` のフレーク解消）: perf 計測が終わったら CPU スロットリングを元に
              // 戻してから `checkFocusOrder` に入る。スロットリング下のままだと、Tab でフォーカスが
              // `console-stream`（内側スクロール領域）の奥へ動くたびに走る「最新へ」ボタンの表示判定
              // （スクロールに連動する React の再描画）が 4 倍遅れ、次の Tab 押下と衝突して
              // `document.activeElement` が一瞬 `<body>` に落ちる（＝「歩き終えた」と誤認する）ことがある
              // （Phase 87 merge 後に 1 回だけ踏んだ `home` light のフレークの再現に成功。単体の
              // デバッグスクリプトでスロットリング無しなら 15/15 回とも `composer` に届き、スロットリング
              // 有りだと崩れることを確認した）。`focus-order` は実際のキー入力の応答性を見る検査なので、
              // ミッドレンジ機の近似（`perf`）とは切り離して素の速度で行うのが筋でもある。
              await cdpSession?.send("Emulation.setCPUThrottlingRate", { rate: 1 }).catch(() => {});
            }
            // Phase 76: フォーカス順（`focus-order`）はページごとに実際の Tab キーで確かめる必要があるので
            // `page.evaluate` 単体の `runChecks` には入れず、ここで別枠として呼ぶ。
            const focusViolations = await checkFocusOrder(page, route);
            for (const v of focusViolations) allViolations.push({ route, scheme, ...v });

            // Phase 91（受け入れ条件 2「タッチ操作」）: `focus-order`（キー入力）の後に行う。タップは
            // フォーカスを動かしうるので、先に `focus-order`（document 先頭からの Tab 到達性）を済ませて
            // おく（順序を入れ替えると、この後のタップで生まれた新しいフォーカス位置が `focus-order` の
            // 起点になってしまい、この検査自体が作った偽陽性になる）。light scheme だけで行う（`perf` と
            // 同じ慣例。`cdpSession` は light scheme のときだけ生きている＝上の CPU スロットリングと
            // 同じセッションを使い回す）。
            if (scheme === "light") {
              const touchScrollViolations = await checkTouchScroll(page, cdpSession, route);
              for (const v of touchScrollViolations) allViolations.push({ route, scheme, ...v });
              const tapViolations = await checkPrimaryActionTap(page, route);
              for (const v of tapViolations) allViolations.push({ route, scheme, ...v });
            }
          }
          await cdpSession?.detach().catch(() => {});
          if (pageErrors.length > 0) {
            allViolations.push({
              route,
              scheme,
              rule: "page-error",
              selector: routePath,
              box: {},
              detail: pageErrors.join(" / "),
            });
          }
          // light は既存どおり `<route>.png`、dark は `<route>.dark.png`（目視差分用。git には入れない）。
          const shotName = scheme === "dark" ? `${route}.dark.png` : `${route}.png`;
          const shotPath = path.join(OUT_DIR, shotName);
          await page.screenshot({ path: shotPath, fullPage: true }).catch(() => {});
          routeReports.push({ route, path: routePath, scheme, status, duration_ms: Date.now() - routeStartedAt });
          await page.close();
        }
      }

      // Phase 88（受け入れ条件 3、任意）: `--repeat` のときはレポート・要約を回ごとに出す（`report.json`
      // は毎回上書きするので最後の回の中身が残る。`rep`/`repeat` を JSON と 1 行要約の両方に足す）。
      fs.writeFileSync(
        REPORT_PATH,
        JSON.stringify(
          {
            generated_at: new Date().toISOString(),
            // Phase 87（監査レポートの証跡強化）: どの HEAD に対する監査結果かをレポート自身に残す
            // （`docs/PROGRESS.md`/`gui/docs/PROGRESS.md` に貼るときにコミットとの対応が一目で分かるように）。
            git_sha: gitSha,
            routes: routeReports,
            violations: allViolations,
            perf: perfRecords,
            perf_budget: PERF_BUDGET,
          },
          null,
          2,
        ),
      );

      const byRule = {};
      const byScheme = { light: 0, dark: 0 };
      for (const v of allViolations) {
        byRule[v.rule] = (byRule[v.rule] ?? 0) + 1;
        if (v.scheme) byScheme[v.scheme] = (byScheme[v.scheme] ?? 0) + 1;
      }
      // Phase 87: 性能予算表の中で最も重い（js+css 転送量が最大の）ルートを 1 つ拾い、一行まとめに出す
      // （`perf` は light scheme だけで計るので、ここも light の合計で比べる）。
      let perfWorst = null;
      for (const r of perfRecords) {
        const totalKb = (r.js_bytes + r.css_bytes) / 1024;
        if (!perfWorst || totalKb > perfWorst.totalKb) perfWorst = { route: r.route, totalKb };
      }
      // `gui/biome.json` は `console.log` を禁止している（`error`/`warn` だけ許可）ので `console.error` で出す。
      console.error(formatPerfTable(perfRecords));
      console.error(
        JSON.stringify(
          {
            ok: allViolations.length === 0,
            total: allViolations.length,
            by_rule: byRule,
            by_scheme: byScheme,
            git_sha: gitSha,
            report: REPORT_PATH,
            ...(repeat > 1 ? { rep, repeat } : {}),
          },
          null,
          2,
        ),
      );
      // Phase 87（監査スクリプトの堅牢化）: JSON を読まなくても目で追える 1 行の要約。
      console.error(
        `routes=${selectedRoutes.length} schemes=2 violations=${allViolations.length} ` +
          `perf_worst=${perfWorst?.route ?? "-"} ${perfWorst ? perfWorst.totalKb.toFixed(1) : "0.0"}KB` +
          (repeat > 1 ? ` rep=${rep}/${repeat}` : ""),
      );
      if (allViolations.length > 0) anyFailed = true;
    }
  } finally {
    await browser?.close();
    gui.kill("SIGTERM");
    await mock.close();
  }
  process.exit(anyFailed ? 1 : 0);
}

// Phase 87（監査スクリプトの堅牢化）: `pnpm mobile-audit`（`node scripts/mobile-audit.mjs`）として直接
// 実行されたときだけ `main()`（実 Chromium の起動・偽の celeris・`pnpm build` を伴う重い処理）を走らせる。
// この module-execution guard が無いと、`cssPathRef` を単体テストから `import` するだけで `main()` まで
// 実行されてしまい（トップレベル await）、ユニットテストのはずが毎回フル監査を走らせる重い・遅い・
// ブラウザ依存のテストになってしまう。
const isMainModule = process.argv[1] != null && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMainModule) {
  await main();
}
