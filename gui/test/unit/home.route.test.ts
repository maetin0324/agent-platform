import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { normalizeScope } from "~/lib/console";

/**
 * `/`（Console。ADR-0048 D4、GUI Phase G22）。P-59-a で「秘書へ 302」（Phase G13f-1）から置き換わった:
 * `/` 自身が Console になり、celeris には問い合わせず redirect するだけだった旧 loader は無くなった。
 * 実際の celeris 呼び出し（`GET /console` / `POST /console/instruct`）の中身は `~/celeris/console.server.ts`
 * が持つので、そちらを `test/unit/console.server.test.ts` で見る（`getCelerisClient()` はプロセスで
 * 1 つのインスタンスを使い回す設計〈`~/celeris/client.server.ts`〉なので、route の `loader` を直接呼ぶ
 * テストは他の loader テストと同じく避け、明示的な `CelerisClient` を渡す関数を見る）。
 */

describe("normalizeScope（`?scope=` の正規化）", () => {
  it("無し・all は all", () => {
    expect(normalizeScope(null)).toBe("all");
    expect(normalizeScope(undefined)).toBe("all");
    expect(normalizeScope("all")).toBe("all");
  });

  it("project:<id> / node:<id> はそのまま", () => {
    expect(normalizeScope("project:01JPROJECT")).toBe("project:01JPROJECT");
    expect(normalizeScope("node:cos")).toBe("node:cos");
  });

  it("形が違えば all に丸める", () => {
    expect(normalizeScope("bogus")).toBe("all");
    expect(normalizeScope("project:")).toBe("all");
    expect(normalizeScope("")).toBe("all");
  });
});

describe("ルート定義（P-59-a・ADR-0048 D4、GUI Phase G22）", () => {
  const routes = readFileSync(fileURLToPath(new URL("../../app/routes.ts", import.meta.url)), "utf8");

  it("index は home（Console）、受信箱は /inbox のまま", () => {
    expect(routes).toContain('index("routes/home.tsx")');
    expect(routes).toContain('route("inbox", "routes/inbox.tsx")');
  });

  it("/org/secretary は残る（旧 URL の 302 用）。専用の /org/cos ルートは無く org/:id が受ける", () => {
    expect(routes).toContain('route("org/secretary", "routes/org.secretary.tsx")');
    expect(routes).toContain('route("org/:id", "routes/org.$id.tsx")');
    expect(routes).not.toContain('"org/cos"');
  });

  it("Console の SSE 中継と run の全行の resource route がある", () => {
    expect(routes).toContain('route("console/stream", "routes/console.stream.ts")');
    expect(routes).toContain('route("tasks/:id/runs/:runId/events", "routes/tasks.$id.runs.$runId.events.ts")');
  });
});

describe("/org/secretary（P-59-a: 旧 URL は /org/cos へ 302）", () => {
  it("celeris に問い合わせず /org/cos へ 302 する", async () => {
    const { loader } = await import("~/routes/org.secretary");
    const response = loader({} as never);
    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe("/org/cos");
  });
});
