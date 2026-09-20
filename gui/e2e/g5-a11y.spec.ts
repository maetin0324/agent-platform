import { execFileSync } from "node:child_process";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { AxeBuilder } from "@axe-core/playwright";
import type { TaskList } from "~/celeris/types";
import { expect, test } from "./test";

// `axe-core` は `@axe-core/playwright` の推移的依存で、この pnpm workspace では phantom dependency に
// なる（`node_modules/axe-core` が無い）ため、型は `AxeBuilder#analyze` の戻り値から推論する
// （`axe-core` を直接 import しない）。
type AxeResults = Awaited<ReturnType<AxeBuilder["analyze"]>>;

// Phase G5 の受け入れ条件 3（CSP ヘッダ、CSP 違反 0 件は `./test` の auto fixture が担う）・
// 受け入れ条件 4（`@axe-core/playwright` の critical / serious が 0 件。docs/adr/0008-g5-decisions.md D8）。
// `scripts/celeris.sh fixture basic && start basic` の実 celeris に対して検証する。moderate / minor の
// violation はゲートにせず `test.info().annotations` に記録するだけにする。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const CELERIS_SH = path.join(REPO_ROOT, "scripts/celeris.sh");

const CELERIS_API_URL = new URL(process.env.CELERIS_API_URL ?? "http://127.0.0.1:7710");
const CELERIS_API_HOST = CELERIS_API_URL.hostname;
const CELERIS_API_PORT = Number(CELERIS_API_URL.port || "80");

function sh(...args: string[]): string {
  return execFileSync(CELERIS_SH, args, { cwd: REPO_ROOT, stdio: "pipe" }).toString();
}

function apiGet<T>(pathAndQuery: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: CELERIS_API_HOST, port: CELERIS_API_PORT, path: `/api/v1${pathAndQuery}`, method: "GET", agent: false },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => {
          const status = res.statusCode ?? 0;
          if (status < 200 || status >= 300) {
            reject(new Error(`celeris ${pathAndQuery} responded ${status}`));
            return;
          }
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")) as T);
          } catch (e) {
            reject(e);
          }
        });
      },
    );
    req.on("error", reject);
    req.end();
  });
}

async function idOf(title: string): Promise<string> {
  const list = await apiGet<TaskList>(`/tasks?q=${encodeURIComponent(title)}&limit=500`);
  const item = list.items.find((i) => i.title === title);
  if (!item) throw new Error(`fixture task not found: ${title}`);
  return item.id;
}

let chainA1Id = "";

test.beforeAll(async () => {
  try {
    sh("stop", "dev");
  } catch {
    // 動いていなければ何もしない
  }
  try {
    sh("stop", "basic");
  } catch {
    // 動いていなければ何もしない
  }
  sh("fixture", "basic");
  sh("start", "basic");
  chainA1Id = await idOf("Chain-A1");
});

test.afterAll(() => {
  // 次の担当が引き継げるよう `basic` は動かしたまま終える（e2e/g1.spec.ts と同じ規約）。
  // ここでは何もしない。
});

function formatViolations(results: AxeResults, minImpact: "critical" | "serious"): string {
  const order: Record<string, number> = { critical: 4, serious: 3, moderate: 2, minor: 1 };
  const threshold = order[minImpact];
  const relevant = results.violations.filter((v) => order[v.impact ?? ""] >= threshold);
  return JSON.stringify(
    relevant.map((v) => ({
      id: v.id,
      impact: v.impact,
      help: v.help,
      nodes: v.nodes.map((n) => n.target),
    })),
    null,
    2,
  );
}

async function assertNoSeriousViolations(page: ConstructorParameters<typeof AxeBuilder>[0]["page"], pagePath: string) {
  const results = await new AxeBuilder({ page }).analyze();
  const gating = results.violations.filter((v) => v.impact === "critical" || v.impact === "serious");
  const minor = results.violations.filter((v) => v.impact === "moderate" || v.impact === "minor");
  for (const v of minor) {
    test.info().annotations.push({
      type: "a11y-minor",
      description: `${pagePath}: ${v.id} (${v.impact}) — ${v.help} [${v.nodes.map((n) => n.target).join(", ")}]`,
    });
  }
  expect(gating, `critical/serious a11y violations on ${pagePath}:\n${formatViolations(results, "serious")}`).toEqual(
    [],
  );
}

const PAGES: { path: () => string; label: string }[] = [
  { path: () => "/", label: "/" },
  { path: () => "/tasks", label: "/tasks" },
  { path: () => `/tasks/${chainA1Id}`, label: "/tasks/<id>" },
  { path: () => "/tasks/new", label: "/tasks/new" },
  { path: () => "/providers", label: "/providers" },
  { path: () => "/daemon", label: "/daemon" },
];

test.describe("受け入れ条件 3: 全ページの応答に CSP ヘッダ", () => {
  for (const { path: pathFn, label } of PAGES) {
    test(`${label} の応答に Content-Security-Policy と X-Content-Type-Options がある`, async ({ page }) => {
      const response = await page.goto(pathFn());
      expect(response).not.toBeNull();
      const headers = response?.headers() ?? {};
      expect(headers["content-security-policy"] ?? "").toContain("default-src 'self'");
      expect(headers["x-content-type-options"]).toBe("nosniff");
      await expect(page.locator("main")).toBeVisible();
    });
  }
});

test.describe("受け入れ条件 4: a11y（critical / serious が 0 件）", () => {
  for (const { path: pathFn, label } of PAGES) {
    test(`${label} に critical / serious の a11y violation が無い`, async ({ page }) => {
      await page.goto(pathFn());
      await expect(page.locator("main")).toBeVisible();
      await assertNoSeriousViolations(page, label);
    });
  }
});
