/**
 * GUI サーバ側の環境変数（docs/DESIGN.md §8、§9）。サーバ専用モジュール（`.server.ts`）なのでクライアントには入らない。
 * トークンの中身はここでは読まない（`CelerisClient.fromEnv` がファイルを読んでメモリに持つ）。
 */
export interface GuiConfig {
  /** `CELERIS_GUI_BIND`（既定 127.0.0.1:7700）の host 部分 */
  bindHost: string;
  /** `Host` 検査の許可リスト（小文字、ポート無し） */
  allowedHosts: ReadonlySet<string>;
}

const LOOPBACK_HOSTS = ["localhost", "127.0.0.1", "[::1]", "::1"];

/** `host:port` / `[v6]:port` / `host` の host 部分を取り出す（小文字化）。 */
export function hostWithoutPort(hostHeader: string): string {
  const h = hostHeader.trim().toLowerCase();
  if (h.startsWith("[")) {
    const end = h.indexOf("]");
    return end === -1 ? h : h.slice(0, end + 1);
  }
  const colon = h.lastIndexOf(":");
  return colon === -1 ? h : h.slice(0, colon);
}

export function readGuiConfig(env: NodeJS.ProcessEnv = process.env): GuiConfig {
  const bind = env.CELERIS_GUI_BIND ?? "127.0.0.1:7700";
  const bindHost = hostWithoutPort(bind);
  const allowed = new Set<string>(LOOPBACK_HOSTS);
  if (bindHost) allowed.add(bindHost);
  for (const h of (env.CELERIS_GUI_ALLOWED_HOSTS ?? "").split(",")) {
    const v = hostWithoutPort(h);
    if (v) allowed.add(v);
  }
  return { bindHost, allowedHosts: allowed };
}

let cached: GuiConfig | undefined;
export function getGuiConfig(): GuiConfig {
  cached ??= readGuiConfig();
  return cached;
}
