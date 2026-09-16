import { defineConfig } from "vitest/config";

// 単体テスト。実 taskd は使わず、test/mock-taskd/（プロセス内 HTTP サーバ）だけで動く。外部ネットワークに出ない。
export default defineConfig({
  resolve: {
    tsconfigPaths: true,
  },
  test: {
    environment: "node",
    include: ["test/**/*.test.ts", "app/**/*.test.ts"],
    exclude: ["e2e/**", "node_modules/**", "build/**"],
  },
});
