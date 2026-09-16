import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  cancelAccountLogin,
  checkAccount,
  createAccount,
  deleteAccount,
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

describe("readAccountId / readLoginCode", () => {
  it("reads the form fields, empty string when absent", () => {
    const form = new FormData();
    expect(readAccountId(form)).toBe("");
    expect(readLoginCode(form)).toBe("");
    form.set("id", "b");
    form.set("code", "good-code");
    expect(readAccountId(form)).toBe("b");
    expect(readLoginCode(form)).toBe("good-code");
  });
});

const account: AccountView = {
  id: "b",
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
  it("POSTs /accounts with {id}", async () => {
    mock.on("POST", "/api/v1/accounts", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ id: "b" });
      sendJson(res, 201, account);
    });
    const result = await createAccount(client, "b");
    expect(result).toEqual({ ok: true, op: "create", id: "b", account });
  });

  it("409 account_exists is returned as an ActionError", async () => {
    mock.on("POST", "/api/v1/accounts", (_req, res) =>
      sendProblem(res, { status: 409, code: "account_exists", detail: "b already exists" }),
    );
    const result = await createAccount(client, "b");
    expect(result.ok).toBe(false);
  });

  it("409 accounts_unavailable ([accounts] not configured)", async () => {
    mock.on("POST", "/api/v1/accounts", (_req, res) =>
      sendProblem(res, { status: 409, code: "accounts_unavailable", detail: "[accounts] is not configured" }),
    );
    const result = await createAccount(client, "b");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.code).toBe("accounts_unavailable");
  });
});

describe("deleteAccount", () => {
  it("DELETEs /accounts/{id}", async () => {
    mock.on("DELETE", "/api/v1/accounts/b", (_req, res) => sendJson(res, 200, {}));
    const result = await deleteAccount(client, "b");
    expect(result).toEqual({ ok: true, op: "delete", id: "b" });
  });

  it("409 account_in_use", async () => {
    mock.on("DELETE", "/api/v1/accounts/b", (_req, res) =>
      sendProblem(res, { status: 409, code: "account_in_use", detail: "in use" }),
    );
    const result = await deleteAccount(client, "b");
    expect(result.ok).toBe(false);
  });
});

describe("checkAccount", () => {
  it("POSTs /accounts/{id}/check", async () => {
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
    mock.on("POST", "/api/v1/accounts/b/check", (_req, res) => sendJson(res, 200, response));
    const result = await checkAccount(client, "b");
    expect(result).toEqual({ ok: true, op: "check", id: "b", result: response });
  });
});

describe("startAccountLogin", () => {
  it("POSTs /accounts/{id}/login and returns the url", async () => {
    const start: AccountLoginStart = {
      url: "https://claude.example.invalid/cai/oauth/authorize?code=true",
      expires_at: "2026-09-16T00:10:00Z",
    };
    mock.on("POST", "/api/v1/accounts/b/login", (_req, res) => sendJson(res, 200, start));
    const result = await startAccountLogin(client, "b");
    expect(result).toEqual({ ok: true, op: "login_start", id: "b", login: start });
  });

  it("502 login_failed (no URL within 15s)", async () => {
    mock.on("POST", "/api/v1/accounts/b/login", (_req, res) =>
      sendProblem(res, { status: 502, code: "login_failed", detail: "no authorization url within 15s" }),
    );
    const result = await startAccountLogin(client, "b");
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
    const result = await submitAccountLoginCode(client, "b", "good-code");
    expect(result).toEqual({ ok: true, op: "login_code", id: "b", result: loginResult });
  });

  it("409 login_not_started", async () => {
    mock.on("POST", "/api/v1/accounts/b/login/code", (_req, res) =>
      sendProblem(res, { status: 409, code: "login_not_started", detail: "no login in progress" }),
    );
    const result = await submitAccountLoginCode(client, "b", "x");
    expect(result.ok).toBe(false);
  });
});

describe("cancelAccountLogin", () => {
  it("DELETEs /accounts/{id}/login", async () => {
    mock.on("DELETE", "/api/v1/accounts/b/login", (_req, res) => sendJson(res, 200, {}));
    const result = await cancelAccountLogin(client, "b");
    expect(result).toEqual({ ok: true, op: "login_cancel", id: "b" });
  });
});
