/**
 * `/accounts` の「MCP クライアント」節（ADR-0056 D4、GUI Phase 80）と Console の「外部（<client>）」帯
 * （ADR-0056 D2）で使う純関数。celeris が返した値（`McpClient` / `McpScope`）をそのまま整形するだけで、
 * 判断（認証・スコープ・流量制限）は MCP サーバー（`crates/celeris-mcp`）の中で決まっている
 * （`~/lib/llm-sources.ts` と同じ規律。ADR-0055 D2）。
 */

import type { McpClient, McpScope } from "~/celeris/types";

/** `docs/mcp.md` §4 の表と同じ並び（`celerisctl mcp client add` の既定に出てくる順）。 */
const SCOPE_ORDER: readonly McpScope[] = [
  "knowledge:read",
  "knowledge:propose",
  "tasks:read",
  "tasks:interact",
  "tasks:control",
  "tasks:decide",
  "console:instruct",
  "org:read",
  "org:write",
  "skills:read",
  "skills:write",
];

/** チップの並び（`docs/mcp.md` §4 の並び順に揃え、重複は落とす）。未知の値は末尾にアルファベット順で足す。 */
export function sortMcpScopes(scopes: readonly McpScope[] | null | undefined): McpScope[] {
  const unique = Array.from(new Set(scopes ?? []));
  const known = SCOPE_ORDER.filter((s) => unique.includes(s));
  const unknown = unique.filter((s) => !SCOPE_ORDER.includes(s)).sort();
  return [...known, ...unknown];
}

const SCOPE_LABEL: Record<McpScope, string> = {
  "knowledge:read": "知識: 読む",
  "knowledge:propose": "知識: 提案",
  "tasks:read": "タスク/案件: 読む",
  "tasks:interact": "タスク: コメント/回答",
  "tasks:control": "タスク: やり直し/取り消し",
  "tasks:decide": "タスク: 承認/却下",
  "console:instruct": "Console: 指示",
  "org:read": "組織: 読む",
  "org:write": "組織: 書く",
  "skills:read": "skills: 読む",
  "skills:write": "skills: 書く",
};

/** スコープ 1 件の短い日本語ラベル（未知の値はそのまま返す）。 */
export function mcpScopeLabel(scope: McpScope): string {
  return SCOPE_LABEL[scope] ?? scope;
}

/**
 * 認証の種類（ADR-0056 D1）: `token_hash` が有れば `auth = "token"` の口で発行したトークン付きの客、
 * 無ければ `--no-token`（`auth = "none"` の口に `client = "<id>"` で固定する専用）。
 * 状態バッジは 1 語（`docs/adr/0055-mobile-ux.md` D1-3。`gui/scripts/mobile-audit.mjs` が機械検査する）。
 */
export function mcpAuthKindWord(client: Pick<McpClient, "token_hash">): "token" | "none" {
  return client.token_hash ? "token" : "none";
}

/** 失効の有無を 1 語のバッジに。 */
export function mcpClientStatusWord(client: Pick<McpClient, "revoked_at">): "revoked" | "active" {
  return client.revoked_at ? "revoked" : "active";
}

/**
 * 接続 URL のヒント（`docs/mcp.md` §2 の既定値。ADR-0056 D1 の 2 つの口 — `auth = "token"` の
 * 既定 `127.0.0.1:18200`、`auth = "none"`（loopback 限定トンネル専用）の既定 `127.0.0.1:18201`）。
 * **トークンの値は一切含まない**（`token_hash` はそもそも値そのものを持たない）。実際の `listen` は
 * デプロイごとの設定（`[mcp]`）で変わりうるので、あくまで「よくある既定」のヒント（Phase 84、
 * `/accounts` の「MCP クライアント」節でコピーできるようにする）。
 */
export function mcpConnectionUrlHint(client: Pick<McpClient, "token_hash">): string {
  return mcpAuthKindWord(client) === "token" ? "http://127.0.0.1:18200/mcp" : "http://127.0.0.1:18201/mcp";
}

/**
 * Console の human ブロック（ADR-0056 D2）の `author`（`mcp:<client_id>` または `null`）を
 * 「外部（<client name>）」の帯の文言にする。`clients` に該当する客が見つかれば `name` を、
 * 見つからなければ（未取得・失効後に消えた等）id をそのまま使う。人の発言（`author` が無い）は `null`
 * （帯を出さない）。
 */
export function resolveMcpAuthorLabel(
  author: string | null | undefined,
  clients: readonly Pick<McpClient, "id" | "name">[],
): string | null {
  if (!author) return null;
  const id = author.startsWith("mcp:") ? author.slice("mcp:".length) : author;
  const found = clients.find((c) => c.id === id);
  return `外部（${found?.name ?? id}）`;
}
