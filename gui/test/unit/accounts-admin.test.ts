import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  cancelAccountLogin,
  checkAccount,
  createAccount,
  deleteAccount,
  readAccountAdapter,
  readAccountId,
  readLoginCode,
  startAccountLogin,
  submitAccountLoginCode,
} from "~/taskd/accounts-admin.server";
import { TaskdClient } from "~/taskd/client.server";
import type { AccountCheckResponse, AccountLoginResult, AccountLoginStart, AccountView } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

describe("readAccountId / readAccountAdapter / readLoginCode", () => {
  it("reads the form fields, empty string / claude-code default when absent", () => {
    const form = new FormData();
    expect(readAccountId(form)).toBe("");
    expect(readAccountAdapter(form)).toBe("claude-code");
    expect(readLoginCode(form)).toBe("");
    form.set("id", "b");
    form.set("code", "good-code");
    expect(readAccountId(form)).toBe("b");
    expect(readLoginCode(form)).toBe("good-code");
  });

  it("reads adapter=codex; anything else falls back to claude-code (taskd's own default, ADR-0025 D6)", () => {
    const form = new FormData();
    form.set("adapter", "codex");
    expect(readAccountAdapter(form)).toBe("codex");
    form.set("adapter", "bogus");
    expect(readAccountAdapter(form)).toBe("claude-code");
  });
});

const account: AccountView = {
  id: "b",
  adapter: "claude-code",
  dir: "/root/claude-accounts/b",
  logged_in: false,
  in_use: 0,
  usage: null,
  score: null,
  excluded_reason: null,
  cooldown: null,
  last_check: null,
  login_pending: false,
  stats: { runs: 0, done: 0, error: 0, input_tokens: 0, output_tokens: 0 },
};

describe("createAccount", () => {
  it("POSTs /accounts with {id, adapter}", async () => {
    mock.on("POST", "/api/v1/accounts", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ id: "b", adapter: "claude-code" });
      sendJson(res, 201, account);
    });
    const result = await createAccount(client, "b", "claude-code");
    expect(result).toEqual({ ok: true, op: "create", id: "b", adapter: "claude-code", account });
  });

  it("passes adapter=codex through to the request body", async () => {
    mock.on("POST", "/api/v1/accounts", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ id: "c", adapter: "codex" });
      sendJson(res, 201, { ...account, id: "c", adapter: "codex", dir: "/root/codex-accounts/c" });
    });
    const result = await createAccount(client, "c", "codex");
    expect(result.ok).toBe(true);
    if (result.ok && result.op === "create") expect(result.adapter).toBe("codex");
  });

  it("409 account_exists is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/accounts", (_req, res) =>
      sendProblem(res, { status: 409, code: "account_exists", detail: "b already exists" }),
    );
    const result = await createAccount(client, "b", "claude-code");
    expect(result.ok).toBe(false);
  });

  it("409 accounts_unavailable ([accounts] not configured)", async () => {
    mock.on("POST", "/api/v1/accounts", (_req, res) =>
      sendProblem(res, { status: 409, code: "accounts_unavailable", detail: "[accounts] is not configured" }),
    );
    const result = await createAccount(client, "b", "claude-code");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.code).toBe("accounts_unavailable");
  });
});

describe("deleteAccount", () => {
  it("DELETEs /accounts/{id}?adapter=claude-code", async () => {
    mock.on("DELETE", "/api/v1/accounts/b", (req, res) => {
      expect(req.url).toContain("adapter=claude-code");
      sendJson(res, 200, {});
    });
    const result = await deleteAccount(client, "b", "claude-code");
    expect(result).toEqual({ ok: true, op: "delete", id: "b", adapter: "claude-code" });
  });

  it("DELETEs /accounts/{id}?adapter=codex", async () => {
    mock.on("DELETE", "/api/v1/accounts/c", (req, res) => {
      expect(req.url).toContain("adapter=codex");
      sendJson(res, 200, {});
    });
    const result = await deleteAccount(client, "c", "codex");
    expect(result).toEqual({ ok: true, op: "delete", id: "c", adapter: "codex" });
  });

  it("409 account_in_use", async () => {
    mock.on("DELETE", "/api/v1/accounts/b", (_req, res) =>
      sendProblem(res, { status: 409, code: "account_in_use", detail: "in use" }),
    );
    const result = await deleteAccount(client, "b", "claude-code");
    expect(result.ok).toBe(false);
  });
});

describe("checkAccount", () => {
  it("POSTs /accounts/{id}/check?adapter=", async () => {
    const response: AccountCheckResponse = {
      result: "ok",
      checked_at: "2026-09-16T00:00:00Z",
      detail: "ok",
      usage: {
        five_hour: { utilization: 0.42, resets_at: "2026-09-16T01:00:00Z" },
        seven_day: { utilization: 0.18, resets_at: "2026-09-17T00:00:00Z" },
        status: "allowed",
        observed_at: "2026-09-16T00:00:00Z",
        source: "check",
      },
    };
    mock.on("POST", "/api/v1/accounts/b/check", (req, res) => {
      expect(req.url).toContain("adapter=claude-code");
      sendJson(res, 200, response);
    });
    const result = await checkAccount(client, "b", "claude-code");
    expect(result).toEqual({ ok: true, op: "check", id: "b", adapter: "claude-code", result: response });
  });
});

describe("startAccountLogin", () => {
  it("POSTs /accounts/{id}/login (claude-code, paste_code) and returns the url", async () => {
    const start: AccountLoginStart = {
      kind: "paste_code",
      url: "https://claude.example.invalid/cai/oauth/authorize?code=true",
      expires_at: "2026-09-16T00:10:00Z",
    };
    mock.on("POST", "/api/v1/accounts/b/login", (req, res) => {
      expect(req.url).toContain("adapter=claude-code");
      sendJson(res, 200, start);
    });
    const result = await startAccountLogin(client, "b", "claude-code");
    expect(result).toEqual({ ok: true, op: "login_start", id: "b", adapter: "claude-code", login: start });
  });

  it("POSTs /accounts/{id}/login?adapter=codex and returns kind: device_code + user_code (ADR-0025 D5)", async () => {
    const start: AccountLoginStart = {
      kind: "device_code",
      url: "https://auth.openai.com/codex/device",
      user_code: "ABCD-EFGHI",
      expires_at: "2026-09-16T00:15:00Z",
    };
    mock.on("POST", "/api/v1/accounts/c/login", (req, res) => {
      expect(req.url).toContain("adapter=codex");
      sendJson(res, 200, start);
    });
    const result = await startAccountLogin(client, "c", "codex");
    expect(result).toEqual({ ok: true, op: "login_start", id: "c", adapter: "codex", login: start });
    if (result.ok && result.op === "login_start") {
      expect(result.login.kind).toBe("device_code");
      expect(result.login.user_code).toBe("ABCD-EFGHI");
    }
  });

  it("502 login_failed (no URL within 15s)", async () => {
    mock.on("POST", "/api/v1/accounts/b/login", (_req, res) =>
      sendProblem(res, { status: 502, code: "login_failed", detail: "no authorization url within 15s" }),
    );
    const result = await startAccountLogin(client, "b", "claude-code");
    expect(result.ok).toBe(false);
  });
});

describe("submitAccountLoginCode", () => {
  it("POSTs /accounts/{id}/login/code with {code}", async () => {
    const loginResult: AccountLoginResult = { result: "ok", detail: null };
    mock.on("POST", "/api/v1/accounts/b/login/code", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ code: "good-code" });
      sendJson(res, 200, loginResult);
    });
    const result = await submitAccountLoginCode(client, "b", "claude-code", "good-code");
    expect(result).toEqual({ ok: true, op: "login_code", id: "b", adapter: "claude-code", result: loginResult });
  });

  it("409 login_not_started", async () => {
    mock.on("POST", "/api/v1/accounts/b/login/code", (_req, res) =>
      sendProblem(res, { status: 409, code: "login_not_started", detail: "no login in progress" }),
    );
    const result = await submitAccountLoginCode(client, "b", "claude-code", "x");
    expect(result.ok).toBe(false);
  });

  it("409 login_code_not_supported for codex (ADR-0025 D5, surfaced by ErrorFlash as a clear message)", async () => {
    mock.on("POST", "/api/v1/accounts/c/login/code", (req, res) => {
      expect(req.url).toContain("adapter=codex");
      sendProblem(res, {
        status: 409,
        code: "login_code_not_supported",
        detail: "codex completes login via device auth only",
      });
    });
    const result = await submitAccountLoginCode(client, "c", "codex", "x");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.code).toBe("login_code_not_supported");
      expect(result.error.status).toBe(409);
    }
  });
});

describe("cancelAccountLogin", () => {
  it("DELETEs /accounts/{id}/login?adapter=", async () => {
    mock.on("DELETE", "/api/v1/accounts/b/login", (req, res) => {
      expect(req.url).toContain("adapter=claude-code");
      sendJson(res, 200, {});
    });
    const result = await cancelAccountLogin(client, "b", "claude-code");
    expect(result).toEqual({ ok: true, op: "login_cancel", id: "b", adapter: "claude-code" });
  });
});
