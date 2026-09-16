import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { TaskdClient } from "~/taskd/client.server";
import {
  buildProviderCreateInput,
  buildProviderPatchInput,
  checkProvider,
  createProvider,
  deleteProvider,
  parseEnvText,
  patchProvider,
} from "~/taskd/providers-admin.server";
import type { ProviderCheckResponse, ProviderConfigView1, ReloadResult } from "~/taskd/types";
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

describe("parseEnvText", () => {
  it("parses KEY=VALUE lines, ignoring blank lines", () => {
    expect(parseEnvText("A=1\n\nB=two\n")).toEqual({ A: "1", B: "two" });
  });

  it("returns undefined for empty input (means: unchanged)", () => {
    expect(parseEnvText("")).toBeUndefined();
    expect(parseEnvText("   \n  \n")).toBeUndefined();
  });

  it("treats a line without '=' as a key with an empty value", () => {
    expect(parseEnvText("JUST_A_KEY")).toEqual({ JUST_A_KEY: "" });
  });

  it("splits only at the first '=' (values may contain '=')", () => {
    expect(parseEnvText("A=b=c")).toEqual({ A: "b=c" });
  });
});

describe("buildProviderCreateInput", () => {
  it("reads id/adapter/tiers/concurrency/model/account_pool/env from a FormData", () => {
    const form = new FormData();
    form.set("id", "acct-b");
    form.set("adapter", "claude-code");
    form.append("tiers", "standard");
    form.append("tiers", "frontier");
    form.set("concurrency", "3");
    form.set("model", "claude-sonnet-5");
    form.set("account_pool", "on");
    form.set("env", "KEY=value\n");

    expect(buildProviderCreateInput(form)).toEqual({
      id: "acct-b",
      adapter: "claude-code",
      tiers: ["frontier", "standard"],
      concurrency: 3,
      model: "claude-sonnet-5",
      account_pool: true,
      env: { KEY: "value" },
    });
  });

  it("omits optional fields when blank (taskd applies its own defaults)", () => {
    const form = new FormData();
    form.set("id", "x");
    form.set("adapter", "fake");
    expect(buildProviderCreateInput(form)).toEqual({ id: "x", adapter: "fake" });
  });
});

describe("buildProviderPatchInput", () => {
  it("always sends tiers/concurrency/model/account_pool (pre-filled edit form), env only when non-empty", () => {
    const form = new FormData();
    form.append("tiers", "cheap");
    form.set("concurrency", "1");
    form.set("model", "");
    const patch = buildProviderPatchInput(form);
    expect(patch).toEqual({ tiers: ["cheap"], concurrency: 1, model: "", account_pool: false });
  });

  it("includes env only when the textarea has at least one KEY=VALUE line (empty means unchanged)", () => {
    const form = new FormData();
    form.set("env", "SECRET=abc");
    const patch = buildProviderPatchInput(form);
    expect(patch.env).toEqual({ SECRET: "abc" });
  });
});

const providerConfig: ProviderConfigView1 = {
  id: "acct-b",
  adapter: "claude-code",
  tiers: ["standard"],
  concurrency: 1,
  model: null,
  env_keys: [],
  account_pool: true,
};

describe("createProvider", () => {
  it("POSTs /providers then POST /reload on success, returning both outcomes", async () => {
    mock.on("POST", "/api/v1/providers", (_req, res) => sendJson(res, 201, providerConfig));
    mock.on("POST", "/api/v1/reload", (_req, res) => sendJson(res, 200, { reloaded: true } satisfies ReloadResult));

    const result = await createProvider(client, { id: "acct-b", adapter: "claude-code" });

    expect(result.op).toEqual({ ok: true, op: "create", id: "acct-b", provider: providerConfig });
    expect(result.reload).toEqual({ ok: true, result: { reloaded: true } });
  });

  it("does not call /reload when the create itself fails (409 provider_exists)", async () => {
    mock.on("POST", "/api/v1/providers", (_req, res) =>
      sendProblem(res, { status: 409, code: "provider_exists", detail: "acct-b already exists" }),
    );

    const result = await createProvider(client, { id: "acct-b", adapter: "claude-code" });

    expect(result.op.ok).toBe(false);
    expect(result.reload).toBeUndefined();
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });

  it("reports a reload failure separately even though the create succeeded", async () => {
    mock.on("POST", "/api/v1/providers", (_req, res) => sendJson(res, 201, providerConfig));
    mock.on("POST", "/api/v1/reload", (_req, res) =>
      sendProblem(res, { status: 400, code: "invalid_config", detail: "providers.d/acct-b.toml is invalid" }),
    );

    const result = await createProvider(client, { id: "acct-b", adapter: "claude-code" });

    expect(result.op.ok).toBe(true);
    expect(result.reload).toEqual({
      ok: false,
      error: expect.objectContaining({ status: 400, code: "invalid_config" }) as unknown,
    });
  });

  it("401 unauthorized (no token configured for management) is returned as an ActionError, not thrown", async () => {
    mock.on("POST", "/api/v1/providers", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const result = await createProvider(client, { id: "acct-b", adapter: "claude-code" });
    expect(result.op).toEqual({
      ok: false,
      op: "create",
      id: "acct-b",
      error: expect.objectContaining({ status: 401, code: "unauthorized" }) as unknown,
    });
  });
});

describe("patchProvider", () => {
  it("PATCHes /providers/{id} then reloads", async () => {
    mock.on("PATCH", "/api/v1/providers/acct-b", (_req, res) => sendJson(res, 200, providerConfig));
    mock.on("POST", "/api/v1/reload", (_req, res) => sendJson(res, 200, { reloaded: true } satisfies ReloadResult));

    const result = await patchProvider(client, "acct-b", { concurrency: 2 });
    expect(result.op).toEqual({ ok: true, op: "patch", id: "acct-b", provider: providerConfig });
    expect(result.reload).toEqual({ ok: true, result: { reloaded: true } });
  });

  it("404 provider_not_found is returned as an ActionError", async () => {
    mock.on("PATCH", "/api/v1/providers/missing", (_req, res) =>
      sendProblem(res, { status: 404, code: "provider_not_found", detail: "no such provider" }),
    );
    const result = await patchProvider(client, "missing", {});
    expect(result.op.ok).toBe(false);
  });
});

describe("deleteProvider", () => {
  it("DELETEs /providers/{id} then reloads", async () => {
    mock.on("DELETE", "/api/v1/providers/acct-b", (_req, res) => sendJson(res, 200, {}));
    mock.on("POST", "/api/v1/reload", (_req, res) => sendJson(res, 200, { reloaded: true } satisfies ReloadResult));

    const result = await deleteProvider(client, "acct-b");
    expect(result.op).toEqual({ ok: true, op: "delete", id: "acct-b" });
    expect(result.reload).toEqual({ ok: true, result: { reloaded: true } });
  });
});

describe("checkProvider", () => {
  it("POSTs /providers/{id}/check and does not reload", async () => {
    const checkResponse: ProviderCheckResponse = { result: "ok", checked_at: "2026-09-16T00:00:00Z", detail: "ok" };
    mock.on("POST", "/api/v1/providers/acct-b/check", (_req, res) => sendJson(res, 200, checkResponse));

    const result = await checkProvider(client, "acct-b");
    expect(result.op).toEqual({ ok: true, op: "check", id: "acct-b", result: checkResponse });
    expect(result.reload).toBeUndefined();
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });
});
