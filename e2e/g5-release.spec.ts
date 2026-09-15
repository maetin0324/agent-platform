import { type ChildProcess, execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "./test";

// Phase G5 受け入れ条件 5（後半）: `pnpm release` が作る配布物を、空のディレクトリに展開して
// `pnpm install --prod --frozen-lockfile --ignore-scripts` するだけで `node server.js` が動くことを確認する。
// docs/adr/0008-g5-decisions.md D9。--offline を使うため、このテストは外部ネットワークに出ない。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");
const TASKD_API_URL = process.env.TASKD_API_URL ?? "http://127.0.0.1:7710";
const RELEASE_PORT = 7703;

const pkg = JSON.parse(readFileSync(path.join(REPO_ROOT, "package.json"), "utf8")) as { version: string };
const VERSION = pkg.version;
const STAGE_NAME = `taskd-gui-${VERSION}`;
const TARBALL = path.join(REPO_ROOT, "dist", `${STAGE_NAME}.tar.gz`);

let extractDir: string | undefined;
let server: ChildProcess | undefined;

async function taskdIsUp(): Promise<boolean> {
  try {
    const res = await fetch(`${TASKD_API_URL}/api/v1/health`);
    return res.ok;
  } catch {
    return false;
  }
}

test.beforeAll(async () => {
  // pnpm build（release 内）+ tar 展開 + pnpm install --offline（このホストでは $HOME が NFS 越しで
  // 数千ファイルの整合性検証に数十秒〜1分以上かかることがある）を直列に行うため、既定の 60s では足りない。
  test.setTimeout(480_000);

  // このスモークは taskd に接続できなくても `/` が 200 を返す設計だが、可能なら実 taskd を立てておく
  // （taskd が居ない環境での偶然の成功と区別するため）。失敗しても続行する。
  if (!(await taskdIsUp())) {
    try {
      execFileSync(TASKD_SH, ["start", "dev"], { cwd: REPO_ROOT, stdio: "pipe" });
    } catch {
      // 起動できなくても続行する（受け入れ条件はこの taskd の有無に依存しない）
    }
  }

  execFileSync("pnpm", ["release"], { cwd: REPO_ROOT, stdio: "pipe", timeout: 240_000 });

  expect(existsSync(TARBALL)).toBe(true);

  // 展開先は dist/（gitignore 済み）配下に作る。pnpm のコンテンツアドレス store はファイルシステムごとに
  // 分かれるため（このホストでは $HOME が NFS、/tmp がローカル ext4）、os.tmpdir() 配下に展開すると
  // --offline install がまだ何もキャッシュされていない store を見て失敗する。dist/ はリポジトリと同じ
  // ファイルシステム上にあるので、リポジトリの pnpm install で既に温まっている store をそのまま使える。
  const distDir = path.join(REPO_ROOT, "dist");
  mkdirSync(distDir, { recursive: true });
  extractDir = mkdtempSync(path.join(distDir, "release-smoke-"));
  execFileSync("tar", ["xzf", TARBALL], { cwd: extractDir, stdio: "pipe" });

  const stageDir = path.join(extractDir, STAGE_NAME);
  execFileSync("pnpm", ["install", "--prod", "--frozen-lockfile", "--ignore-scripts", "--offline"], {
    cwd: stageDir,
    stdio: "pipe",
  });

  server = spawn("node", ["server.js"], {
    cwd: stageDir,
    env: {
      ...process.env,
      TASKD_GUI_BIND: `127.0.0.1:${RELEASE_PORT}`,
      TASKD_API_URL,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  await new Promise<void>((resolve, reject) => {
    let stderr = "";
    const timer = setTimeout(() => {
      reject(new Error(`server.js did not report listening within 20s; stderr so far:\n${stderr}`));
    }, 20_000);
    const onData = (chunk: Buffer) => {
      stderr += chunk.toString("utf8");
      if (stderr.includes("listening on")) {
        clearTimeout(timer);
        server?.stdout?.off("data", onData);
        server?.stderr?.off("data", onData);
        resolve();
      }
    };
    server?.stdout?.on("data", onData);
    server?.stderr?.on("data", onData);
    server?.on("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`server.js exited early (code ${code}); stderr so far:\n${stderr}`));
    });
  });
});

test.afterAll(() => {
  if (server && !server.killed) {
    server.kill("SIGTERM");
  }
  if (extractDir) {
    rmSync(extractDir, { recursive: true, force: true });
  }
});

test.describe("Phase G5 受け入れ条件 5: pnpm release の配布物", () => {
  test("展開した配布物を pnpm install --prod --offline するだけで node server.js が動く", async ({ page }) => {
    const response = await page.goto(`http://127.0.0.1:${RELEASE_PORT}/`);
    expect(response?.status()).toBe(200);

    const footer = page.getByTestId("footer");
    await expect(footer).toContainText(`taskd-gui ${VERSION}`);
  });
});
