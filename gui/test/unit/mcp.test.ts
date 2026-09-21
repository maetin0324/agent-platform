import { describe, expect, it } from "vitest";
import type { McpClient, McpScope } from "~/celeris/types";
import {
  mcpAuthKindWord,
  mcpClientStatusWord,
  mcpConnectionUrlHint,
  mcpScopeLabel,
  resolveMcpAuthorLabel,
  sortMcpScopes,
} from "~/lib/mcp";

/**
 * `~/lib/mcp.ts` の純粋関数（ADR-0056 D4、GUI Phase 80）。`~/lib/llm-sources.ts` と同じ方針で、
 * celeris が返した値（`McpClient` / `McpScope`）をそのまま整形するだけの関数を切り出してテストする。
 */

describe("sortMcpScopes", () => {
  it("orders scopes as docs/mcp.md §4 lists them, regardless of input order", () => {
    const input: McpScope[] = ["org:write", "knowledge:read", "console:instruct", "knowledge:propose"];
    expect(sortMcpScopes(input)).toEqual(["knowledge:read", "knowledge:propose", "console:instruct", "org:write"]);
  });

  it("de-duplicates repeated scopes", () => {
    const input: McpScope[] = ["tasks:read", "tasks:read", "org:read"];
    expect(sortMcpScopes(input)).toEqual(["tasks:read", "org:read"]);
  });

  it("returns [] for null/undefined/empty", () => {
    expect(sortMcpScopes(null)).toEqual([]);
    expect(sortMcpScopes(undefined)).toEqual([]);
    expect(sortMcpScopes([])).toEqual([]);
  });
});

describe("mcpScopeLabel", () => {
  it("maps every known McpScope to a non-empty Japanese label", () => {
    const all: McpScope[] = [
      "knowledge:read",
      "knowledge:propose",
      "tasks:read",
      "console:instruct",
      "org:read",
      "org:write",
      "skills:read",
      "skills:write",
    ];
    for (const scope of all) {
      const label = mcpScopeLabel(scope);
      expect(label.length).toBeGreaterThan(0);
      expect(label).not.toBe(scope);
    }
  });
});

describe("mcpAuthKindWord", () => {
  it("is token when token_hash is set", () => {
    expect(mcpAuthKindWord({ token_hash: "abc123" })).toBe("token");
  });

  it("is none when token_hash is null/undefined (--no-token client)", () => {
    expect(mcpAuthKindWord({ token_hash: null })).toBe("none");
    expect(mcpAuthKindWord({ token_hash: undefined })).toBe("none");
  });
});

describe("mcpClientStatusWord", () => {
  it("is revoked when revoked_at is set", () => {
    expect(mcpClientStatusWord({ revoked_at: "2026-09-21T00:00:00Z" })).toBe("revoked");
  });

  it("is active otherwise", () => {
    expect(mcpClientStatusWord({ revoked_at: null })).toBe("active");
    expect(mcpClientStatusWord({ revoked_at: undefined })).toBe("active");
  });
});

describe("mcpConnectionUrlHint（Phase 84。docs/mcp.md §2 の既定値。トークンの値は含まない）", () => {
  it("token 付きの客は既定の Bearer トークンの口（18200）", () => {
    expect(mcpConnectionUrlHint({ token_hash: "abc123" })).toBe("http://127.0.0.1:18200/mcp");
  });

  it("--no-token の客は既定の認証なし・トンネル専用の口（18201）", () => {
    expect(mcpConnectionUrlHint({ token_hash: null })).toBe("http://127.0.0.1:18201/mcp");
    expect(mcpConnectionUrlHint({ token_hash: undefined })).toBe("http://127.0.0.1:18201/mcp");
  });

  it("トークンの値そのものは文字列に含まれない", () => {
    const hint = mcpConnectionUrlHint({ token_hash: "super-secret-hash" });
    expect(hint).not.toContain("super-secret-hash");
  });
});

function client(over: Partial<McpClient> = {}): McpClient {
  return { id: "chatgpt", name: "chatgpt", created_at: "2026-09-21T00:00:00Z", ...over };
}

describe("resolveMcpAuthorLabel", () => {
  it("returns null for a human message (no author)", () => {
    expect(resolveMcpAuthorLabel(null, [])).toBeNull();
    expect(resolveMcpAuthorLabel(undefined, [])).toBeNull();
  });

  it("resolves mcp:<id> to the client's name when known", () => {
    expect(resolveMcpAuthorLabel("mcp:chatgpt", [client({ id: "chatgpt", name: "ChatGPT（研究班）" })])).toBe(
      "外部（ChatGPT（研究班））",
    );
  });

  it("falls back to the raw id when the client list doesn't (yet) have it", () => {
    expect(resolveMcpAuthorLabel("mcp:unknown-client", [client({ id: "chatgpt" })])).toBe("外部（unknown-client）");
    expect(resolveMcpAuthorLabel("mcp:unknown-client", [])).toBe("外部（unknown-client）");
  });
});
