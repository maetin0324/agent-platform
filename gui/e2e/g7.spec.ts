import { execFileSync } from "node:child_process";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { AxeBuilder } from "@axe-core/playwright";
import type { TaskDetail, TaskList } from "~/taskd/types";
import { expect, test } from "./test";

// Phase G7 の受け入れ条件 1〜6（docs/DESIGN.md §10 Phase G7、docs/adr/0010-g7-decisions.md）。
// `clusters` フィクスチャ（ADR-0010 D1/D2、実際に localhost への ssh 多重接続を使う）と `delegation`
// フィクスチャ（ADR-0010 D3）に対して検証する。受け入れ条件 7（lint/typecheck/test/build/e2e/gen:types）は
// このファイルではなく `pnpm` の各コマンドと `docs/PROGRESS.md` の証拠で扱う。

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const TASKD_SH = path.join(REPO_ROOT, "scripts/taskd.sh");
const TASKD_API_HOST = "127.0.0.1";
const TASKD_API_PORT = 7710;
const INSTANCE_NAMES = ["dev", "basic", "clusters", "delegation"] as const;

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

function apiGet<T>(pathAndQuery: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: TASKD_API_HOST, port: TASKD_API_PORT, path: `/api/v1${pathAndQuery}`, method: "GET", agent: false },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => {
          const status = res.statusCode ?? 0;
          if (status < 200 || status >= 300) {
            reject(new Error(`taskd ${pathAndQuery} responded ${status}`));
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

test.describe("受け入れ条件 1・2・3・6: /clusters、受信箱の cluster_unavailable、Remote タスク詳細", () => {
  test.beforeAll(() => {
    stopAll();
    sh("fixture", "clusters");
    sh("start", "clusters");
  });

  test.afterAll(() => {
    sh("stop", "clusters");
  });

  test("/clusters が 200 で、fixture の 2 クラスタ（接続あり/無し）が出て、connected: false の行にだけログイン案内が出る", async ({
    page,
  }) => {
    const response = await page.goto("/clusters");
    expect(response?.status()).toBe(200);
    await expect(page.getByTestId("clusters-section")).toBeVisible();

    const rows = page.getByTestId("cluster-row");
    await expect(rows).toHaveCount(2);

    const local = page.locator('[data-testid="cluster-row"][data-cluster-id="local"]');
    await expect(local.getByTestId("cluster-host")).toHaveText("taskd-localhost");
    await expect(local.getByTestId("cluster-connected")).toHaveText("connected");
    await expect(local.getByTestId("cluster-login-hint")).toHaveCount(0);

    const offline = page.locator('[data-testid="cluster-row"][data-cluster-id="offline"]');
    await expect(offline.getByTestId("cluster-host")).toHaveText("taskd-no-such-host-for-tests");
    await expect(offline.getByTestId("cluster-connected")).toHaveText("disconnected");
    await expect(offline.getByTestId("cluster-login-hint")).toContainText(
      "scripts/cluster-login.sh taskd-no-such-host-for-tests",
    );
  });

  test("受信箱に cluster_unavailable の項目が出て、押すと /clusters に遷移する", async ({ page }) => {
    await page.goto("/");
    const item = page.locator('[data-testid="attention-item"][data-attention-type="cluster_unavailable"]');
    await expect(item).toHaveCount(1);
    await item.getByTestId("attention-cluster-link").click();
    await expect(page).toHaveURL(/\/clusters$/);
  });

  test("Remote のタスクの詳細に cluster と写しの注記が出て、run のログが開ける", async ({ page }) => {
    const id = await idOf("Cluster-Local");
    await page.goto(`/tasks/${id}`);
    await expect(page.getByTestId("task-status")).toHaveText("done");
    await expect(page.getByTestId("task-cluster")).toContainText("local");
    await expect(page.getByTestId("task-workspace-note")).toBeVisible();

    await page.getByTestId("run-log-link").first().click();
    await expect(page).toHaveURL(/\/tasks\/.+\/runs\/.+$/);
    await expect(page.getByTestId("stdout-line").first()).toContainText("used the cluster file");
  });

  test("/clusters の a11y: critical / serious な violation が無い", async ({ page }) => {
    await page.goto("/clusters");
    const results = await new AxeBuilder({ page }).analyze();
    const gating = results.violations.filter((v) => v.impact === "critical" || v.impact === "serious");
    expect(gating, JSON.stringify(gating, null, 2)).toEqual([]);
  });
});

test.describe("受け入れ条件 4・5: 委譲のあるタスクの詳細、作成フォームの role / aggregate", () => {
  test.beforeAll(() => {
    stopAll();
    sh("fixture", "delegation");
    sh("start", "delegation");
  });

  test.afterAll(() => {
    sh("stop", "delegation");
  });

  test("委譲のあるタスクの詳細に role と delegated[] が出て、子のリンクから子の詳細に飛べる", async ({ page }) => {
    const id = await idOf("Lead-Delegator");
    await page.goto(`/tasks/${id}`);
    await expect(page.getByTestId("task-status")).toHaveText("done");
    await expect(page.getByTestId("task-role")).toHaveText("lead");

    const groups = page.getByTestId("delegated-group");
    await expect(groups).toHaveCount(1);
    const childLinks = groups.first().getByTestId("delegated-child-link");
    await expect(childLinks).toHaveCount(2);
    await expect(childLinks.first()).toHaveText("Delegated-Child-1");

    await childLinks.first().click();
    await expect(page).toHaveURL(/\/tasks\/(?!.*Lead).+$/);
    await expect(page.getByTestId("task-title")).toHaveText("Delegated-Child-1");
  });

  test("一覧の行と DAG のノードに役割のラベルが出る（taskd-requests R2 対応後）", async ({ page }) => {
    // 一覧: TaskSummary.role をそのまま出す（追加の GET /tasks/{id} は呼ばない）。
    await page.goto("/tasks?q=Lead-Delegator");
    const leadRow = page.getByTestId("task-row").filter({ hasText: "Lead-Delegator" });
    await expect(leadRow.getByTestId("task-role")).toHaveText("lead");
    await page.goto("/tasks?q=Delegated-Child-1");
    const childRow = page.getByTestId("task-row").filter({ hasText: "Delegated-Child-1" });
    await expect(childRow.getByTestId("task-role")).toHaveText("implementer");

    // DAG: ノードのラベル 2 行目に役割（GraphNode.role）。
    const leadId = await idOf("Lead-Delegator");
    await page.goto(`/graph?root=${leadId}`);
    await expect(page.getByTestId("graph-canvas")).toBeVisible();
    const leadNode = page.locator(`.react-flow__node[data-id="${leadId}"]`);
    await expect(leadNode).toContainText("Lead-Delegator");
    await expect(leadNode).toContainText("[lead]");
    await expect(page.locator(".react-flow__node").filter({ hasText: "Delegated-Child-1" })).toContainText(
      "[implementer]",
    );
  });

  test("作成フォームで role と aggregate を指定すると POST /tasks の本文にそれが載る（空欄なら送らない）", async ({
    page,
  }) => {
    await page.goto("/tasks/new");
    await page.fill("#title", "G7-Role-Task");
    await page.fill("#objective", "created from the G7 e2e test");
    await page.getByTestId("criterion-row").getByRole("combobox").selectOption("command");
    await page.getByTestId("criterion-row").getByPlaceholder("cargo test").fill("true");
    await page.fill("#role", "reviewer-custom");
    await page.getByTestId("aggregate-checkbox").check();
    await page.getByTestId("submit").click();
    await expect(page).toHaveURL(/\/tasks\/[0-9A-Z]+$/);

    const id = /\/tasks\/([0-9A-Z]+)$/.exec(page.url())?.[1];
    if (!id) throw new Error("no task id in URL after create");
    const detail = await apiGet<TaskDetail>(`/tasks/${id}`);
    expect(detail.role).toBe("reviewer-custom");
    expect(detail.task.aggregate).toBe(true);

    await page.goto("/tasks/new");
    await page.fill("#title", "G7-No-Role-Task");
    await page.fill("#objective", "created without role/aggregate");
    await page.getByTestId("criterion-row").getByRole("combobox").selectOption("command");
    await page.getByTestId("criterion-row").getByPlaceholder("cargo test").fill("true");
    await page.getByTestId("submit").click();
    await expect(page).toHaveURL(/\/tasks\/[0-9A-Z]+$/);

    const id2 = /\/tasks\/([0-9A-Z]+)$/.exec(page.url())?.[1];
    if (!id2) throw new Error("no task id in URL after create");
    const detail2 = await apiGet<TaskDetail>(`/tasks/${id2}`);
    expect(detail2.role ?? null).toBeNull();
    expect(detail2.task.aggregate ?? false).toBe(false);
  });
});
