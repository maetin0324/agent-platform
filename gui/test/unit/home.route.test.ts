import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { loader } from "~/routes/home";

/**
 * 最初の画面は秘書（Phase G13f-1、監査 2）。受信箱は裏方の `/inbox` に残す。
 * `loader` は taskd に問い合わせず、`/org/secretary` へリダイレクトするだけ。
 */
describe("/（最初の画面）", () => {
  it("秘書へリダイレクトする", () => {
    const response = loader({} as never);
    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe("/org/secretary");
  });

  it("ルート定義: index は home、受信箱は /inbox", () => {
    const routes = readFileSync(fileURLToPath(new URL("../../app/routes.ts", import.meta.url)), "utf8");
    expect(routes).toContain('index("routes/home.tsx")');
    expect(routes).toContain('route("inbox", "routes/inbox.tsx")');
  });
});
