import { defineConfig, devices } from "@playwright/test";

// 結合テスト。scripts/taskd.sh が起動した実 taskd（fake ワーカー、127.0.0.1:7710）に対して、
// pnpm build 済みの server.js（127.0.0.1:7700）を Playwright が起動して行う。全て loopback。
const GUI_BIND = process.env.TASKD_GUI_BIND ?? "127.0.0.1:7700";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  timeout: 60_000,
  use: {
    baseURL: `http://${GUI_BIND}`,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "pnpm build && node server.js",
    url: `http://${GUI_BIND}/healthz`,
    reuseExistingServer: false,
    timeout: 120_000,
    env: {
      TASKD_API_URL: process.env.TASKD_API_URL ?? "http://127.0.0.1:7710",
      TASKD_GUI_BIND: GUI_BIND,
    },
  },
});
