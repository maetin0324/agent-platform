import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { projectTitleFromText, replyArrived } from "~/lib/conversation";
import { TaskdClient } from "~/taskd/client.server";
import {
  buildMessagePostBody,
  loadConversation,
  runConversationAction,
  sendMessage,
  startProjectFromMessage,
} from "~/taskd/conversation.server";
import type { Message, MessageAccepted, MessageList, OrgList, Project, ProjectList } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * 秘書・各ノードとの対話（Phase G13b-2、ADR-0033 D4、docs/taskd-api-v1.md §3.54〜3.55）。
 * 実 taskd は起動せず、プロセス内の偽 taskd（`test/mock-taskd/server.ts`）だけを見る。
 */

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

const message = (over: Partial<Message> = {}): Message => ({
  id: "m1",
  node_id: "secretary",
  role: "user",
  text: "この案件をお願いします",
  created_at: "2026-09-17T00:00:00Z",
  ...over,
});

const project = (over: Partial<Project> = {}): Project => ({
  id: "01JPROJECT",
  title: "Pluvio の新テーマ",
  request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証",
  status: "active",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

function form(entries: Array<[string, string]>): FormData {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
}

describe("projectTitleFromText（本文の先頭 40 字が案件名）", () => {
  it("40 字までならそのまま（改行は空白に潰す）", () => {
    expect(projectTitleFromText("  Pluvio の新テーマ\n模索  ")).toBe("Pluvio の新テーマ 模索");
  });

  it("40 字を超えたら切って … を付ける", () => {
    const text = "あ".repeat(60);
    const title = projectTitleFromText(text);
    expect(Array.from(title)).toHaveLength(41); // 40 字 + …
    expect(title.endsWith("…")).toBe(true);
    expect(title.startsWith("あ".repeat(40))).toBe(true);
  });
});

describe("replyArrived（ポーリングの終了条件）", () => {
  const user = message({ id: "m1", role: "user" });
  const reply = message({ id: "m2", role: "node", text: "理解の確認です。", run_id: "r1" });

  it("送った発言がまだ一覧に出ていなければ待つ", () => {
    expect(replyArrived([], "m1")).toBe(false);
  });

  it("送った発言だけで返事がまだなら待つ", () => {
    expect(replyArrived([user], "m1")).toBe(false);
  });

  it("送った発言より後ろに node の返事が入ったら止める", () => {
    expect(replyArrived([user, reply], "m1")).toBe(true);
  });

  it("前の返事は数えない（今回送った発言より後ろだけを見る）", () => {
    const older = message({ id: "m0", role: "node", text: "先週の返事", run_id: "r0" });
    expect(replyArrived([older, user], "m1")).toBe(false);
  });

  it("どの発言か分からないとき（案件を作った直後）は、一覧の最後が node なら止める", () => {
    expect(replyArrived([user, reply], null)).toBe(true);
    expect(replyArrived([user], null)).toBe(false);
    expect(replyArrived([], null)).toBe(false);
  });
});

describe("loadConversation", () => {
  it("GET /org・GET /projects・GET /org/{id}/messages を束ねる（案件を選ぶと ?project= が付く）", async () => {
    const org: OrgList = {
      items: [
        {
          id: "secretary",
          parent_id: null,
          name: "秘書",
          kind: "secretary",
          position: 0,
          brief: "あなたの相手",
          created_at: "2026-09-17T00:00:00Z",
          updated_at: "2026-09-17T00:00:00Z",
        },
      ],
    };
    const messages: MessageList = {
      items: [message({ id: "m1" }), message({ id: "m2", role: "node", text: "理解の確認です。", run_id: "r1" })],
    };
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, org));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [project()] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org/secretary/messages", (_req, res) => sendJson(res, 200, messages));

    const result = await loadConversation(
      client,
      "secretary",
      new Request("http://gui.invalid/org/secretary?project=01JPROJECT"),
    );

    expect(result.nodeId).toBe("secretary");
    expect(result.node?.name).toBe("秘書");
    expect(result.projects).toHaveLength(1);
    expect(result.projectId).toBe("01JPROJECT");
    expect(result.messages).toEqual(messages.items);
    const asked = mock.requests.find((r) => r.url.startsWith("/api/v1/org/secretary/messages"));
    expect(asked?.url).toBe("/api/v1/org/secretary/messages?project=01JPROJECT&limit=200");
  });

  it("案件を選んでいなければ ?project= を送らない（案件なしの雑談）", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org/coding-poc/messages", (_req, res) =>
      sendJson(res, 200, { items: [] } satisfies MessageList),
    );

    // 選択肢「案件なし」を選んで GET フォームを送ると `?project=`（空）になる。それも「案件なし」として扱う。
    const result = await loadConversation(
      client,
      "coding-poc",
      new Request("http://gui.invalid/org/coding-poc?project="),
    );

    expect(result.projectId).toBeNull();
    expect(result.node).toBeNull();
    const asked = mock.requests.find((r) => r.url.startsWith("/api/v1/org/coding-poc/messages"));
    expect(asked?.url).toBe("/api/v1/org/coding-poc/messages?limit=200");
  });

  it("GET /org が落ちてもやり取りは出す（名前の表示にしか使わない）", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org/secretary/messages", (_req, res) =>
      sendJson(res, 200, { items: [message()] } satisfies MessageList),
    );

    const result = await loadConversation(client, "secretary", new Request("http://gui.invalid/org/secretary"));
    expect(result.node).toBeNull();
    expect(result.messages).toHaveLength(1);
  });

  it("404 org_node_not_found はそのまま投げる（ルートの ErrorBoundary が受ける）", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org/nobody/messages", (_req, res) =>
      sendProblem(res, { status: 404, code: "org_node_not_found", detail: "no such node: nobody" }),
    );

    await expect(
      loadConversation(client, "nobody", new Request("http://gui.invalid/org/nobody")),
    ).rejects.toMatchObject({ status: 404, code: "org_node_not_found", detail: "no such node: nobody" });
  });

  it("401 unauthorized もそのまま投げる", async () => {
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    mock.on("GET", "/api/v1/org/secretary/messages", (_req, res) =>
      sendJson(res, 200, { items: [] } satisfies MessageList),
    );

    await expect(
      loadConversation(client, "secretary", new Request("http://gui.invalid/org/secretary")),
    ).rejects.toMatchObject({ status: 401 });
  });
});

describe("buildMessagePostBody", () => {
  it("案件を選んでいれば project_id を載せる", () => {
    expect(
      buildMessagePostBody(
        form([
          ["text", "先週の続き"],
          ["project_id", "01JPROJECT"],
        ]),
      ),
    ).toEqual({
      text: "先週の続き",
      project_id: "01JPROJECT",
    });
  });

  it("案件なしなら project_id を送らない（雑談に案件の話を混ぜない）", () => {
    expect(
      buildMessagePostBody(
        form([
          ["text", "雑談"],
          ["project_id", ""],
        ]),
      ),
    ).toEqual({ text: "雑談" });
  });
});

describe("sendMessage（POST /org/{id}/messages。202 を素通しする）", () => {
  it("202 の {message_id, task_id} をそのまま返す", async () => {
    const accepted: MessageAccepted = { message_id: "01JMSG", task_id: "01JTASK" };
    mock.on("POST", "/api/v1/org/coding-poc/messages", (_req, res) => sendJson(res, 202, accepted));

    const outcome = await sendMessage(client, "coding-poc", { text: "先週の続き", project_id: "01JPROJECT" });

    expect(outcome).toEqual({ ok: true, op: "send", accepted });
    const posted = mock.requests.find((r) => r.method === "POST");
    expect(posted?.url).toBe("/api/v1/org/coding-poc/messages");
    expect(JSON.parse(posted?.body ?? "{}")).toEqual({ text: "先週の続き", project_id: "01JPROJECT" });
  });

  it("401 unauthorized（管理系）は文言をそのまま返す", async () => {
    mock.on("POST", "/api/v1/org/secretary/messages", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const outcome = await sendMessage(client, "secretary", { text: "x" });
    expect(outcome).toEqual({
      ok: false,
      op: "send",
      error: expect.objectContaining({ status: 401, code: "unauthorized" }) as unknown,
    });
  });

  it("404 org_node_not_found も文言をそのまま返す", async () => {
    mock.on("POST", "/api/v1/org/nobody/messages", (_req, res) =>
      sendProblem(res, { status: 404, code: "org_node_not_found", detail: "no such node: nobody" }),
    );
    const outcome = await sendMessage(client, "nobody", { text: "x" });
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) expect(outcome.error.detail).toBe("no such node: nobody");
  });
});

describe("startProjectFromMessage（秘書に話しかけて新しい案件にする）", () => {
  it("POST /projects を {title: 先頭 40 字, request: 本文} で呼ぶ", async () => {
    const text = "Pluvio を基盤に用いた新たな研究テーマの模索、検証。まずは関連研究を広く見てほしい。";
    mock.on("POST", "/api/v1/projects", (_req, res) => sendJson(res, 201, project()));

    const outcome = await startProjectFromMessage(client, text);

    expect(outcome).toEqual({ ok: true, op: "new_project", project: project() });
    const posted = mock.requests.find((r) => r.method === "POST");
    expect(JSON.parse(posted?.body ?? "{}")).toEqual({ title: projectTitleFromText(text), request: text });
    // 案件を作ると taskd が秘書の最初の返事を起こす（ADR-0033 D4）。GUI からは続けて話しかけない。
    expect(mock.requests.some((r) => r.url.endsWith("/messages"))).toBe(false);
  });

  it("422 validation（空文）は文言をそのまま返す", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "request must not be empty",
        extra: { errors: [{ field: "request", message: "must not be empty" }] },
      }),
    );
    const outcome = await startProjectFromMessage(client, "   ");
    expect(outcome.ok).toBe(false);
    if (!outcome.ok) {
      expect(outcome.error.status).toBe(422);
      expect(outcome.error.fields.request).toEqual(["must not be empty"]);
    }
  });
});

describe("runConversationAction（どちらを呼ぶかはフォームの値だけで決まる）", () => {
  it("「新しい案件として」が付いていれば POST /projects", async () => {
    mock.on("POST", "/api/v1/projects", (_req, res) => sendJson(res, 201, project()));
    const outcome = await runConversationAction(
      client,
      "secretary",
      form([
        ["text", "Pluvio の新テーマ"],
        ["new_project", "on"],
      ]),
    );
    expect(outcome).toEqual({ ok: true, op: "new_project", project: project() });
    expect(mock.requests.map((r) => r.url)).toEqual(["/api/v1/projects"]);
  });

  it("付いていなければ POST /org/{id}/messages", async () => {
    const accepted: MessageAccepted = { message_id: "01JMSG", task_id: "01JTASK" };
    mock.on("POST", "/api/v1/org/secretary/messages", (_req, res) => sendJson(res, 202, accepted));
    const outcome = await runConversationAction(
      client,
      "secretary",
      form([
        ["text", "状況を教えて"],
        ["project_id", "01JPROJECT"],
      ]),
    );
    expect(outcome).toEqual({ ok: true, op: "send", accepted });
    expect(mock.requests.map((r) => r.url)).toEqual(["/api/v1/org/secretary/messages"]);
  });
});
