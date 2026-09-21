import { describe, expect, it } from "vitest";
import type { ConsoleBlock, EventRow } from "~/celeris/types";
import {
  appendConsoleBlock,
  applyMention,
  buildInstructBody,
  consoleWaitingCounts,
  findMentionQuery,
  formatRunEventRow,
  matchMentionCandidates,
  normalizeScope,
  parseScope,
  progressSummaryLine,
  replyTargetForMessageBlock,
  scopeForNode,
  scopeForProject,
  taskLineSummary,
} from "~/lib/console";
import { consoleGrowingReplySteps } from "../mock-celeris/fixtures";

/**
 * `~/lib/console.ts` の純粋関数（ADR-0048 D1/D3/D4、GUI Phase G22）。HTTP も React も持ち込まない
 * ので `~/lib/reports.ts` / `~/lib/approvals.ts` と同じ作り: DOM を描画する unit テストはこのリポジトリに無い
 * （G10-U1）ため、判断・整形はここに集めて純粋関数としてテストする。
 */

const humanBlock = (
  over: Partial<Extract<ConsoleBlock, { kind: "human" }>> = {},
): Extract<ConsoleBlock, { kind: "human" }> => ({
  kind: "human",
  at: "2026-09-20T01:00:00Z",
  cursor: "c1",
  message_id: "m1",
  node_id: "cos",
  text: "こんにちは",
  ...over,
});

const replyBlock = (
  over: Partial<Extract<ConsoleBlock, { kind: "reply" }>> = {},
): Extract<ConsoleBlock, { kind: "reply" }> => ({
  kind: "reply",
  at: "2026-09-20T01:00:10Z",
  cursor: "c2",
  message_id: "m2",
  node_id: "cos",
  text: "了解しました。",
  ...over,
});

const progressBlock = (
  over: Partial<Extract<ConsoleBlock, { kind: "progress" }>> = {},
): Extract<ConsoleBlock, { kind: "progress" }> => ({
  kind: "progress",
  at: "2026-09-20T01:00:05Z",
  cursor: "c3",
  title: "関連研究を調べる",
  assignee: "research",
  harness: "fake",
  tier: "standard",
  progress: {
    task_id: "t1",
    run_id: "r1",
    count: 3,
    tool_count: 2,
    last_status: "searching",
    started_at: "2026-09-20T01:00:05Z",
    updated_at: "2026-09-20T01:00:29Z",
    first: [],
    last: [],
  },
  ...over,
});

const questionBlock = (
  over: Partial<Extract<ConsoleBlock, { kind: "question" }>> = {},
): Extract<ConsoleBlock, { kind: "question" }> => ({
  kind: "question",
  at: "2026-09-20T01:00:00Z",
  cursor: "cq",
  answered: false,
  run_id: "r1",
  task_id: "t1",
  text: "続けてよいですか？",
  ...over,
});

const approvalBlock = (
  over: Partial<Extract<ConsoleBlock, { kind: "approval" }>> = {},
): Extract<ConsoleBlock, { kind: "approval" }> => ({
  kind: "approval",
  at: "2026-09-20T01:00:00Z",
  cursor: "ca",
  approval: {
    id: "a1",
    node_id: "coding-poc",
    question: "本番に触ってよいですか？",
    created_at: "2026-09-20T01:00:00Z",
  },
  ...over,
});

const milestoneBlock = (
  over: Partial<Extract<ConsoleBlock, { kind: "milestone" }>> = {},
): Extract<ConsoleBlock, { kind: "milestone" }> => ({
  kind: "milestone",
  at: "2026-09-20T01:00:00Z",
  cursor: "cm",
  milestone: {
    id: "m1",
    project_id: "p1",
    seq: 1,
    title: "第一段",
    status: "proposed",
    created_at: "2026-09-20T01:00:00Z",
    updated_at: "2026-09-20T01:00:00Z",
  },
  ...over,
});

describe("normalizeScope / parseScope / scopeForProject / scopeForNode", () => {
  it("normalizeScope: 無し・all は all、project:/node: はそのまま、それ以外は all", () => {
    expect(normalizeScope(null)).toBe("all");
    expect(normalizeScope("all")).toBe("all");
    expect(normalizeScope("project:p1")).toBe("project:p1");
    expect(normalizeScope("node:cos")).toBe("node:cos");
    expect(normalizeScope("bogus")).toBe("all");
  });

  it("parseScope: kind と id に分ける", () => {
    expect(parseScope("all")).toEqual({ kind: "all", id: null });
    expect(parseScope("project:p1")).toEqual({ kind: "project", id: "p1" });
    expect(parseScope("node:cos")).toEqual({ kind: "node", id: "cos" });
    expect(parseScope("bogus")).toEqual({ kind: "all", id: null });
  });

  it("scopeForProject / scopeForNode", () => {
    expect(scopeForProject("p1")).toBe("project:p1");
    expect(scopeForNode("cos")).toBe("node:cos");
  });
});

describe("findMentionQuery / matchMentionCandidates / applyMention（@node 補完）", () => {
  it("カーソル直前が @<語> なら拾う", () => {
    expect(findMentionQuery("@codi", 5)).toEqual({ query: "codi", start: 0, end: 5 });
  });

  it("先頭が空白でなければ拾わない（メールアドレス等と混同しない）", () => {
    expect(findMentionQuery("foo@bar", 7)).toBeNull();
  });

  it("空白のあとの @ なら拾う", () => {
    const text = "直して @codi";
    expect(findMentionQuery(text, text.length)).toEqual({ query: "codi", start: 4, end: text.length });
  });

  it("@ の後に空白が挟まれば止める（別の語になっている）", () => {
    expect(findMentionQuery("@codi ng", 8)).toBeNull();
  });

  it("matchMentionCandidates: id / name の大小無視の部分一致、上限で切る", () => {
    const nodes = [
      { id: "coding-poc", name: "コーディング" },
      { id: "coding-frontend", name: "フロントエンド" },
      { id: "research", name: "調査" },
    ];
    expect(matchMentionCandidates("codi", nodes).map((n) => n.id)).toEqual(["coding-poc", "coding-frontend"]);
    expect(matchMentionCandidates("フロント", nodes).map((n) => n.id)).toEqual(["coding-frontend"]);
    expect(matchMentionCandidates("", nodes, 2)).toHaveLength(2);
  });

  it("applyMention: @<id> に置き換え、末尾の空白の直後にカーソルを置く", () => {
    const match = findMentionQuery("直して @codi", 10);
    if (!match) throw new Error("expected a match");
    const applied = applyMention("直して @codi", match, "coding-poc");
    expect(applied.text).toBe("直して @coding-poc ");
    expect(applied.cursor).toBe(applied.text.length);
  });
});

describe("replyTargetForMessageBlock / buildInstructBody（返信先。G22 受け入れ条件 5）", () => {
  it("CoS 宛て・案件に紐づく human/reply ブロックは、その案件に紐づけたまま CoS へ続ける", () => {
    expect(replyTargetForMessageBlock({ node_id: "cos", project_id: "p1" })).toEqual({
      kind: "project",
      projectId: "p1",
    });
  });

  it("CoS 宛てでも案件が無ければノード扱い（cos 宛て）", () => {
    expect(replyTargetForMessageBlock({ node_id: "cos", project_id: null })).toEqual({ kind: "node", nodeId: "cos" });
  });

  it("部署ノード宛てはそのノードへ", () => {
    expect(replyTargetForMessageBlock({ node_id: "coding-poc", project_id: "p1" })).toEqual({
      kind: "node",
      nodeId: "coding-poc",
    });
  });

  it("buildInstructBody: 返信先があれば scope を付ける", () => {
    expect(buildInstructBody("続けて", { kind: "node", nodeId: "coding-poc" })).toEqual({
      text: "続けて",
      scope: "node:coding-poc",
    });
    expect(buildInstructBody("続けて", { kind: "project", projectId: "p1" })).toEqual({
      text: "続けて",
      scope: "project:p1",
    });
  });

  it("buildInstructBody: 返信先が無ければ、画面の既定の範囲（node/project scope）を使う", () => {
    expect(buildInstructBody("状況は？", null, "node:coding-poc")).toEqual({
      text: "状況は？",
      scope: "node:coding-poc",
    });
  });

  it("buildInstructBody: 返信先も既定の範囲も無ければ scope を付けない（celeris の既定 = CoS）", () => {
    expect(buildInstructBody("状況は？", null, null)).toEqual({ text: "状況は？" });
  });

  it("buildInstructBody: 本文が @<node-id> で始まれば scope を付けない（celeris の規則 2 に任せる。優先度が最も高い）", () => {
    expect(buildInstructBody("@coding-poc 直して", { kind: "node", nodeId: "other" }, "node:cos")).toEqual({
      text: "@coding-poc 直して",
    });
  });
});

describe("progressSummaryLine / taskLineSummary", () => {
  it("progressSummaryLine: 担当・harness・tier・経過・tool 回数・最後の status を 1 行に", () => {
    const line = progressSummaryLine(progressBlock());
    expect(line).toContain("research / fake / standard");
    expect(line).toContain("経過 24s");
    expect(line).toContain("tool 2 回");
    expect(line).toContain("最後: searching");
  });

  it("taskLineSummary: from → to（reason）・担当・harness・tier・mode・経過", () => {
    const line = taskLineSummary({
      task_id: "t1",
      title: "x",
      from: "running",
      to: "done",
      reason: "worker_done",
      assignee: "research",
      harness: "fake",
      tier: "standard",
      mode: "worktree",
      elapsed_secs: 33,
    });
    expect(line).toBe("running → done（worker_done） ・ research / fake / standard / worktree ・ 経過 33s");
  });
});

describe("consoleWaitingCounts（D4「未読の扉」）", () => {
  it("未回答の質問・未決の認可・milestone ブロックを数える", () => {
    const counts = consoleWaitingCounts([
      questionBlock({ answered: false }),
      questionBlock({ answered: true, cursor: "cq2" }),
      approvalBlock(),
      approvalBlock({ cursor: "ca2", approval: { ...approvalBlock().approval, id: "a2", decision: "once" } }),
      milestoneBlock(),
      replyBlock(),
    ]);
    expect(counts).toEqual({ questions: 1, approvals: 1, milestones: 1 });
  });

  it("何も待っていなければ全部 0", () => {
    expect(consoleWaitingCounts([replyBlock(), humanBlock()])).toEqual({ questions: 0, approvals: 0, milestones: 0 });
  });
});

describe("appendConsoleBlock（SSE の積み上げ）", () => {
  it("progress は同じ run_id の既存行を置き換える（積み増さない）", () => {
    const first = progressBlock({ cursor: "c3a", progress: { ...progressBlock().progress, count: 1 } });
    const items = appendConsoleBlock([humanBlock()], first);
    expect(items).toHaveLength(2);

    const updated = progressBlock({ cursor: "c3b", progress: { ...progressBlock().progress, count: 5 } });
    const next = appendConsoleBlock(items, updated);
    expect(next).toHaveLength(2);
    expect(next[1]).toBe(updated);
  });

  it("同じ cursor のブロックの再送は無視する", () => {
    const block = replyBlock();
    const items = appendConsoleBlock([humanBlock()], block);
    const next = appendConsoleBlock(items, { ...block });
    expect(next).toHaveLength(2);
  });

  it("ADR-0054 D2: 育つ返事（state=streaming）は同じ run_id/task_id の吹き出しに text/steps を積み増す", () => {
    const first = replyBlock({
      cursor: "cr1",
      run_id: "run-1",
      task_id: "t1",
      state: "streaming",
      thinking: "考え中…",
      text: "承知しま",
      steps: [{ kind: "tool_use", tool: "celerisctl", text: "knowledge search rust" }],
    });
    const items = appendConsoleBlock([humanBlock()], first);
    expect(items).toHaveLength(2);

    const second = replyBlock({
      cursor: "cr2",
      run_id: "run-1",
      task_id: "t1",
      state: "streaming",
      thinking: undefined,
      text: "した。",
      steps: [{ kind: "tool_result", text: "3 件" }],
    });
    const next = appendConsoleBlock(items, second);
    expect(next).toHaveLength(2);
    const reply = next[1];
    expect(reply.kind).toBe("reply");
    if (reply.kind !== "reply") throw new Error("unreachable");
    expect(reply.text).toBe("承知しました。");
    expect(reply.thinking).toBe("考え中…"); // 空の thinking は置き換えない
    expect(reply.steps).toHaveLength(2);
    expect(reply.steps?.[0].tool).toBe("celerisctl");
    expect(reply.steps?.[1].text).toBe("3 件");
  });

  it("ADR-0054 D2: 確定した返事（state=done）は育つ返事を積み増しではなく置き換える", () => {
    const streaming = replyBlock({
      cursor: "cr1",
      run_id: "run-1",
      task_id: "t1",
      state: "streaming",
      text: "承知しました",
      steps: [{ kind: "tool_use", tool: "celerisctl", text: "knowledge search rust" }],
    });
    const items = appendConsoleBlock([humanBlock()], streaming);

    const done = replyBlock({
      cursor: "cr-done",
      run_id: "run-1",
      task_id: "t1",
      state: "done",
      text: "承知しました。",
    });
    const next = appendConsoleBlock(items, done);
    expect(next).toHaveLength(2);
    expect(next[1]).toBe(done);
  });

  it("ADR-0054 D2: SSE の 4 段（thinking → tool_use → text → 確定）を順に足すと 1 つの吹き出しに育つ", () => {
    const [step1, step2, step3, done] = consoleGrowingReplySteps();
    let items: ConsoleBlock[] = [];

    items = appendConsoleBlock(items, step1);
    expect(items).toHaveLength(1);
    let reply = items[0];
    if (reply.kind !== "reply") throw new Error("unreachable");
    expect(reply.state).toBe("streaming");
    expect(reply.thinking).toBe("考え中…");
    expect(reply.steps).toHaveLength(0);
    expect(reply.text).toBe("");

    items = appendConsoleBlock(items, step2);
    expect(items).toHaveLength(1); // 同じ run_id/task_id なので新しいブロックにはならない
    reply = items[0];
    if (reply.kind !== "reply") throw new Error("unreachable");
    expect(reply.thinking).toBe("考え中…"); // step2 の thinking は null なので直前の値を保つ
    expect(reply.steps).toHaveLength(1);
    expect(reply.steps?.[0]).toMatchObject({ kind: "tool_use", tool: "celerisctl" });

    items = appendConsoleBlock(items, step3);
    expect(items).toHaveLength(1);
    reply = items[0];
    if (reply.kind !== "reply") throw new Error("unreachable");
    expect(reply.steps).toHaveLength(2);
    expect(reply.steps?.[1]).toMatchObject({ kind: "tool_result", text: "3 件" });
    expect(reply.text).toBe("承知しました。");

    // 完了して `state = "done"` の reply が届くと、育つ状態を引きずらず確定した本文に差し替わる。
    items = appendConsoleBlock(items, done);
    expect(items).toHaveLength(1);
    reply = items[0];
    if (reply.kind !== "reply") throw new Error("unreachable");
    expect(reply.state).toBe("done");
    expect(reply.text).toBe("承知しました。関連研究の調査から始めます。");
    expect(reply.steps ?? []).toHaveLength(0);
  });

  it("上限を超えたら古い方から落とす", () => {
    let items: ConsoleBlock[] = [];
    for (let i = 0; i < 5; i += 1) {
      items = appendConsoleBlock(items, humanBlock({ cursor: `h${i}`, message_id: `m${i}` }), 3);
    }
    expect(items).toHaveLength(3);
    expect(items.map((b) => b.cursor)).toEqual(["h2", "h3", "h4"]);
  });
});

describe("formatRunEventRow（「すべて見る」の行整形）", () => {
  const row = (event: EventRow["event"]): EventRow => ({
    id: 1,
    seq: 1,
    task_id: "t1",
    ts: "2026-09-20T01:00:00Z",
    event,
  });

  it("worker_progress の tool_use は tool + summary", () => {
    const line = formatRunEventRow(
      row({ type: "worker_progress", run_id: "r1", msg: "x", kind: "tool_use", tool: "Bash", summary: "cargo test" }),
    );
    expect(line.label).toBe("tool: Bash — cargo test");
    expect(line.kind).toBe("tool_use");
  });

  it("worker_progress の error フラグを運ぶ", () => {
    const line = formatRunEventRow(
      row({ type: "worker_progress", run_id: "r1", msg: "boom", kind: "tool_result", summary: "失敗", error: true }),
    );
    expect(line.error).toBe(true);
    expect(line.label).toBe("失敗");
  });

  it("summary が無ければ msg にフォールバックする", () => {
    const line = formatRunEventRow(row({ type: "worker_progress", run_id: "r1", msg: "生のメッセージ" }));
    expect(line.label).toBe("生のメッセージ");
  });

  it("その他の run イベントは種類ごとの短い説明にする", () => {
    expect(
      formatRunEventRow(row({ type: "worker_started", run_id: "r1", adapter: "claude-code", model: "opus" })).label,
    ).toBe("run 開始（claude-code / opus）");
    expect(formatRunEventRow(row({ type: "worker_finished", run_id: "r1", outcome: "done" })).label).toBe(
      "run 終了: done",
    );
    expect(
      formatRunEventRow(
        row({
          type: "artifact_produced",
          run_id: "r1",
          artifact: { kind: "doc", name: "report.md", path: "x", sha256: "x" },
        }),
      ).label,
    ).toBe("成果物: report.md");
  });
});
