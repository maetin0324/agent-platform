import type { Message, OrgNode, Project } from "~/taskd/types";

/**
 * 秘書・各ノードとの対話（SPEC §3.4「組織の木を見て誰に言うかを決め、その担当に直接言う」、
 * ADR-0033 D4、docs/taskd-api-v1.md §3.54〜3.55）の純粋な補助。
 * ここには HTTP も React も持ち込まない（画面と server 側の両方から使い、単体で試せるようにする）。
 */

/** 返事を待つ間のポーリング間隔（ms）。返事は同期では返らない（202）ので `GET` を引き直す。 */
export const CONVERSATION_POLL_MS = 2_500;

/** これ以上待っても返事が無ければポーリングを止める（run が落ちて `messages` に何も入らない場合の保険）。 */
export const CONVERSATION_WAIT_LIMIT_MS = 10 * 60 * 1000;

/** 新しい案件の `title` に使う本文の先頭の長さ（SPEC §4「案件を投げる」を秘書との対話に統合するため）。 */
export const PROJECT_TITLE_LIMIT = 40;

/** 秘書のノード id（`/org/secretary` はこの人との対話。`POST /projects` の最初の返事も同じ相手）。 */
export const SECRETARY_NODE_ID = "secretary";

/** 対話画面 1 枚ぶんのデータ（loader が組む。`~/taskd/conversation.server.ts`）。 */
export interface ConversationData {
  /** 話し相手のノード id（URL から。`/org/secretary` は `secretary`） */
  nodeId: string;
  /** `GET /org` から引いた話し相手（名前・分野・brief の表示に使うだけ） */
  node: OrgNode | null;
  /** 案件の選択肢（`GET /projects`。「案件なし」は画面側が足す） */
  projects: Project[];
  /** 選んでいる案件（`?project=`。未選択＝案件に紐づかない雑談） */
  projectId: string | null;
  /** そのノード・その案件のやり取り（古い順） */
  messages: Message[];
}

/**
 * 本文から新しい案件の `title` を作る（先頭 40 字。超えたら `…` を付ける）。
 * 改行は空白に潰す（`title` は一覧の 1 行に出るため）。判断はこれだけで、依頼文（`request`）は本文そのまま。
 */
export function projectTitleFromText(text: string): string {
  const oneLine = text.replace(/\s+/g, " ").trim();
  const chars = Array.from(oneLine);
  if (chars.length <= PROJECT_TITLE_LIMIT) return oneLine;
  return `${chars.slice(0, PROJECT_TITLE_LIMIT).join("")}…`;
}

/**
 * 「考え中」を止めてよいか（ポーリングの終了条件）。
 * `userMessageId`（`POST` の 202 が返した `message_id`）より後ろに `role = "node"` の行が入ったら返事が来た。
 * `userMessageId` が無いとき（案件を作った直後など、どの行が自分の発言か分からないとき）は、
 * 一覧の最後が `node` の発言なら返事が来たとみなす。
 */
export function replyArrived(messages: Message[], userMessageId: string | null): boolean {
  if (userMessageId === null) {
    const last = messages[messages.length - 1];
    return last !== undefined && last.role === "node";
  }
  const index = messages.findIndex((m) => m.id === userMessageId);
  if (index < 0) return false; // 送った発言がまだ一覧に出ていない
  return messages.slice(index + 1).some((m) => m.role === "node");
}
