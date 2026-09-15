import type { Config } from "@react-router/dev/config";

export default {
  // SSR。loader / action がサーバ側の BFF になる（docs/adr/0002 D2）
  ssr: true,
} satisfies Config;
