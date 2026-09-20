import type { ApprovalOpOutcome, StandingRuleOpOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import { formString } from "./forms";
import type { ApprovalDecideBody, ApprovalDecideResult, Decision, StandingRule, StandingRuleCreateBody } from "./types";

/**
 * 「認可」画面（`/approvals`）からの決定・永続の認可の追加・削除（ADR-0033 D5、
 * docs/celeris-api-v1.md §3.56〜3.60。決める・作る・消すは**管理系**、`token_file` 未設定でも 401）。
 * GUI 側では検証しない: celeris が返す 400 / 404 / 422 / 401 をそのまま画面に出す
 * （`org-admin.server.ts` と同じ作り）。
 */

const DECISIONS: readonly Decision[] = ["once", "standing", "denied"];

function readDecision(form: FormData): Decision {
  const v = formString(form, "decision");
  return v && (DECISIONS as readonly string[]).includes(v) ? (v as Decision) : "once";
}

/**
 * 答えるフォーム（`answer` の欄 + 3 つのボタンのどれかの `name="decision"` + `scope`）。
 * `scope` は `decision = "standing"` のときだけ意味を持つ（§3.57）が、GUI は常に送る（送っても無害）。
 */
export function buildApprovalDecideInput(form: FormData): ApprovalDecideBody {
  const body: ApprovalDecideBody = {
    decision: readDecision(form),
    answer: formString(form, "answer") ?? "",
  };
  const scope = formString(form, "scope");
  if (scope) body.scope = scope;
  return body;
}

export async function decideApproval(
  client: CelerisClient,
  id: string,
  input: ApprovalDecideBody,
  signal?: AbortSignal,
): Promise<ApprovalOpOutcome> {
  try {
    const result = await client.post<ApprovalDecideResult>(`/approvals/${encodeURIComponent(id)}/decide`, input, {
      signal,
    });
    return { ok: true, op: "decide", id, result };
  } catch (e) {
    return { ok: false, op: "decide", id, error: toActionError(e) };
  }
}

/** 追加フォーム（`node_id`（空 = 全員向け） / `rule`）。 */
export function buildStandingRuleCreateInput(form: FormData): StandingRuleCreateBody {
  const body: StandingRuleCreateBody = { rule: formString(form, "rule") ?? "" };
  const nodeId = formString(form, "node_id");
  if (nodeId) body.node_id = nodeId;
  return body;
}

export async function createStandingRule(
  client: CelerisClient,
  input: StandingRuleCreateBody,
  signal?: AbortSignal,
): Promise<StandingRuleOpOutcome> {
  try {
    const rule = await client.post<StandingRule>("/standing-rules", input, { signal });
    return { ok: true, op: "create", id: rule.id, rule };
  } catch (e) {
    return { ok: false, op: "create", id: "", error: toActionError(e) };
  }
}

export async function deleteStandingRule(
  client: CelerisClient,
  id: string,
  signal?: AbortSignal,
): Promise<StandingRuleOpOutcome> {
  try {
    await client.delete<Record<string, never>>(`/standing-rules/${encodeURIComponent(id)}`, { signal });
    return { ok: true, op: "delete", id };
  } catch (e) {
    return { ok: false, op: "delete", id, error: toActionError(e) };
  }
}
