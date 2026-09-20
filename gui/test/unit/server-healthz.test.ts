import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { loader } from "~/routes/healthz";

// ADR-0040 D4（Phase 46）: 昇格のライブ引き継ぎのために GUI が満たすべき契約。
//   - `GET /healthz` が `release`（`CELERIS_GUI_RELEASE`。無ければ `"dev"`）を返す。認証の対象外のまま
//     （app/auth.server.ts の PUBLIC_PATHS）。promote.sh はこれで新しい GUI の引き継ぎを見る
//   - `server.js` の `listen` が `reusePort: true` を渡す（新旧の GUI が同じポートに同時に bind できる）
// `server.js` は副作用だけの入口（`build/server/index.js` を import して bind する）でテストから
// import できないので、そちらはソースの契約と、Node が本当に `reusePort` を受けることを見る。

const serverJs = readFileSync(fileURLToPath(new URL("../../server.js", import.meta.url)), "utf8");

describe("/healthz が release を返す（ADR-0040 D4）", () => {
  it("CELERIS_GUI_RELEASE をそのまま返し、既存の ok / name / version も残す", async () => {
    const before = process.env.CELERIS_GUI_RELEASE;
    process.env.CELERIS_GUI_RELEASE = "0123456789ab";
    try {
      const body = await loader().json();
      expect(body.ok).toBe(true);
      expect(body.name).toBe("celeris-gui");
      expect(typeof body.version).toBe("string");
      expect(body.release).toBe("0123456789ab");
    } finally {
      if (before === undefined) delete process.env.CELERIS_GUI_RELEASE;
      else process.env.CELERIS_GUI_RELEASE = before;
    }
  });

  it("CELERIS_GUI_RELEASE が無ければ dev", async () => {
    const before = process.env.CELERIS_GUI_RELEASE;
    delete process.env.CELERIS_GUI_RELEASE;
    try {
      const body = await loader().json();
      expect(body.release).toBe("dev");
    } finally {
      if (before !== undefined) process.env.CELERIS_GUI_RELEASE = before;
    }
  });
});

describe("server.js の reusePort（ADR-0040 D4）", () => {
  it("listen に reusePort: true を渡している", () => {
    expect(serverJs).toMatch(/app\.listen\(\{\s*port: bind\.port,\s*host: bind\.host,\s*reusePort: true\s*\}/);
  });

  it("この Node は同じポートへの二重 bind を reusePort で受ける（loopback のみ）", async () => {
    const listen = (port: number) =>
      new Promise<ReturnType<typeof createServer>>((resolve, reject) => {
        const srv = createServer((_req, res) => res.end("ok"));
        srv.on("error", reject);
        srv.listen({ port, host: "127.0.0.1", reusePort: true }, () => resolve(srv));
      });
    const close = (srv: ReturnType<typeof createServer>) => new Promise<void>((resolve) => srv.close(() => resolve()));

    const first = await listen(0);
    const address = first.address();
    if (address === null || typeof address === "string") throw new Error("no port");
    const second = await listen(address.port);
    try {
      expect(address.port).toBeGreaterThan(0);
    } finally {
      await close(second);
      await close(first);
    }
  });
});
