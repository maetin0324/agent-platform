import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { EventsPage } from "~/celeris/types";
import { expect, test } from "./test";

/**
 * Phase G13f-1（GUI 監査の対応）の結合テスト。SPEC §4 の 6 画面を、人が使う順で 1 本通す:
 * **秘書に投げる → 返事 → 案件 → 報告 → 認可 → 成果物**。
 *
 * **必ず別ポートで動かす**（`playwright.config.ts` の注意書き参照）。既定の 7700 / 7710 は人が使っている
 * 運用中の GUI / celeris なので、この spec は使い捨ての celeris（`scripts/celeris.sh fixture org`。fake アダプタ、
 * `config/org.example.toml` の組織）を別ポートに立てて使う:
 *
 *   cd gui
 *   scripts/celeris.sh build
 *   CELERIS_API_LISTEN=127.0.0.1:17971 scripts/celeris.sh fixture org   # 先に作る（GUI が起動時にトークンを読むため）
 *   CELERIS_GUI_BIND=127.0.0.1:17901 CELERIS_API_URL=http://127.0.0.1:17971 CELERIS_API_LISTEN=127.0.0.1:17971 \
 *     CELERIS_API_TOKEN_FILE="$(pwd)/.run/org/api.token" \
 *     pnpm exec playwright test e2e/g13.spec.ts
 *
 * fixture の作り直し・起動・停止はこのファイルが行う（`fixture org` → `start org` → 最後に `stop org`）。
 * `fixture org` はトークンを作り直さない（同じ値を保つ）ので、GUI が起動時に読んだトークンのまま通る。
 * LLM は呼ばない（fake ワーカー `test/celeris/fixtures/org-worker.sh` が決定的に返す）。
 */

const dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(dirname, "..");
const CELERIS_SH = path.join(REPO_ROOT, "scripts/celeris.sh");
const CELERIS_API_LISTEN = process.env.CELERIS_API_LISTEN ?? "127.0.0.1:17971";
const RUN_ROOT = process.env.CELERIS_RUN_ROOT ?? path.join(REPO_ROOT, ".run");

function sh(...args: string[]): string {
  return execFileSync(CELERIS_SH, args, {
    cwd: REPO_ROOT,
    stdio: "pipe",
    env: { ...process.env, CELERIS_API_LISTEN },
  }).toString();
}

function token(): string {
  return readFileSync(path.join(RUN_ROOT, "org", "api.token"), "utf8").trim();
}

async function api<T>(method: "GET" | "POST", pathAndQuery: string, body?: unknown): Promise<T> {
  const response = await fetch(`http://${CELERIS_API_LISTEN}/api/v1${pathAndQuery}`, {
    method,
    headers: {
      Authorization: `Bearer ${token()}`,
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`celeris ${method} ${pathAndQuery} responded ${response.status}`);
  return (await response.json()) as T;
}

/** 仕事を 1 件作って受け入れる（人が秘書に任せる代わりに、この e2e では直接置く）。 */
async function addTask(spec: {
  title: string;
  objective: string;
  assignee: string;
  projectId: string;
  workspace: string;
  acceptance: unknown[];
}): Promise<string> {
  const created = await api<{ id: string }>("POST", "/tasks", {
    title: spec.title,
    objective: spec.objective,
    assignee: spec.assignee,
    project_id: spec.projectId,
    workspace: spec.workspace,
    acceptance: spec.acceptance,
  });
  await api("POST", `/tasks/${created.id}/approve`, { expected_status: "draft" });
  return created.id;
}

/** 案件 id（最初のテストで秘書に投げて作る。以降のテストが使う）。 */
let projectId = "";

test.describe.configure({ mode: "serial" });

test.describe("Phase G13f-1: 秘書 → 案件 → 報告 → 認可 → 成果物", () => {
  test.beforeAll(() => {
    try {
      sh("stop", "org");
    } catch {
      // 動いていなければ何もしない
    }
    sh("fixture", "org");
    sh("start", "org");
  });

  test.afterAll(() => {
    sh("stop", "org");
  });

  test("/ は秘書（受信箱ではない）で、案件を投げると返事が入る", async ({ page }) => {
    await page.goto("/");
    await expect(page).toHaveURL(/\/org\/secretary$/);
    await expect(page.getByTestId("conversation")).toBeVisible();

    await page.getByTestId("conversation-input").fill("Pluvio を基盤に用いた新たな研究テーマの模索、検証");
    const newProject = page.getByTestId("conversation-new-project-toggle");
    if (!(await newProject.isChecked())) await newProject.check();
    await page.getByTestId("conversation-send").click();

    // 案件ができた状態（?project=...&waiting=1）へ移り、返事が入るまで待つ。
    await expect(page).toHaveURL(/\?project=[^&]+&waiting=1$/, { timeout: 30_000 });
    const nodeMessage = page.locator('[data-testid="conversation-message"][data-role="node"]');
    await expect(nodeMessage.first()).toContainText("理解の確認", { timeout: 60_000 });
    await expect(page.getByTestId("conversation-thinking")).toHaveCount(0);
    await expect(page.getByTestId("conversation-trouble")).toHaveCount(0);

    const url = new URL(page.url());
    projectId = url.searchParams.get("project") ?? "";
    expect(projectId).not.toBe("");

    // 受信箱は裏方に残っている（ナビから開ける）。
    await page.goto("/inbox");
    await expect(page.getByTestId("approvals-section")).toBeVisible();
  });

  test("案件の一覧・詳細が日本語の言葉で出て、仕事の木と担当への導線がある", async ({ page }) => {
    await addTask({
      title: "関連研究を調べる",
      objective: "Pluvio 周辺の関連研究を洗う",
      assignee: "research-survey",
      projectId,
      workspace: "ws-survey",
      acceptance: [{ type: "artifact_exists", name: "report.md" }],
    });

    await page.goto("/projects");
    const row = page.getByTestId("project-row").filter({ hasText: "Pluvio" });
    await expect(row).toHaveCount(1);
    // 状態は日本語（提案中 / 進行中 / 一時停止 / 完了）。
    await expect(row.getByTestId("project-status")).toHaveText(/提案中|進行中|一時停止|完了/);
    // フォームのラベルも日本語（題名・依頼）。
    await expect(page.getByTestId("project-new-form")).toContainText("題名");
    await expect(page.getByTestId("project-new-form")).toContainText("依頼");

    await page.goto(`/projects/${projectId}`);
    await expect(page.getByTestId("work-tree")).toBeVisible({ timeout: 30_000 });
    const assignees = page.getByTestId("work-tree-assignees");
    await expect(assignees).toContainText("関連研究を調べる");
    await expect(assignees.getByTestId("work-tree-talk").first()).toBeVisible();
    // 仕事の木に対話用タスク（「対話: …」）は出ない（裏方）。
    await expect(assignees).not.toContainText("対話:");
  });

  test("「この方針で進める」を押すと、秘書が担当付きの仕事に分解する（監査 H3）", async ({ page }) => {
    await page.goto(`/projects/${projectId}`);
    const before = await page.getByTestId("work-tree-assignees").locator("li").count();

    await page.getByTestId("project-plan-note").fill("急がなくてよい。まず関連研究から。");
    await page.getByTestId("project-plan-submit").click();

    // 202 なので待たない。「分解を秘書に頼みました」と、その裏方のタスクへのリンクが出る。
    await expect(page.getByTestId("flash-project-op")).toContainText("分解を秘書に頼みました");
    await expect(page.getByTestId("flash-project-plan-task")).toBeVisible();

    // 仕事の木は SSE の再検証で増えていく（偽のプランナーが担当付きの子を 3 件返す）。
    const rows = page.getByTestId("work-tree-assignees").locator("li");
    await expect(async () => {
      expect(await rows.count()).toBeGreaterThanOrEqual(before + 3);
    }).toPass({ timeout: 60_000 });
    const list = page.getByTestId("work-tree-assignees");
    await expect(list).toContainText("関連研究を洗い出す");
    await expect(list).toContainText("担当: 関連研究調査課");
    await expect(list).toContainText("担当: 論文執筆課");
    // 承認待ち・レビューのような裏方のタスクは木にも一覧にも出ない（`support`。Phase 29）。
    await expect(list).not.toContainText("Approval needed");
    // 案件は「提案中」から「進行中」になる（celeris が変える）。
    await expect(page.getByTestId("project-status")).toHaveText("進行中");
  });

  test("担当の記憶（案件をまたぐ / この案件の引き出し）が読める（監査 M4）", async ({ page }) => {
    await page.goto("/org?selected=secretary");
    const memory = page.getByTestId("org-node-memory");
    await expect(memory).toBeVisible();
    // この fixture は `[memory]` を設定しているので「設定されていません」は出ない。
    await expect(page.getByTestId("org-node-memory-unavailable")).toHaveCount(0);
    await expect(memory).toContainText("直すならこのファイル");
    // 案件を選ぶと「この案件の引き出し」を読む。
    await memory.getByTestId("org-node-memory-project").selectOption(projectId);
    await memory.getByRole("button", { name: "表示" }).click();
    await expect(page).toHaveURL(new RegExp(`project=${projectId}`));
    await expect(page.getByTestId("org-node-memory")).toContainText("この案件の引き出し");
  });

  test("受信箱の質問は「認可」の画面へ送る（監査 H2）", async ({ page }) => {
    // 人に聞いて止まる仕事を 2 件作る（同じ文面の質問になる。次の「認可」のテストでも使う）。
    await addTask({
      title: "実験の投入先を聞く",
      objective: "クラスタへの投入可否を人に聞く",
      assignee: "coding-poc",
      projectId,
      workspace: "ws-ask-1",
      acceptance: [{ type: "human", text: "人が確認する" }],
    });
    await addTask({
      title: "別の実験の投入先を聞く",
      objective: "クラスタへの投入可否を人に聞く",
      assignee: "research-survey",
      projectId,
      workspace: "ws-ask-2",
      acceptance: [{ type: "human", text: "人が確認する" }],
    });

    await page.goto("/inbox");
    const question = page.getByTestId("question-item").first();
    await expect(question).toBeVisible({ timeout: 60_000 });
    const link = question.getByTestId("question-approval-link");
    await expect(link).toBeVisible();
    await expect(link).toHaveAttribute("href", /\/approvals#approval-/);
    // 受信箱に回答欄は出さない（回答は認可画面に一本化する）。
    await expect(question.getByTestId("question-answer")).toHaveCount(0);
  });

  test("組織の木は名前を先頭に太く出し、英語のバッジを出さない", async ({ page }) => {
    await page.goto("/org");
    const secretary = page.locator('[data-testid="org-node"][data-org-id="secretary"]');
    await expect(secretary.getByTestId("org-node-name")).toHaveText("秘書");
    await expect(secretary.getByTestId("org-node-brief")).toBeVisible();
    // 英語の `kind` バッジ（secretary / department / section）は出さない。
    // 右端に小さく出るのは `genre` の id だけ（日本語の分野名が無いため）。
    await expect(secretary).not.toContainText("department");
    await expect(secretary).not.toContainText("section");
    const coding = page.locator('[data-testid="org-node"][data-org-id="coding"]');
    await expect(coding.getByTestId("org-node-name")).toHaveText("コーディング部");
    await expect(coding).not.toContainText("department");
    const frontend = page.locator('[data-testid="org-node"][data-org-id="coding-frontend"]');
    await expect(frontend).not.toContainText("section");
    await expect(frontend.getByTestId("org-node-genre")).toHaveText("coding");
    // 担当を選ぶと詳細が出て、その担当に話せる。
    await page.goto("/org?selected=research-survey");
    await expect(page.getByTestId("org-node-detail")).toContainText("関連研究調査課");
    await expect(page.getByTestId("org-talk")).toBeVisible();
  });

  test("報告は Markdown で描かれ、案件・担当・裏方のタスク・成果物へ辿れる", async ({ page }) => {
    // 課の報告（level=2）まで出す。既定は秘書レベルの未読だけ。
    await page.goto("/reports?filter=all&level=");
    const row = page.getByTestId("report-row").filter({ hasText: "関連研究" }).first();
    await expect(row).toBeVisible({ timeout: 60_000 });
    await row.getByRole("button").first().click();
    await expect(row.getByTestId("report-body")).toBeVisible();
    await expect(row.getByTestId("report-project-link")).toHaveAttribute("href", `/projects/${projectId}`);
    await expect(row.getByTestId("report-talk-link")).toBeVisible();
    await expect(row.getByTestId("report-artifacts-link")).toHaveAttribute("href", `/artifacts?project=${projectId}`);
    // 内情の説明（celeris に転送するのは…）は画面から消えている。
    await expect(page.getByTestId("reports-filter-form")).not.toContainText("celeris");
  });

  test("認可は同じ文面をまとめて 1 枚にし、答えると履歴に移る", async ({ page }) => {
    // 質問は前のテスト（監査 H2）で作った 2 件（同じ文面）。
    await page.goto("/approvals");
    const card = page.getByTestId("approval-row").first();
    await expect(card).toBeVisible({ timeout: 60_000 });
    await expect(card.getByTestId("approval-question")).toContainText("pegasus");
    // 同じ文面の 2 件は 1 枚にまとまり「N 件」が出る。
    await expect(card.getByTestId("approval-count")).toContainText("2 件", { timeout: 60_000 });

    await card.getByTestId("approval-answer").fill("今回は pegasus で進めてよいです");
    await card.getByTestId("approval-once").click();
    // 答えた 2 件はまとめて決まり、「決めたもの」の履歴に移る（その行は決定と答えを読み取り専用で出す）。
    await expect(page.getByTestId("approvals-section")).toContainText("今回だけ", { timeout: 30_000 });
    await expect(page.getByTestId("approvals-section")).toContainText("今回は pegasus で進めてよいです", {
      timeout: 30_000,
    });
  });

  test("成果物は案件ごとに読める（Markdown とリンク集）", async ({ page }) => {
    await page.goto(`/artifacts?project=${projectId}`);
    await expect(page.getByTestId("artifacts-section")).toContainText("report.md", { timeout: 60_000 });
    await expect(page.getByTestId("artifacts-section")).toContainText("sources.json");
  });

  test("操作の失敗は消えない（監査 H1: SSE の再検証で actionData が捨てられない）", async ({ page }) => {
    await page.goto("/projects");
    await page.getByTestId("project-title").fill("");
    await page.getByTestId("project-request").fill("");
    await page.getByTestId("project-new-submit").click();

    const flash = page.locator('[data-testid="flash"][data-flash-kind="error"]');
    await expect(flash).toBeVisible();
    // celeris の tick（200ms）ごとに SSE の daemon イベントが届くので、3 秒待っても消えないことを見る。
    await page.waitForTimeout(3_000);
    await expect(flash).toBeVisible();
  });

  test("裏方のタスクから案件・担当へ戻れる（監査 M2）", async ({ page }) => {
    await page.goto("/tasks?q=関連研究");
    const row = page.getByTestId("task-row").filter({ hasText: "関連研究を調べる" }).first();
    await expect(row).toBeVisible({ timeout: 30_000 });
    await expect(row.getByTestId("task-project")).toContainText("Pluvio");
    await expect(row.getByTestId("task-assignee")).toContainText("関連研究調査課");

    await row.getByRole("link").first().click();
    await expect(page.getByTestId("task-place").getByTestId("task-project-link")).toHaveAttribute(
      "href",
      `/projects/${projectId}`,
    );
    await expect(page.getByTestId("task-place").getByTestId("task-assignee-link")).toHaveAttribute(
      "href",
      "/org/research-survey",
    );
  });

  // Phase G13h（ADR-0033 追記、実機の事故 2026-09-18）: 失敗した仕事を人が一手でやり直せる。
  test("失敗 → やり直す → Go → 動く（Phase 31）", async ({ page }) => {
    const failedId = await addTask({
      title: "Fail-G13h",
      objective: "LLM 先の停止を再現する（1 回目は必ず失敗する fake ワーカー）",
      assignee: "coding-poc",
      projectId,
      workspace: "ws-g13h-retry",
      acceptance: [{ type: "command", cmd: "true", expect_exit: 0 }],
    });

    // 失敗: 1 回目の run は `Fail-G13h` タイトルの分岐で決定的に非リトライ失敗する。
    await page.goto(`/tasks/${failedId}`);
    await expect(page.getByTestId("task-status")).toHaveText("failed", { timeout: 30_000 });

    // やり直す: accept は付けない（新しいタスクは draft から始まる）。
    await page.getByTestId("action-retry").click();
    await page.waitForURL(
      (url) => /^\/tasks\/[0-9A-HJKMNP-TV-Z]{26}$/.test(url.pathname) && !url.pathname.endsWith(failedId),
      {
        timeout: 30_000,
      },
    );
    const retriedId = page.url().split("/").pop() ?? "";
    expect(retriedId).not.toBe(failedId);
    await expect(page.getByTestId("task-status")).toHaveText("draft");

    // 新しいタスクは `Event::Retried{from}` を持つ（複製の記録）。
    const events = await api<EventsPage>("GET", `/tasks/${retriedId}/events`);
    expect(events.items.some((row) => row.event.type === "retried" && row.event.from === failedId)).toBe(true);

    // Go: draft → ready（既存の `POST /tasks/{id}/approve`）。
    await page.getByTestId("action-approve").click();
    await expect(page.getByTestId("task-status")).toHaveText(/ready|running|reviewing|done/, { timeout: 10_000 });

    // 動く: やり直した run はワークスペースを複製した元のタスクと共有するので、
    // fake ワーカーはマーカーファイルを見て 2 回目は成功する（= 実機で LLM 先が復旧した後の再現）。
    await expect(page.getByTestId("task-status")).toHaveText("done", { timeout: 30_000 });
  });
});
