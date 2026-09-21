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
