import { describe, expect, it } from "vitest";
import { buildInstructBody } from "~/lib/console";
import { consoleComposerScopeForLocation, isConsoleComposerPathname } from "~/lib/console-composer";

/**
 * ADR-0057（Phase 92「Console composer をレイアウトレベルへ」）。composer 自体（`~/components/
 * ConsoleComposer.tsx`）は `~/root.tsx` と `~/components/Console.tsx` の 2 か所からレンダーされる
 * ようになったが、「どの画面で出すか」「どの scope（=誰宛て）で送るか」を決める判断は celeris への
 * 問い合わせを伴わない純粋関数（`~/lib/console-composer.ts`）に切り出してある。このリポジトリには
 * DOM を描画する unit テストが無い（G10-U1、`test/unit/console.test.ts` と同じ理由）ので、この判断
 * ロジックをここで直接テストする。
 */

describe("isConsoleComposerPathname（composer を出す画面。P-G42-1 の受け入れ条件 1）", () => {
  it("home（/）と org-node（/org/:id）では出す", () => {
    expect(isConsoleComposerPathname("/")).toBe(true);
    expect(isConsoleComposerPathname("/org/cos")).toBe(true);
    expect(isConsoleComposerPathname("/org/coding-poc")).toBe(true);
  });

  it("/org 自体（一覧）・/org/:id の下の階層では出さない", () => {
    expect(isConsoleComposerPathname("/org")).toBe(false);
    expect(isConsoleComposerPathname("/org/")).toBe(false);
    expect(isConsoleComposerPathname("/org/cos/extra")).toBe(false);
  });

  it("Console 以外の画面では出さない", () => {
    for (const pathname of ["/board", "/projects", "/projects/p1", "/tasks/t1", "/inbox", "/help"]) {
      expect(isConsoleComposerPathname(pathname)).toBe(false);
    }
  });
});

describe("consoleComposerScopeForLocation（送信先の scope。受け入れ条件 2）", () => {
  it("/ は既定（scope=all）なら null（`scope` フィールドを送らない。Phase 91 までと同じ挙動）", () => {
    expect(consoleComposerScopeForLocation("/", "")).toBeNull();
    expect(consoleComposerScopeForLocation("/", "?scope=bogus")).toBeNull();
  });

  it("/ に ?scope=project:<id> / node:<id> があればそれ（normalizeScope と同じ規則）", () => {
    expect(consoleComposerScopeForLocation("/", "?scope=project:p1")).toBe("project:p1");
    expect(consoleComposerScopeForLocation("/", "?scope=node:cos")).toBe("node:cos");
  });

  it("/org/:id は node:<id>（URL エンコードされた id もデコードする）", () => {
    expect(consoleComposerScopeForLocation("/org/cos", "")).toBe("node:cos");
    expect(consoleComposerScopeForLocation("/org/coding-poc", "")).toBe("node:coding-poc");
    expect(consoleComposerScopeForLocation("/org/foo%2Fbar", "")).toBe("node:foo/bar");
  });

  it("composer の対象外の画面では null", () => {
    expect(consoleComposerScopeForLocation("/org", "")).toBeNull();
    expect(consoleComposerScopeForLocation("/board", "")).toBeNull();
    expect(consoleComposerScopeForLocation("/tasks/t1", "?scope=node:cos")).toBeNull();
  });
});

describe("composer の送信は「いま見ているノード」へ飛ぶ（受け入れ条件 2、~/components/ConsoleComposer.tsx::submit と同じ組み立て）", () => {
  it("/org/coding-poc で打つと、返信先を選んでいなくても node:coding-poc 宛てになる", () => {
    const scope = consoleComposerScopeForLocation("/org/coding-poc", "");
    expect(buildInstructBody("状況は？", null, scope)).toEqual({ text: "状況は？", scope: "node:coding-poc" });
  });

  it("/?scope=project:p1 で打つと project:p1 宛てになる", () => {
    const scope = consoleComposerScopeForLocation("/", "?scope=project:p1");
    expect(buildInstructBody("進めて", null, scope)).toEqual({ text: "進めて", scope: "project:p1" });
  });

  it("返信先（『返信』から選んだ先）があれば、画面の scope より優先される", () => {
    const scope = consoleComposerScopeForLocation("/org/coding-poc", "");
    expect(buildInstructBody("続けて", { kind: "node", nodeId: "research" }, scope)).toEqual({
      text: "続けて",
      scope: "node:research",
    });
  });

  it("/（scope=all）では既定の scope を付けない（celeris の既定 = CoS）", () => {
    const scope = consoleComposerScopeForLocation("/", "");
    expect(buildInstructBody("状況は？", null, scope)).toEqual({ text: "状況は？" });
  });
});
