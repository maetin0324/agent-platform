import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { deleteSecret, listSecrets, putSecret, readSecretId, readSecretValue } from "~/celeris/secrets-admin.server";
import type { ReloadResult, SecretList, SecretPutResult } from "~/celeris/types";
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

describe("readSecretId / readSecretValue", () => {
  it("reads the form fields, empty string when absent", () => {
    const form = new FormData();
    expect(readSecretId(form)).toBe("");
    expect(readSecretValue(form)).toBe("");
    form.set("id", "tavily");
    form.set("value", "tvly-abc123");
    expect(readSecretId(form)).toBe("tavily");
    expect(readSecretValue(form)).toBe("tvly-abc123");
  });
});

describe("listSecrets", () => {
  it("passes GET /secrets through verbatim, including entries with updated_at/fingerprint null (未設定)", async () => {
    const list: SecretList = {
      dir: "/home/u/celeris/secrets",
      items: [
        {
          id: "tavily",
          updated_at: "2026-09-17T01:00:00Z",
          fingerprint: "a1b2c3d4",
          used_by: [{ scope: "adapter", name: "local-deep-research", env: "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY" }],
        },
        {
          id: "exa",
          updated_at: null,
          fingerprint: null,
          used_by: [{ scope: "provider", name: "ldr-exa", env: "LDR_SEARCH_ENGINE_WEB_EXA_API_KEY" }],
        },
      ],
    };
    mock.on("GET", "/api/v1/secrets", (_req, res) => sendJson(res, 200, list));

    const result = await listSecrets(client);

    expect(result).toEqual(list);
    // 値はどこにも含まれない（GUI 側で作らない、celeris の応答をそのまま渡すだけ）。
    expect(JSON.stringify(result)).not.toMatch(/tvly-|value/i);
  });

  it("propagates 401 unauthorized as-is (management endpoint, ADR-0030 D3)", async () => {
    mock.on("GET", "/api/v1/secrets", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    await expect(listSecrets(client)).rejects.toMatchObject({ status: 401, code: "unauthorized" });
  });

  it("propagates 409 secrets_unavailable ([secrets] not configured)", async () => {
    mock.on("GET", "/api/v1/secrets", (_req, res) =>
      sendProblem(res, { status: 409, code: "secrets_unavailable", detail: "[secrets] is not configured" }),
    );

    await expect(listSecrets(client)).rejects.toMatchObject({ status: 409, code: "secrets_unavailable" });
  });
});

const putResult: SecretPutResult = { id: "tavily", updated_at: "2026-09-17T02:00:00Z", fingerprint: "deadbeef" };

describe("putSecret", () => {
  it("PUTs /secrets/{id} with {value} then POST /reload on success, returning both outcomes", async () => {
    mock.on("PUT", "/api/v1/secrets/tavily", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ value: "tvly-abc123" });
      sendJson(res, 200, putResult);
    });
    mock.on("POST", "/api/v1/reload", (_req, res) => sendJson(res, 200, { reloaded: true } satisfies ReloadResult));

    const result = await putSecret(client, "tavily", "tvly-abc123");

    expect(result.op).toEqual({ ok: true, op: "put", id: "tavily", secret: putResult });
    expect(result.reload).toEqual({ ok: true, result: { reloaded: true } });
    // 値はリクエスト本文にだけ出る（応答・action の戻り値には出ない）。
    expect(JSON.stringify(result)).not.toContain("tvly-abc123");
  });

  it("does not call /reload when the put itself fails (422 validation: blank value)", async () => {
    mock.on("PUT", "/api/v1/secrets/tavily", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "value must not be blank",
        extra: { errors: [{ field: "value", message: "must not be blank" }] },
      }),
    );

    const result = await putSecret(client, "tavily", "   ");

    expect(result.op.ok).toBe(false);
    expect(result.reload).toBeUndefined();
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });

  it("404 secret_not_found (invalid id) is returned as an ActionError, not thrown", async () => {
    mock.on("PUT", "/api/v1/secrets/bad%2Fid", (_req, res) =>
      sendProblem(res, { status: 404, code: "secret_not_found", detail: "invalid secret id" }),
    );

    const result = await putSecret(client, "bad/id", "x");

    expect(result.op).toEqual({
      ok: false,
      op: "put",
      id: "bad/id",
      error: expect.objectContaining({ status: 404, code: "secret_not_found" }) as unknown,
    });
  });

  it("409 secrets_unavailable ([secrets] not configured) is returned as an ActionError", async () => {
    mock.on("PUT", "/api/v1/secrets/tavily", (_req, res) =>
      sendProblem(res, { status: 409, code: "secrets_unavailable", detail: "[secrets] is not configured" }),
    );

    const result = await putSecret(client, "tavily", "x");

    expect(result.op.ok).toBe(false);
    expect(result.reload).toBeUndefined();
  });

  it("401 unauthorized (no token configured for management) is returned as an ActionError", async () => {
    mock.on("PUT", "/api/v1/secrets/tavily", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await putSecret(client, "tavily", "x");

    expect(result.op).toEqual({
      ok: false,
      op: "put",
      id: "tavily",
      error: expect.objectContaining({ status: 401, code: "unauthorized" }) as unknown,
    });
  });

  it("reports a reload failure separately even though the put succeeded", async () => {
    mock.on("PUT", "/api/v1/secrets/tavily", (_req, res) => sendJson(res, 200, putResult));
    mock.on("POST", "/api/v1/reload", (_req, res) =>
      sendProblem(res, { status: 400, code: "invalid_config", detail: "providers.d/ is invalid" }),
    );

    const result = await putSecret(client, "tavily", "tvly-abc123");

    expect(result.op.ok).toBe(true);
    expect(result.reload).toEqual({
      ok: false,
      error: expect.objectContaining({ status: 400, code: "invalid_config" }) as unknown,
    });
  });
});

describe("deleteSecret", () => {
  it("DELETEs /secrets/{id} then reloads", async () => {
    mock.on("DELETE", "/api/v1/secrets/tavily", (_req, res) => sendJson(res, 200, {}));
    mock.on("POST", "/api/v1/reload", (_req, res) => sendJson(res, 200, { reloaded: true } satisfies ReloadResult));

    const result = await deleteSecret(client, "tavily");

    expect(result.op).toEqual({ ok: true, op: "delete", id: "tavily" });
    expect(result.reload).toEqual({ ok: true, result: { reloaded: true } });
  });

  it("404 secret_not_found is returned as an ActionError", async () => {
    mock.on("DELETE", "/api/v1/secrets/missing", (_req, res) =>
      sendProblem(res, { status: 404, code: "secret_not_found", detail: "no such secret" }),
    );

    const result = await deleteSecret(client, "missing");

    expect(result.op.ok).toBe(false);
    expect(result.reload).toBeUndefined();
    expect(mock.requests.some((r) => r.url === "/api/v1/reload")).toBe(false);
  });
});
