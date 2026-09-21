/**
 * `/accounts` の「LLM source」節（ADR-0053 D4、Phase 66）と `/clusters` のトンネル表示で使う純関数。
 * celeris が返した値（`LlmSourcesView` / `ClusterForwardView`）をそのまま表示用に整形するだけで、
 * 判断（到達性・残量・cooldown の計算）はしない（celeris が既に計算済み。ADR-0055 D2 と同じ規律）。
 */

import type { LlmSourceAccountView, LlmSourceView } from "~/celeris/types";
import { formatDuration } from "./time-delta";

/**
 * 供給元 id（`claude-oauth` / `codex-oauth` / `openai-compatible:<id>`）を人が読む見出しにする。
 * `openai-compatible:<id>` は `<id>` だけを出す（設定した名前をそのまま見せる）。
 */
export function sourceLabel(id: string): string {
  if (id === "claude-oauth") return "Claude";
  if (id === "codex-oauth") return "Codex";
  const relay = id.startsWith("openai-compatible:") ? id.slice("openai-compatible:".length) : null;
  return relay && relay.length > 0 ? relay : id;
}

/**
 * 状態バッジの一語（ADR-0055 D2 D1-3: 空白なし、12 字以内）。
 * `openai-compatible` は probe した到達性（`reachable`）、oauth のプールは `enabled` だけを見る
 * （到達性ではなくアカウントの残量で見る供給元なので、`reachable` は元から無い。`docs/gui/api.md` §3.108）。
 */
export function sourceStatusWord(source: Pick<LlmSourceView, "enabled" | "reachable">): string {
  if (!source.enabled) return "disabled";
  if (source.reachable === true) return "reachable";
  if (source.reachable === false) return "unreachable";
  return "enabled";
}

/** 0.0〜1.0 を % 表示に（測れないときは「不明」。値を捏造しない。ADR-0024 D3 と同じ規律）。 */
export function formatRemaining(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "不明";
  return `${Math.round(Math.min(1, Math.max(0, value)) * 100)}%`;
}

/** `celeris/<tier>` の解決先を人が読む形に（解決先が無ければ「供給元なし」）。 */
export function tierResolutionLabel(resolvesTo: string | null | undefined): string {
  if (!resolvesTo) return "供給元なし";
  return sourceLabel(resolvesTo);
}

/**
 * `celeris/<tier>` が今の供給元に解決した理由の一語（ADR-0055 ラウンド 11、`/accounts` の
 * 「LLM source」節）。**celeris の選択スコアを GUI 側で再計算しない**（gui/CLAUDE.md の禁止事項）:
 * ADR-0053 D1 が文書化した規則（(a) 到達可能な無料源を最優先 → (b) アカウントプールの残量）を、
 * `GET /llm/sources` が既に返している値（`kind`・`enabled`・`reachable`・`accounts[].cooldown_until`）
 * だけを読んで説明する。
 * - 解決先が無ければ `"no-source"`。
 * - 解決先が `openai-compatible`（無料中継。Qwen 等）なら `"free-first"`（D1(a) どおり最優先で選ばれた）。
 * - 解決先が oauth プールで、`openai-compatible` の供給元が設定されていて `reachable === false` なら
 *   `"unreachable"`（無料源が落ちているのでアカウントに倒れた）。
 * - 解決先が oauth プールで、そのプール内に cooldown 中のアカウントが 1 つでもあれば `"cooldown"`
 *   （どのアカウントが実際に選ばれたかは celeris だけが知っている。「このプールに cooldown 中の
 *   アカウントがいる」という既存の事実を示すだけで、選択の順位までは再現しない）。
 * - それ以外は `"unknown"`（無料源が無い・cooldown も無い、普通のプール選択）。
 */
export type TierResolutionReason = "free-first" | "cooldown" | "unreachable" | "no-source" | "unknown";

export function tierResolutionReason(
  resolvesTo: string | null | undefined,
  sources: readonly Pick<LlmSourceView, "id" | "kind" | "enabled" | "reachable" | "accounts">[],
  nowSec: number,
): TierResolutionReason {
  if (!resolvesTo) return "no-source";
  const resolved = sources.find((s) => s.id === resolvesTo);
  if (!resolved) return "unknown";
  if (resolved.kind === "openai-compatible") return "free-first";
  const freeSource = sources.find((s) => s.kind === "openai-compatible");
  if (freeSource?.enabled && freeSource.reachable === false) return "unreachable";
  if (resolved.accounts.some((a) => isAccountCoolingDown(a, nowSec))) return "cooldown";
  return "unknown";
}

const TIER_RESOLUTION_REASON_LABEL: Record<TierResolutionReason, string> = {
  "free-first": "無料の Qwen が使えるため優先しています",
  cooldown: "一部アカウントが cooldown 中です",
  unreachable: "Qwen が届かないため Claude / GPT に倒れています",
  "no-source": "使える供給元がありません",
  unknown: "通常のアカウント選択です",
};

/** `tierResolutionReason` を一行の日本語にする。 */
export function tierResolutionReasonLabel(reason: TierResolutionReason): string {
  return TIER_RESOLUTION_REASON_LABEL[reason];
}

/** `"frontier"` / `"standard"` / `"cheap"` を画面の見出しに（未知の値はそのまま）。 */
export function tierLabel(tier: string): string {
  const known: Record<string, string> = { frontier: "frontier", standard: "standard", cheap: "cheap" };
  return known[tier] ?? tier;
}

/**
 * Unix 秒の `cooldown_until` を、`nowSec`（`fetchedAt` の秒）から見た残り時間に（celeris の
 * `AccountCooldownView.until` は RFC 3339 だが、`LlmSourceAccountView.cooldown_until` は Unix 秒
 * なので変換が要る。celeris の側の型はそのまま、GUI 側だけの表示変換）。
 */
export function cooldownRemainingLabel(cooldownUntilSec: number, nowSec: number): string {
  const remaining = cooldownUntilSec - nowSec;
  if (remaining <= 0) return "切れています";
  return formatDuration(remaining);
}

/** アカウント 1 件が cooldown 中か（`cooldown_until` が未来か）。 */
export function isAccountCoolingDown(account: Pick<LlmSourceAccountView, "cooldown_until">, nowSec: number): boolean {
  return account.cooldown_until != null && account.cooldown_until > nowSec;
}

/**
 * cooldown の絶対時刻（`title` 属性用の RFC 3339。ADR-0055 D2「相対時刻＋`title` に絶対時刻」の規律を
 * Unix 秒の `cooldown_until` にもそのまま適用する）。値を捏造しない: 呼び出し側は
 * `isAccountCoolingDown`/`cooldown_until != null` を確認してから使うこと。
 */
export function cooldownUntilTitle(cooldownUntilSec: number): string {
  return new Date(cooldownUntilSec * 1000).toISOString();
}

/**
 * ある供給元のアカウントが 1 件以上あり、**全員**が cooldown 中か（`/accounts` の「Claude のアカウントが
 * 全部 cooldown 中」の警告カード用）。0 件（アカウントが無い）は「全員 cooldown 中」とは言えないので false。
 */
export function allAccountsCoolingDown(
  accounts: readonly Pick<LlmSourceAccountView, "cooldown_until">[],
  nowSec: number,
): boolean {
  return accounts.length > 0 && accounts.every((a) => isAccountCoolingDown(a, nowSec));
}

/**
 * `/clusters` の port forward（ADR-0053 D3、Phase 66。listener/target の分離は Phase 85）1 本の
 * 状態バッジの一語。`up` が `null`（まだ観測が無い。celeris 再起動直後など）は「unknown」にする
 * （値を捏造しない）。`listener === true` かつ `target_healthy === false` は「転送はあるが先方が
 * 応答しない」（celeris は再発行しない。ADR-0053 Phase 85）ので、`down`（転送自体が無い）とは
 * 別の一語 `unreachable` にする。
 */
export function forwardStatusWord(forward: {
  up?: boolean | null;
  listener?: boolean | null;
  target_healthy?: boolean | null;
}): string {
  if (forward.up === true) return "up";
  if (forward.listener === true && forward.target_healthy === false) return "unreachable";
  if (forward.up === false) return "down";
  return "unknown";
}
