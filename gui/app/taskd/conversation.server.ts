import { type ConversationData, projectTitleFromText } from "~/lib/conversation";
import type { ConversationOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type { Inbox, MessageAccepted, MessageList, MessagePostBody, OrgList, Project, ProjectList } from "./types";

/**
 * 秘書・各ノードとの対話（`/org/:id`、`/org/secretary`。SPEC §3.1・§3.4・§4 の 1 と 2、ADR-0033 D4、
 * docs/taskd-api-v1.md §3.54〜3.55）の中継。
 * - `GET /org/{id}/messages?project=&limit=` は読み取り（管理系ではない）。`POST /org/{id}/messages` は**管理系**。
 * - 返事は 202 の後に非同期で入る。GUI は `POST` の応答（`{message_id, task_id}`）をそのまま画面へ渡し、
 *   `GET` を引き直して返事を待つ（`~/lib/conversation.ts`）。
 * - GUI 側では検証しない: taskd が 401 / 404 / 422 を返したらその文言をそのまま出す。
 */

/** 一度に読むやり取りの上限（API の上限は 500）。 */
export const CONVERSATION_MESSAGE_LIMIT = 200;

/**
 * 画面 1 枚ぶんをまとめて読む。`GET /org` だけは落ちても続ける（名前の表示にしか使わないため。
 * `routes/projects.$id.tsx` と同じ扱い）。`messages` と `projects` の 401 / 404 はそのまま投げる。
 */
export async function loadConversation(
  client: TaskdClient,
  nodeId: string,
  request: Request,
): Promise<ConversationData> {
  // 空の `?project=`（選択肢「案件なし」を選んで GET フォームを送った形）は「案件なし」として扱う
  // （そのまま送ると taskd が 404 `project_not_found` を返す。ULID でない文字列だから）。
  const raw = new URL(request.url).searchParams.get("project");
  const projectId = raw !== null && raw.length > 0 ? raw : null;
  const [org, projects, messages, inbox] = await Promise.all([
    client.get<OrgList>("/org", { signal: request.signal }).catch(() => ({ items: [] }) as OrgList),
    client.get<ProjectList>("/projects", { signal: request.signal }),
    client.get<MessageList>(`/org/${encodeURIComponent(nodeId)}/messages`, {
      query: { project: projectId, limit: CONVERSATION_MESSAGE_LIMIT },
      signal: request.signal,
    }),
    // 「考え中」の間に返事が作れない状態（経路なし等）になっていないかを見るため（監査 M1）。
    // 落ちても対話は出す（`GET /org` と同じ扱い）。
    client.get<Inbox>("/inbox", { signal: request.signal }).catch(() => null),
  ]);
  return {
    nodeId,
    node: org.items.find((n) => n.id === nodeId) ?? null,
    projects: projects.items,
    projectId,
    messages: messages.items,
    attention: inbox?.attention ?? [],
  };
}

/** `POST /org/{id}/messages`（**管理系**、202 `{message_id, task_id}`）。応答はそのまま画面へ渡す。 */
export async function sendMessage(
  client: TaskdClient,
  nodeId: string,
  body: MessagePostBody,
  signal?: AbortSignal,
): Promise<ConversationOpOutcome> {
  try {
    const accepted = await client.post<MessageAccepted>(`/org/${encodeURIComponent(nodeId)}/messages`, body, {
      signal,
    });
    return { ok: true, op: "send", accepted };
  } catch (e) {
    return { ok: false, op: "send", error: toActionError(e) };
  }
}

/**
 * 秘書に話しかけて**新しい案件**にする（SPEC §4 の 1「案件を投げる」）。`title` は本文の先頭 40 字、
 * `request` は本文そのまま。`POST /projects` の直後に秘書が最初の返事（理解確認・方針・最初の途中目標）を
 * 自分で返す（ADR-0033 D4、SPEC §7）ので、GUI はここで `POST /org/{id}/messages` を続けて呼ばない。
 */
export async function startProjectFromMessage(
  client: TaskdClient,
  text: string,
  signal?: AbortSignal,
): Promise<ConversationOpOutcome> {
  try {
    const project = await client.post<Project>(
      "/projects",
      { title: projectTitleFromText(text), request: text },
      { signal },
    );
    return { ok: true, op: "new_project", project };
  } catch (e) {
    return { ok: false, op: "new_project", error: toActionError(e) };
  }
}

/** 入力欄（`text` / `project_id` / `new_project`）を読む。空の `project_id` は送らない（案件なしの雑談）。 */
export function buildMessagePostBody(form: FormData): MessagePostBody {
  const body: MessagePostBody = { text: formString(form, "text") ?? "" };
  const projectId = formString(form, "project_id");
  if (projectId) body.project_id = projectId;
  return body;
}

/**
 * 対話画面の action 本体。「新しい案件として」が付いていれば `POST /projects`、そうでなければ
 * `POST /org/{id}/messages`。どちらにするかはフォームの値だけで決まる（GUI 側で本文を読んで判断しない）。
 */
export async function runConversationAction(
  client: TaskdClient,
  nodeId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<ConversationOpOutcome> {
  if (form.get("new_project") === "on") {
    return await startProjectFromMessage(client, formString(form, "text") ?? "", signal);
  }
  return await sendMessage(client, nodeId, buildMessagePostBody(form), signal);
}
