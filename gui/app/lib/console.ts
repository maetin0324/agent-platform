import type {
  ConsoleBlock,
  ConsolePage,
  ConsoleProgress,
  ConsoleTaskLine,
  EventRow,
  InstructBody,
  OrgNode,
  Project,
} from "~/celeris/types";
import { formatDuration, secondsBetween } from "~/lib/time-delta";

/**
 * Console 1 画面ぶんのデータ（`~/celeris/console.server.ts` の `loadConsole` が組む）。ここに置くのは
 * `~/components/Console.tsx`（component）が `.server.ts` を import せずに済むようにするため
 * （`~/lib/conversation.ts` の `ConversationData` と同じ理由。React Router の server-only 境界は
 * `.server.ts` のファイル単位なので、component は純粋な `~/lib/*` からだけ型を取る）。
 */
export interface ConsoleData {
  /** 正規化済みの範囲（`all` / `project:<id>` / `node:<id>`）。 */
  scope: string;
  page: ConsolePage;
  org: OrgNode[];
  projects: Project[];
}

/**
 * Console（ADR-0048 D1/D3/D4、Phase G22）の純粋な補助。ここには HTTP も React も持ち込まない
 * （`~/lib/reports.ts` / `~/lib/approvals.ts` と同じ方針）。celeris が返した値をそのまま出す・並べる・
 * まとめるだけで、新しい判断（誰に届くか等）は作らない（それは celeris の仕事。docs/adr/0048-console.md）。
 */

/** CoS（Chief of Staff。ADR-0046 D6）の組織ノード id。旧 `SECRETARY_NODE_ID`（`~/lib/conversation.ts`、削除済み）と同じ値。 */
export const COS_NODE_ID = "cos";

// ---------------------------------------------------------------------------
// 範囲（scope）
// ---------------------------------------------------------------------------

export type ConsoleScopeKind = "all" | "project" | "node";

export interface ParsedConsoleScope {
  kind: ConsoleScopeKind;
  id: string | null;
}

/** `?scope=` の値を celeris が受け付ける形（`all` / `project:<id>` / `node:<id>`）に正規化する。形が違えば `all`。 */
export function normalizeScope(raw: string | null | undefined): string {
  if (!raw || raw === "all") return "all";
  if (/^(project|node):.+/.test(raw)) return raw;
  return "all";
}

export function scopeForProject(projectId: string): string {
  return `project:${projectId}`;
}

export function scopeForNode(nodeId: string): string {
  return `node:${nodeId}`;
}

/** `normalizeScope` 済みの文字列を種類と id に分ける（表示・選択肢の初期値に使う）。 */
export function parseScope(scope: string): ParsedConsoleScope {
  if (scope === "all") return { kind: "all", id: null };
  const sep = scope.indexOf(":");
  if (sep < 0) return { kind: "all", id: null };
  const kind = scope.slice(0, sep);
  const id = scope.slice(sep + 1);
  if ((kind === "project" || kind === "node") && id) return { kind, id };
  return { kind: "all", id: null };
}

// ---------------------------------------------------------------------------
// `@node` 補完
// ---------------------------------------------------------------------------

export interface MentionQuery {
  /** `@` に続く文字列（`@` 自体は含まない）。 */
  query: string;
  /** `text` の中で `@` が始まる位置。 */
  start: number;
  /** カーソル位置（= `query` の終わり）。 */
  end: number;
}

/**
 * カーソルの直前が `@<語>`（語の中に空白を含まない）で終わっていれば、その語を返す。
 * `@` の直前は文字列の先頭か空白でなければならない（メールアドレスの `@` 等と混同しないため）。
 */
export function findMentionQuery(text: string, cursor: number): MentionQuery | null {
  const upto = text.slice(0, Math.max(0, Math.min(cursor, text.length)));
  const m = /(?:^|\s)@([^\s@]*)$/.exec(upto);
  if (!m) return null;
  const start = upto.length - m[0].length + (m[0].startsWith("@") ? 0 : 1);
  return { query: m[1], start, end: upto.length };
}

export interface MentionCandidate {
  id: string;
  name: string;
}

/** `id` か `name` に問い合わせ文字列を含むノードを、先頭から `limit` 件だけ返す（大小無視）。 */
export function matchMentionCandidates(
  query: string,
  nodes: readonly MentionCandidate[],
  limit = 8,
): MentionCandidate[] {
  const q = query.toLowerCase();
  const out: MentionCandidate[] = [];
  for (const n of nodes) {
    if (n.id.toLowerCase().includes(q) || n.name.toLowerCase().includes(q)) {
      out.push(n);
      if (out.length >= limit) break;
    }
  }
  return out;
}

/** 選んだ候補を本文に差し込む（`@<id> ` に置き換え、末尾の空白の直後にカーソルを置く）。 */
export function applyMention(text: string, match: MentionQuery, nodeId: string): { text: string; cursor: number } {
  const before = text.slice(0, match.start);
  const after = text.slice(match.end);
  const inserted = `@${nodeId} `;
  return { text: before + inserted + after, cursor: (before + inserted).length };
}

// ---------------------------------------------------------------------------
// 返信先（D1「各ブロックに『返信』」・G22 受け入れ条件 5）
// ---------------------------------------------------------------------------

/** `POST /console/instruct` へ続く返信先（`human` / `reply` ブロックの「返信」から作る）。 */
export type InstructReplyTarget = { kind: "node"; nodeId: string } | { kind: "project"; projectId: string };

/**
 * `human` / `reply` ブロックへの「返信」が指す先。CoS 宛て（`node_id === COS_NODE_ID`）かつ案件に
 * 紐づいていれば、その案件に紐づけたまま CoS へ続ける（`scope=project:<id>`）。それ以外はそのノード宛て
 * （`scope=node:<id>`）。`docs/adr/0048-console.md` §3.107 の「相手の決め方」の 3 と同じ判断。
 */
export function replyTargetForMessageBlock(block: {
  node_id: string;
  project_id?: string | null;
}): InstructReplyTarget {
  if (block.node_id === COS_NODE_ID && block.project_id) {
    return { kind: "project", projectId: block.project_id };
  }
  return { kind: "node", nodeId: block.node_id };
}

/** `@<node-id> ` で始まる本文（celeris の相手の決め方の規則 2）。`scope` を付けるとこの規則より
 * 優先されてしまう（§3.107 の規則は 1〜4 の順で先勝ち）ので、この形なら `scope` を付けずに任せる。 */
const MENTION_PREFIX = /^@[^\s@]+\s/;

/**
 * 入力欄の本文・返信先・（範囲がノード/案件に絞られた画面の）既定の範囲から `POST /console/instruct` の
 * 本文を組み立てる。優先順位（celeris の相手の決め方 §3.107 と矛盾しないように GUI 側で決める）:
 * 1. 本文が `@<node-id> ` で始まる → `scope` は付けない（celeris の規則 2 に任せる。「返信先」やこの画面の
 *    既定の範囲より、いま打った `@mention` を優先する）。
 * 2. 返信先（「返信」から選んだ先）があればそれ。
 * 3. 無ければ、いま見ている画面の既定の範囲（`node:<id>` / `project:<id>` に絞った Console。`scope=all` の
 *    Console では既定を付けない）。
 * 4. どれも無ければ `scope` を付けない（celeris の既定 = CoS、案件に紐づかない）。
 */
export function buildInstructBody(
  text: string,
  replyTarget: InstructReplyTarget | null,
  defaultScope?: string | null,
): InstructBody {
  const body: InstructBody = { text };
  if (MENTION_PREFIX.test(text)) return body;
  if (replyTarget?.kind === "node") body.scope = scopeForNode(replyTarget.nodeId);
  else if (replyTarget?.kind === "project") body.scope = scopeForProject(replyTarget.projectId);
  else if (defaultScope) body.scope = defaultScope;
  return body;
}

// ---------------------------------------------------------------------------
// 表示用の整形
// ---------------------------------------------------------------------------

/** `progress` ブロックの折り畳みの見出し 1 行（担当・harness・tier・経過・tool 回数・最後の status）。 */
export function progressElapsedSecs(progress: Pick<ConsoleProgress, "started_at" | "updated_at">): number {
  return secondsBetween(progress.started_at, progress.updated_at);
}

export function progressSummaryLine(block: {
  assignee?: string | null;
  harness?: string | null;
  tier: string;
  progress: ConsoleProgress;
}): string {
  const who = [block.assignee ?? "-", block.harness ?? "-", block.tier].join(" / ");
  const elapsed = formatDuration(progressElapsedSecs(block.progress));
  const status = block.progress.last_status ? ` ・ 最後: ${block.progress.last_status}` : "";
  return `${who} — 経過 ${elapsed} ・ tool ${block.progress.tool_count} 回${status}`;
}

/** `task` ブロックの 1 行（開始・終了・失敗・中止・割り込み）。 */
export function taskLineSummary(task: ConsoleTaskLine): string {
  const who = [task.assignee ?? "-", task.harness ?? "-", task.tier, task.mode ?? null].filter(Boolean).join(" / ");
  const elapsed = task.elapsed_secs != null ? ` ・ 経過 ${formatDuration(task.elapsed_secs)}` : "";
  return `${task.from} → ${task.to}（${task.reason}） ・ ${who}${elapsed}`;
}

// ---------------------------------------------------------------------------
// 上部の帯（質問・認可・途中目標の待ち件数。D4「未読の扉」）
// ---------------------------------------------------------------------------

export interface ConsoleWaitingCounts {
  questions: number;
  approvals: number;
  milestones: number;
}

/**
 * いま読み込んでいるブロックから、答え待ちの件数を数える（新しい API 呼び出しは足さない。G22 の方針）。
 * `question` は `answered = false`、`approval` は `decision` 未設定、`milestone` は celeris が
 * `proposed` のものしか流さない（`docs/adr/0048-console.md` Phase 60a 追記 7）ので、出ているだけ数える。
 */
export function consoleWaitingCounts(blocks: readonly ConsoleBlock[]): ConsoleWaitingCounts {
  let questions = 0;
  let approvals = 0;
  let milestones = 0;
  for (const b of blocks) {
    if (b.kind === "question" && !b.answered) questions += 1;
    else if (b.kind === "approval" && !b.approval.decision) approvals += 1;
    else if (b.kind === "milestone") milestones += 1;
  }
  return { questions, approvals, milestones };
}

// ---------------------------------------------------------------------------
// 育つ吹き出し（フェーズ 73、ADR-0055 D2 ラウンド 5）
// ---------------------------------------------------------------------------

/** いま流れているブロックの中に、育っている最中（`state === "streaming"`）の返事があるか。
 * 入力欄の「送信待ち（前の run が終わってから）」ヒント（ADR-0054 D2 のキュー）に使う。 */
export function hasStreamingReply(blocks: readonly ConsoleBlock[]): boolean {
  return blocks.some((b) => b.kind === "reply" && b.state === "streaming");
}

/** 複数行の文字列の 1 行目だけを返す（`tool_result` の折り畳みの「開く前に見える行」に使う）。 */
export function firstLine(text: string): string {
  const idx = text.indexOf("\n");
  return idx === -1 ? text : text.slice(0, idx);
}

/** `text` が 1 行目より長い（＝ `firstLine` の裏に隠れている内容がある）か。 */
export function hasMoreThanFirstLine(text: string): boolean {
  return text.length > firstLine(text).length;
}

/**
 * `console-stream`（`overflow-y-auto` の箱）の scroll 状態から、新しいブロックが来たときに
 * 自動で下まで追いかけてよいか（＝人が上にスクロールして読んでいる最中ではないか）を決める。
 * `threshold` 未満（既定 96px。フェーズ 71 からの値）まで下に居れば「最新に張り付いている」とみなす。
 */
export function shouldStickToBottom(
  scrollHeight: number,
  scrollTop: number,
  clientHeight: number,
  threshold = 96,
): boolean {
  return scrollHeight - scrollTop - clientHeight < threshold;
}

// ---------------------------------------------------------------------------
// SSE の積み上げ（`GET /console/stream` の `console.block` を既存の一覧に足す）
// ---------------------------------------------------------------------------

/**
 * SSE で届いた 1 ブロックを、いま画面にある一覧に足す。
 * - `progress` は run ごとに 1 行にまとめて表示する（D1「Console は progress を run ごとに束ねる」）ので、
 *   同じ `run_id` の既存の行があれば **その場で置き換える**（新しい 1 行として積み増さない）。
 * - ADR-0054 D2（Phase 68）: 育つ `reply`（`state = "streaming"`）も同じく `run_id` で置き換える
 *   （celeris が送る `text`/`thinking`/`steps` は**その接続で見た積み上げそのもの**なので、置き換えるだけで
 *   吹き出しが育つ。完了して `state = "done"` の `reply` が届いたときも同じ `run_id` なので、同じ位置で
 *   確定した本文に差し替わる＝「吹き出しが確定」）。
 * - それ以外で同じ `cursor`（= 同じブロック）が既にあれば、再送として無視する。
 * - `maxItems` を超えたら古い方から落とす（ブラウザのメモリを無限に増やさないための簡易な上限。
 *   越えて見たい場合は `GET /console` の `since` によるページングを別途足す余地がある。docs/PROGRESS.md 参照）。
 */
export function appendConsoleBlock(
  items: readonly ConsoleBlock[],
  incoming: ConsoleBlock,
  maxItems = 300,
): ConsoleBlock[] {
  if (incoming.kind === "progress") {
    const idx = items.findIndex((b) => b.kind === "progress" && b.progress.run_id === incoming.progress.run_id);
    if (idx >= 0) {
      const next = items.slice();
      next[idx] = incoming;
      return next;
    }
  } else if (incoming.kind === "reply" && incoming.run_id) {
    const idx = items.findIndex(
      (b) => b.kind === "reply" && b.run_id === incoming.run_id && b.task_id === incoming.task_id,
    );
    if (idx >= 0) {
      const existing = items[idx];
      const next = items.slice();
      // celeris は育つ返事（`state = "streaming"`）を**その接続で見た増分だけ**で送る（`text` はその回
      // に届いた分だけ、`steps` もその回の分だけ）。GUI 側で積み上げてはじめて 1 つの育つ吹き出しになる。
      // 確定した返事（`state = "done"`）は `messages` から来た本文そのもの（増分ではない）なので置き換える。
      next[idx] =
        incoming.state === "streaming" && existing.kind === "reply"
          ? {
              ...incoming,
              text: existing.text + incoming.text,
              thinking: incoming.thinking ?? existing.thinking,
              steps: [...(existing.steps ?? []), ...(incoming.steps ?? [])],
            }
          : incoming;
      return next;
    }
  } else if (items.some((b) => b.cursor === incoming.cursor)) {
    return items.slice();
  }
  const next =
    items.length >= maxItems ? [...items.slice(items.length - maxItems + 1), incoming] : [...items, incoming];
  return next;
}

// ---------------------------------------------------------------------------
// run の全行（「すべて見る」。`GET /tasks/{id}/runs/{run_id}/events`）
// ---------------------------------------------------------------------------

export interface RunEventLine {
  seq: number;
  at: string;
  kind: string;
  label: string;
  detail?: string | null;
  error?: boolean;
}

/** `EventsPage.items[]` の 1 行を、Console の progress 展開と同じ見た目の行に整形する。 */
export function formatRunEventRow(row: EventRow): RunEventLine {
  const e = row.event;
  if (e.type === "worker_progress") {
    const kind = e.kind ?? "status";
    const text = e.summary ?? e.msg;
    const label = kind === "tool_use" ? `tool: ${e.tool ?? "?"}${text ? ` — ${text}` : ""}` : text;
    return { seq: row.seq, at: row.ts, kind, label, detail: e.detail ?? null, error: e.error ?? false };
  }
  if (e.type === "worker_started") {
    return { seq: row.seq, at: row.ts, kind: "status", label: `run 開始（${e.adapter} / ${e.model}）` };
  }
  if (e.type === "worker_finished") {
    return { seq: row.seq, at: row.ts, kind: "status", label: `run 終了: ${e.outcome}` };
  }
  if (e.type === "artifact_produced") {
    return { seq: row.seq, at: row.ts, kind: "status", label: `成果物: ${e.artifact.name}` };
  }
  if (e.type === "review_verdict") {
    return {
      seq: row.seq,
      at: row.ts,
      kind: "status",
      label: `判定 #${e.criterion_idx}: ${e.pass ? "通過" : "不通過"} — ${e.reason}`,
    };
  }
  if (e.type === "question_raised") {
    return { seq: row.seq, at: row.ts, kind: "status", label: `質問: ${e.text}` };
  }
  if (e.type === "delegated") {
    return { seq: row.seq, at: row.ts, kind: "status", label: `委譲: ${e.task_ids.length} 件` };
  }
  return { seq: row.seq, at: row.ts, kind: "status", label: e.type };
}
