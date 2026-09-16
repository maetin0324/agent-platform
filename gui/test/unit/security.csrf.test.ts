import { describe, expect, it } from "vitest";
import { csrfCheck, csrfViolation, expressCsrfGuard } from "~/middleware/security.server";

// docs/DESIGN.md §8.2 CSRF / docs/adr/0005 D1。
// 変更系（GET / HEAD / OPTIONS 以外）だけ、`Origin` は自オリジンと一致、`Sec-Fetch-Site` は same-origin / none。

const SELF = "http://127.0.0.1:7700";

function req(method: string, headers: Record<string, string> = {}): Request {
  return new Request(`${SELF}/tasks/01HZZZZZZZZZZZZZZZZZZZZZZZ`, { method, headers });
}

describe("csrfViolation", () => {
  it("ignores safe methods even with a foreign Origin", () => {
    expect(csrfViolation(req("GET", { origin: "http://evil.example" }))).toBeNull();
    expect(csrfViolation(req("HEAD", { "sec-fetch-site": "cross-site" }))).toBeNull();
    expect(csrfViolation(req("OPTIONS", { origin: "http://evil.example" }))).toBeNull();
  });

  it("passes a POST with no Origin and no Sec-Fetch-Site (curl, server-side fetch)", () => {
    expect(csrfViolation(req("POST"))).toBeNull();
  });

  it("passes a same-origin browser POST (Origin matches, Sec-Fetch-Site same-origin)", () => {
    expect(csrfViolation(req("POST", { origin: SELF, "sec-fetch-site": "same-origin" }))).toBeNull();
    expect(csrfViolation(req("POST", { origin: "HTTP://127.0.0.1:7700", "sec-fetch-site": "none" }))).toBeNull();
  });

  it("rejects a POST whose Origin is not us", () => {
    const v = csrfViolation(req("POST", { origin: "http://evil.example" }));
    expect(v).toContain("http://evil.example");
    // ホスト名が同じでもポートが違えば別オリジン
    expect(csrfViolation(req("POST", { origin: "http://127.0.0.1:7710" }))).not.toBeNull();
  });

  it("rejects cross-site / same-site Sec-Fetch-Site even when Origin is absent", () => {
    expect(csrfViolation(req("POST", { "sec-fetch-site": "cross-site" }))).toContain("cross-site");
    expect(csrfViolation(req("POST", { "sec-fetch-site": "same-site" }))).toContain("same-site");
  });

  it("applies to every non-safe method", () => {
    expect(csrfViolation(req("DELETE", { origin: "http://evil.example" }))).not.toBeNull();
    expect(csrfViolation(req("PUT", { origin: "http://evil.example" }))).not.toBeNull();
  });
});

function args(request: Request) {
  return { request, params: {}, context: {} as never, url: new URL(request.url), pattern: "/tasks/:id" };
}

describe("csrfCheck middleware", () => {
  it("throws a 403 Response on violation and returns nothing otherwise", async () => {
    let thrown: unknown;
    try {
      await csrfCheck(args(req("POST", { origin: "http://evil.example" })), (() =>
        Promise.resolve(new Response())) as never);
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(Response);
    expect((thrown as Response).status).toBe(403);
    expect(await (thrown as Response).text()).toContain("forbidden");

    // csrfCheck は同期関数（違反が無ければ何も返さない）
    expect(
      csrfCheck(args(req("POST", { origin: SELF })), (() => Promise.resolve(new Response())) as never),
    ).toBeUndefined();
  });
});

describe("expressCsrfGuard (Express layer, before React Router's own 400)", () => {
  function run(method: string, headers: Record<string, string>) {
    const sent: { status?: number; body?: string } = {};
    let nextCalled = false;
    expressCsrfGuard(
      {
        method,
        protocol: "http",
        originalUrl: "/tasks/01HZZZZZZZZZZZZZZZZZZZZZZZ",
        headers: { host: "127.0.0.1:7700", ...headers },
      },
      {
        status(code) {
          sent.status = code;
          return {
            type() {
              return {
                send(body: string) {
                  sent.body = body;
                },
              };
            },
          };
        },
      },
      () => {
        nextCalled = true;
      },
    );
    return { sent, nextCalled };
  }

  it("lets GET through and POST without browser headers through", () => {
    expect(run("GET", { origin: "http://evil.example" }).nextCalled).toBe(true);
    expect(run("POST", {}).nextCalled).toBe(true);
    expect(run("POST", { origin: "http://127.0.0.1:7700" }).nextCalled).toBe(true);
  });

  it("answers 403 (not 400) to a POST with a foreign Origin, and does not call next", () => {
    const r = run("POST", { origin: "http://evil.example" });
    expect(r.nextCalled).toBe(false);
    expect(r.sent.status).toBe(403);
    expect(r.sent.body).toContain("forbidden");
  });

  it("answers 403 to Sec-Fetch-Site: cross-site", () => {
    expect(run("POST", { "sec-fetch-site": "cross-site" }).sent.status).toBe(403);
  });
});
