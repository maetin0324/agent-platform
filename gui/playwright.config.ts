import { defineConfig, devices } from "@playwright/test";

// 結合テスト。scripts/taskd.sh が起動した実 taskd（fake ワーカー、既定 127.0.0.1:7710）に対して、
// pnpm build 済みの server.js（既定 127.0.0.1:7700）を Playwright が起動して行う。全て loopback。
//
// **注意（Phase G13f-1）: 既定の 7700 / 7710 は人が普段使っている運用中の GUI / taskd を掴む。**
// e2e を回すときは必ず別ポートを環境変数で指定すること（運用中のデータベースやタスクに触らないため）:
//
//   cd gui
//   TASKD_API_LISTEN=127.0.0.1:17971 scripts/taskd.sh build
//   TASKD_GUI_BIND=127.0.0.1:17901 TASKD_API_URL=http://127.0.0.1:17971 TASKD_API_LISTEN=127.0.0.1:17971 \
//     TASKD_API_TOKEN_FILE="$(pwd)/.run/org/api.token" \
//     pnpm exec playwright test e2e/g13.spec.ts
//
// - `TASKD_GUI_BIND`: この設定が起動する GUI の待ち受け（`baseURL` もこれに追従する）。
// - `TASKD_API_URL`: GUI から見た taskd の場所。
// - `TASKD_API_LISTEN`: `scripts/taskd.sh` が起動する taskd の待ち受け（spec 側が使う）。
// - `TASKD_API_TOKEN_FILE`: 管理系 API（案件の作成・対話・認可など）に要るトークン。
//   `webServer.env` は `process.env` に**上書きで足される**ので、ここに書かない環境変数もそのまま GUI に渡る。
const GUI_BIND = process.env.TASKD_GUI_BIND ?? "127.0.0.1:7700";
const TASKD_API_URL = process.env.TASKD_API_URL ?? "http://127.0.0.1:7710";

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
      TASKD_API_URL,
      TASKD_GUI_BIND: GUI_BIND,
    },
  },
});
