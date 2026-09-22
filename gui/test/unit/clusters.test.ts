import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import {
  cancelClusterConnect,
  putClusterSettings,
  readClusterWorkDir,
  startClusterConnect,
  submitClusterConnectCode,
} from "~/celeris/clusters-admin.server";
import type { ClusterConnectResult, ClusterConnectStart, ClusterSettingsView, Clusters } from "~/celeris/types";
import {
  clusterConnectPanelState,
  clusterStatusWord,
  clusterWorkDirWord,
  loadClusters,
  showClusterWorkDirClear,
} from "~/routes/clusters";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

const clustersView: Clusters = {
  items: [
    {
      id: "gpu-a",
      host: "gpu-a.example",
      concurrency: 2,
      sync: "rsync",
      delete_on_push: false,
      has_setup: true,
      env_keys: ["API_KEY"],
      rsync_excludes: [".git"],
      in_use: 1,
      connected: true,
      cooldown_until: null,
      cooldown_remaining_secs: null,
      auth: "publickey",
      connect_pending: false,
      // ADR-0059 D6（Phase 99/100）: DB の上書き（画面から登録）。
      work_dir: "/work/NBB/rmaeda",
      work_dir_source: "settings",
    },
    {
      id: "gpu-b",
      host: "gpu-b.example",
      concurrency: 1,
      sync: "none",
      delete_on_push: true,
      has_setup: false,
      env_keys: [],
      rsync_excludes: [],
      in_use: 0,
      connected: false,
      cooldown_until: "2026-09-15T00:05:00Z",
      cooldown_remaining_secs: 300,
      auth: "totp",
      connect_pending: true,
      // 作業ディレクトリが未登録（設定ファイルにも DB にも無い）。
      work_dir: null,
      work_dir_source: null,
    },
  ],
};

describe("loadClusters", () => {
  it("calls GET /clusters and returns {clusters} as-is", async () => {
    mock.on("GET", "/api/v1/clusters", (_req, res) => {
      sendJson(res, 200, clustersView);
    });

    const result = await loadClusters(client, new Request("http://gui.invalid/clusters"));

    expect(result.clusters).toEqual(clustersView);
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/clusters")).toBe(true);
  });

  it("passes auth and connect_pending through unmodified (ADR-0032 D1/D5)", async () => {
    mock.on("GET", "/api/v1/clusters", (_req, res) => sendJson(res, 200, clustersView));

    const result = await loadClusters(client, new Request("http://gui.invalid/clusters"));

    expect(result.clusters.items[0]).toMatchObject({ auth: "publickey", connect_pending: false });
    expect(result.clusters.items[1]).toMatchObject({ auth: "totp", connect_pending: true });
  });

  it("passes work_dir and work_dir_source through unmodified (ADR-0059 D6)", async () => {
    mock.on("GET", "/api/v1/clusters", (_req, res) => sendJson(res, 200, clustersView));

    const result = await loadClusters(client, new Request("http://gui.invalid/clusters"));

    expect(result.clusters.items[0]).toMatchObject({ work_dir: "/work/NBB/rmaeda", work_dir_source: "settings" });
    expect(result.clusters.items[1]).toMatchObject({ work_dir: null, work_dir_source: null });
  });

  it("rejects when celeris is not reachable (loader converts this to a Response)", async () => {
    const closed = await startMockCeleris();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new CelerisClient({ baseUrl, timeoutMs: 1000 });

    await expect(loadClusters(unreachable, new Request("http://gui.invalid/clusters"))).rejects.toBeTruthy();
  });
});

/**
 * クラスタへの接続の中継（ADR-0032、docs/celeris-api-v1.md §3.39〜3.41）。`accounts-admin.test.ts` と同じ流儀:
 * celeris の応答をそのまま素通しし、エラーは `ActionError` として返す（例外にしない）。`POST /reload` は
 * 一切呼ばない（接続を張っても設定は変わらないため。`secrets-admin.test.ts` の reload 呼び出しとの対比）。
 */
describe("startClusterConnect", () => {
  it("passes through kind: connected as-is", async () => {
    const start: ClusterConnectStart = { kind: "connected", prompt: null, expires_at: null };
    mock.on("POST", "/api/v1/clusters/gpu-a/connect", (_req, res) => sendJson(res, 200, start));

    const result = await startClusterConnect(client, "gpu-a");

    expect(result).toEqual({ ok: true, op: "connect_start", id: "gpu-a", start });
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });

  it("passes through kind: needs_code as-is, including the prompt", async () => {
    const start: ClusterConnectStart = {
      kind: "needs_code",
      prompt: "(rmaeda@130.158.241.2) Verification code: ",
      expires_at: "2026-09-17T01:35:00Z",
    };
    mock.on("POST", "/api/v1/clusters/gpu-b/connect", (_req, res) => sendJson(res, 200, start));

    const result = await startClusterConnect(client, "gpu-b");

    expect(result).toEqual({ ok: true, op: "connect_start", id: "gpu-b", start });
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });

  it("401 unauthorized (no token configured for a management endpoint) is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/clusters/gpu-a/connect", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await startClusterConnect(client, "gpu-a");

    expect(result).toEqual({
      ok: false,
      op: "connect_start",
      id: "gpu-a",
      error: expect.objectContaining({ status: 401, code: "unauthorized" }) as unknown,
    });
  });

  it("404 cluster_not_found is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/clusters/unknown/connect", (_req, res) =>
      sendProblem(res, { status: 404, code: "cluster_not_found", detail: "no such cluster" }),
    );

    const result = await startClusterConnect(client, "unknown");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 404, code: "cluster_not_found" });
  });

  it("409 cluster_connect_not_supported (auth = manual) is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/clusters/legacy/connect", (_req, res) =>
      sendProblem(res, { status: 409, code: "cluster_connect_not_supported", detail: "auth is manual" }),
    );

    const result = await startClusterConnect(client, "legacy");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 409, code: "cluster_connect_not_supported" });
  });

  it("502 cluster_connect_failed (ssh failed) is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/clusters/gpu-a/connect", (_req, res) =>
      sendProblem(res, { status: 502, code: "cluster_connect_failed", detail: "ssh exited with status 255" }),
    );

    const result = await startClusterConnect(client, "gpu-a");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 502, code: "cluster_connect_failed" });
  });
});

describe("submitClusterConnectCode", () => {
  it("does not swallow ok: false (a rejected code is still a 200 with ok: false, per celeris 3.40)", async () => {
    const result_: ClusterConnectResult = { ok: false, detail: "verification failed" };
    mock.on("POST", "/api/v1/clusters/gpu-b/connect/code", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ code: "000000" });
      sendJson(res, 200, result_);
    });

    const result = await submitClusterConnectCode(client, "gpu-b", "000000");

    expect(result).toEqual({ ok: true, op: "connect_code", id: "gpu-b", result: result_ });
    // 成功でも失敗でも、コードそのものはどこにも残らない。
    expect(JSON.stringify(result)).not.toContain("000000");
  });

  it("passes through ok: true as-is", async () => {
    const result_: ClusterConnectResult = { ok: true, detail: null };
    mock.on("POST", "/api/v1/clusters/gpu-b/connect/code", (_req, res) => sendJson(res, 200, result_));

    const result = await submitClusterConnectCode(client, "gpu-b", "123456");

    expect(result).toEqual({ ok: true, op: "connect_code", id: "gpu-b", result: result_ });
  });

  it("422 validation (blank/control-character code) is returned as an ActionError, not thrown", async () => {
    mock.on("POST", "/api/v1/clusters/gpu-b/connect/code", (_req, res) =>
      sendProblem(res, { status: 422, code: "validation", detail: "code must not be blank" }),
    );

    const result = await submitClusterConnectCode(client, "gpu-b", "   ");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 422, code: "validation" });
  });

  it("409 cluster_connect_not_started (no session) is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/clusters/gpu-b/connect/code", (_req, res) =>
      sendProblem(res, { status: 409, code: "cluster_connect_not_started", detail: "no connect session" }),
    );

    const result = await submitClusterConnectCode(client, "gpu-b", "123456");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 409, code: "cluster_connect_not_started" });
  });

  it("401 unauthorized is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/clusters/gpu-b/connect/code", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await submitClusterConnectCode(client, "gpu-b", "123456");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 401, code: "unauthorized" });
  });

  it("does not call POST /reload", async () => {
    mock.on("POST", "/api/v1/clusters/gpu-b/connect/code", (_req, res) =>
      sendJson(res, 200, { ok: true, detail: null }),
    );

    await submitClusterConnectCode(client, "gpu-b", "123456");

    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });
});

describe("cancelClusterConnect", () => {
  it("DELETEs /clusters/{id}/connect and does not call /reload", async () => {
    mock.on("DELETE", "/api/v1/clusters/gpu-b/connect", (_req, res) => sendJson(res, 200, {}));

    const result = await cancelClusterConnect(client, "gpu-b");

    expect(result).toEqual({ ok: true, op: "connect_cancel", id: "gpu-b" });
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });

  it("404 cluster_not_found is returned as an ActionError", async () => {
    mock.on("DELETE", "/api/v1/clusters/unknown/connect", (_req, res) =>
      sendProblem(res, { status: 404, code: "cluster_not_found", detail: "no such cluster" }),
    );

    const result = await cancelClusterConnect(client, "unknown");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 404, code: "cluster_not_found" });
  });

  it("401 unauthorized is returned as an ActionError", async () => {
    mock.on("DELETE", "/api/v1/clusters/gpu-b/connect", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await cancelClusterConnect(client, "gpu-b");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 401, code: "unauthorized" });
  });
});

/**
 * ADR-0032 D6 の回帰: 実機で「接続処理が進行中です」から抜け出せなくなった（入力欄も接続ボタンも
 * 出ず、取り消すしか道が無い）。原因は `showConnectButton` が `!showPendingElsewhere` を条件にしていたこと。
 * `/accounts` のログインは同じ問題を既に直してある（「ログインをやり直す」）ので、同じ扱いに揃える。
 */
describe("clusterConnectPanelState", () => {
  const base = {
    connected: false,
    auth: "totp" as const,
    connectPending: false,
    needsCode: false,
    hasCodeResult: false,
  };

  it("進行中でも接続ボタンを出す（押し直せば入力欄に戻れる）", () => {
    const state = clusterConnectPanelState({ ...base, connectPending: true });
    expect(state.showPendingElsewhere).toBe(true);
    expect(state.showConnectButton).toBe(true);
    expect(state.showCodeForm).toBe(false);
  });

  it("このタブで開始して needs_code なら入力欄を出し、進行中の通知は出さない", () => {
    const state = clusterConnectPanelState({ ...base, connectPending: true, needsCode: true });
    expect(state.showCodeForm).toBe(true);
    expect(state.showPendingElsewhere).toBe(false);
    expect(state.showConnectButton).toBe(false);
  });

  it("コードの送信結果が付いたら入力欄を閉じ、もう一度接続できる", () => {
    const state = clusterConnectPanelState({ ...base, needsCode: true, hasCodeResult: true });
    expect(state.showCodeForm).toBe(false);
    expect(state.showConnectButton).toBe(true);
  });

  it("接続済みなら何も出さない", () => {
    const state = clusterConnectPanelState({ ...base, connected: true, connectPending: true, needsCode: true });
    expect(state).toEqual({ showCodeForm: false, showPendingElsewhere: false, showConnectButton: false });
  });

  it("manual は接続ボタンを出さない（従来の案内のまま）", () => {
    const state = clusterConnectPanelState({ ...base, auth: "manual" });
    expect(state.showConnectButton).toBe(false);
  });

  it("publickey は入力欄を出さずに接続ボタンだけ出す", () => {
    const state = clusterConnectPanelState({ ...base, auth: "publickey", needsCode: true });
    expect(state.showCodeForm).toBe(false);
    expect(state.showConnectButton).toBe(true);
  });
});

/**
 * `/clusters` のスマホ版 1 語バッジ（ADR-0055 D1-3、Phase 86）: connected / login-needed / down。
 * `tunnel_login_needed` を「down」より優先する（鍵認証も失敗して人の TOTP が要る状態を先に伝える）。
 */
describe("clusterStatusWord", () => {
  it("tunnel_login_needed を最優先する（connected が false でも true でも）", () => {
    expect(clusterStatusWord({ connected: false, tunnel_login_needed: true })).toBe("login-needed");
    expect(clusterStatusWord({ connected: true, tunnel_login_needed: true })).toBe("login-needed");
  });

  it("tunnel_login_needed が無ければ connected をそのまま反映する", () => {
    expect(clusterStatusWord({ connected: true, tunnel_login_needed: false })).toBe("connected");
    expect(clusterStatusWord({ connected: false, tunnel_login_needed: false })).toBe("down");
  });

  it("観測が無ければ値を捏造せず unknown にする", () => {
    expect(clusterStatusWord({ connected: null, tunnel_login_needed: false })).toBe("unknown");
    expect(clusterStatusWord({ connected: undefined, tunnel_login_needed: undefined })).toBe("unknown");
    expect(clusterStatusWord({})).toBe("unknown");
  });

  it("バッジ 1 語（空白なし・12 字以内）", () => {
    for (const input of [
      { connected: true, tunnel_login_needed: false },
      { connected: false, tunnel_login_needed: false },
      { connected: false, tunnel_login_needed: true },
      {},
    ]) {
      const word = clusterStatusWord(input);
      expect(word).not.toMatch(/\s/);
      expect(word.length).toBeLessThanOrEqual(12);
    }
  });
});

/**
 * 作業ディレクトリの表示（ADR-0059 D6、ADR-0055 ラウンド 21、docs/celeris-api-v1.md §3.23/§3.107）:
 * 実効値の出どころを 1 語のバッジにする純関数と、「上書きを消す」ボタンの出し分け。
 */
describe("clusterWorkDirWord", () => {
  it("DB の上書きは settings", () => {
    expect(clusterWorkDirWord("settings")).toBe("settings");
  });

  it("設定ファイルの値は config", () => {
    expect(clusterWorkDirWord("config")).toBe("config");
  });

  it("どちらも無ければ unregistered（値を捏造しない）", () => {
    expect(clusterWorkDirWord(null)).toBe("unregistered");
    expect(clusterWorkDirWord(undefined)).toBe("unregistered");
    expect(clusterWorkDirWord("")).toBe("unregistered");
  });

  it("バッジ 1 語（空白なし）", () => {
    for (const source of ["settings", "config", null, undefined]) {
      expect(clusterWorkDirWord(source)).not.toMatch(/\s/);
    }
  });
});

describe("showClusterWorkDirClear", () => {
  it("DB の上書きがあるときだけ「上書きを消す」を出す", () => {
    expect(showClusterWorkDirClear("settings")).toBe(true);
  });

  it("設定ファイルの値・未登録では出さない（消すものが無い）", () => {
    expect(showClusterWorkDirClear("config")).toBe(false);
    expect(showClusterWorkDirClear(null)).toBe(false);
    expect(showClusterWorkDirClear(undefined)).toBe(false);
  });
});

describe("readClusterWorkDir", () => {
  it("無ければ空文字（celeris の 422 に任せる）", () => {
    const form = new FormData();
    expect(readClusterWorkDir(form)).toBe("");
  });

  it("前後の空白だけ落とす", () => {
    const form = new FormData();
    form.set("work_dir", "  /work/NBB/rmaeda  ");
    expect(readClusterWorkDir(form)).toBe("/work/NBB/rmaeda");
  });

  it("空白だけの入力は空文字のまま（null にしない。`putClusterSettings` の null は「消す」ボタン専用）", () => {
    const form = new FormData();
    form.set("work_dir", "   ");
    expect(readClusterWorkDir(form)).toBe("");
  });
});

/**
 * `PUT /clusters/{id}/settings`（ADR-0059 D6 §3.107）の中継。celeris の応答をそのまま素通しし、
 * エラーは `ActionError` として返す（例外にしない）。接続の中継と同じく `POST /reload` は一切呼ばない。
 */
describe("putClusterSettings", () => {
  it("絶対パスの保存: 応答をそのまま返し、要求本文に work_dir を載せる", async () => {
    const settings: ClusterSettingsView = {
      cluster_id: "pegasus",
      work_dir: "/work/NBB/rmaeda",
      updated_at: "2026-09-22T12:00:00Z",
    };
    mock.on("PUT", "/api/v1/clusters/pegasus/settings", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ work_dir: "/work/NBB/rmaeda" });
      sendJson(res, 200, settings);
    });

    const result = await putClusterSettings(client, "pegasus", "/work/NBB/rmaeda");

    expect(result).toEqual({ ok: true, op: "cluster_settings", id: "pegasus", settings });
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });

  it("null を送ると上書きを消す（応答の work_dir も null）", async () => {
    const settings: ClusterSettingsView = {
      cluster_id: "pegasus",
      work_dir: null,
      updated_at: "2026-09-22T12:00:00Z",
    };
    mock.on("PUT", "/api/v1/clusters/pegasus/settings", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ work_dir: null });
      sendJson(res, 200, settings);
    });

    const result = await putClusterSettings(client, "pegasus", null);

    expect(result).toEqual({ ok: true, op: "cluster_settings", id: "pegasus", settings });
  });

  it("422 validation（絶対パスでも ~ 始まりでもない）は ActionError として返る（例外にしない）", async () => {
    mock.on("PUT", "/api/v1/clusters/pegasus/settings", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "work_dir must be an absolute path or ~ / ~/…",
        extra: {
          errors: [{ field: "work_dir", message: "work_dir must be an absolute path or ~ / ~/…" }],
        },
      }),
    );

    const result = await putClusterSettings(client, "pegasus", "relative/path");

    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toMatchObject({ status: 422, code: "validation" });
      expect(result.error.fields.work_dir).toEqual(["work_dir must be an absolute path or ~ / ~/…"]);
    }
  });

  it("404 cluster_not_found は ActionError として返る", async () => {
    mock.on("PUT", "/api/v1/clusters/unknown/settings", (_req, res) =>
      sendProblem(res, { status: 404, code: "cluster_not_found", detail: "no such cluster" }),
    );

    const result = await putClusterSettings(client, "unknown", "/work/x");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 404, code: "cluster_not_found" });
  });

  it("401 unauthorized（管理系、token_file 未設定でも 401）は ActionError として返る", async () => {
    mock.on("PUT", "/api/v1/clusters/pegasus/settings", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await putClusterSettings(client, "pegasus", "/work/x");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatchObject({ status: 401, code: "unauthorized" });
  });

  it("does not call POST /reload", async () => {
    mock.on("PUT", "/api/v1/clusters/pegasus/settings", (_req, res) =>
      sendJson(res, 200, { cluster_id: "pegasus", work_dir: "/work/x", updated_at: "2026-09-22T12:00:00Z" }),
    );

    await putClusterSettings(client, "pegasus", "/work/x");

    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });
});
