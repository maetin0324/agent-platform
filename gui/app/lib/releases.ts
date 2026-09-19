import type { Tone } from "~/components/ui/tone";
import { instanceRoleLabel } from "~/lib/labels";
import type { DaemonInstance, ReleaseItem, ReleaseRunning, Releases } from "~/taskd/types";

/**
 * 「リリース」画面（`/releases`、Phase G14。ADR-0040 D6、docs/taskd-api-v1.md §3.66〜3.67）の純粋関数。
 *
 * この画面には DOM の unit テストが無い（G10-U1）ので、**表示の判断は全部ここに集めて**
 * `gui/test/unit/releases.test.ts` で試す（`~/lib/reports.ts` / `clusterConnectPanelState` と同じ方針）。
 *
 * GUI は判断をしない: 昇格できるかどうかの本当の判定は taskd と `promote.sh` が持っていて
 * （`verify.json.ok` が真でなければ拒否。`--force` は無い。ADR-0040 D2）、ここはその結果を
 * **先回りして同じ理由で灰色にするだけ**。押せてしまっても taskd が 409 で断る。
 */

/** `ReleaseItem.verify` の 4 通り（ADR-0040 D3）。 */
export type ReleaseVerifyState = "unverified" | "ok_live" | "ok_stop_start" | "ng";

export function releaseVerifyState(item: Pick<ReleaseItem, "verify">): ReleaseVerifyState {
  const verify = item.verify;
  if (!verify) return "unverified";
  if (!verify.ok) return "ng";
  return verify.live_ok ? "ok_live" : "ok_stop_start";
}

const VERIFY_LABEL: Record<ReleaseVerifyState, string> = {
  unverified: "未検証",
  ok_live: "検証済み（ライブ引き継ぎ）",
  ok_stop_start: "検証済み（停止 → 起動）",
  ng: "検証に落ちました",
};

export function releaseVerifyLabel(item: Pick<ReleaseItem, "verify">): string {
  return VERIFY_LABEL[releaseVerifyState(item)];
}

const VERIFY_TONE: Record<ReleaseVerifyState, Tone> = {
  unverified: "neutral",
  ok_live: "success",
  ok_stop_start: "warning",
  ng: "danger",
};

export function releaseVerifyTone(item: Pick<ReleaseItem, "verify">): Tone {
  return VERIFY_TONE[releaseVerifyState(item)];
}

/** gate（`release.sh` の 7 段）の一言。 */
export function releaseGateLabel(item: Pick<ReleaseItem, "gate_ok">): string {
  return item.gate_ok ? "gate ✓" : "gate ✗";
}

/** リリースの「いまの位置」（現行 / 直前 / それ以外）。 */
export function releasePositionLabel(item: Pick<ReleaseItem, "is_current" | "is_previous">): string | null {
  if (item.is_current) return "現行";
  if (item.is_previous) return "直前";
  return null;
}

/**
 * 「昇格」ボタンを出すか・押せるか。`reason` が `null` のときだけ押せる。
 * 判定の順は taskd（`taskd::releases::start_promote`）と同じにしてあるので、文言もほぼ同じになる。
 */
export function promoteAvailability(item: ReleaseItem): { canPromote: boolean; reason: string | null } {
  if (item.is_current) return { canPromote: false, reason: "いま動いているリリースです" };
  if (item.promoting) return { canPromote: false, reason: "昇格が走っています" };
  if (item.problem) return { canPromote: false, reason: "リリースのファイルが読めません" };
  const state = releaseVerifyState(item);
  if (state === "unverified") {
    return { canPromote: false, reason: "未検証です（verify.sh を通してください）" };
  }
  if (state === "ng") return { canPromote: false, reason: "検証に落ちています" };
  return { canPromote: true, reason: null };
}

/** 昇格の確認文（`verify.live_ok` で切り替えの仕方が変わる。ADR-0040 D4）。 */
export function promoteConfirmText(item: ReleaseItem): string {
  const how =
    releaseVerifyState(item) === "ok_live"
      ? "動いている仕事を止めずに引き継ぎます（旧は手元の run を見終わってから終わります）"
      : "taskd と GUI をいったん停止してから起動し直します（数十秒、API と画面が止まります）";
  return `${item.sha12} に昇格します。${how}。よろしいですか？`;
}

/** `verify` されていない／`draining` 中のインスタンスを除いた「いま働いているもの」。 */
export function activeInstances(instances: DaemonInstance[]): DaemonInstance[] {
  return instances.filter((i) => i.role === "active");
}

export function drainingInstances(instances: DaemonInstance[]): DaemonInstance[] {
  return instances.filter((i) => i.role === "draining");
}

/**
 * 引き継ぎが進行中か（ADR-0040 D4）。**`GET /releases` を 2 秒ごとに読み直すかどうか**の判断に使う。
 * 進行中の印は 3 つ: どれかのリリースが `promoting`（`promote.lock` の pid が生きている）、
 * `draining` のインスタンスが居る、`active` が 2 つ以上居る（切り替わりの窓）。
 */
export function handoffInFlight(view: Pick<Releases, "items" | "instances">): boolean {
  if (view.items.some((i) => i.promoting)) return true;
  if (drainingInstances(view.instances).length > 0) return true;
  return activeInstances(view.instances).length > 1;
}

/** 引き継ぎの進行の一行（画面の帯に出す）。進行していなければ `null`。 */
export function handoffProgressText(view: Pick<Releases, "items" | "instances" | "running">): string | null {
  if (!handoffInFlight(view)) return null;
  const draining = drainingInstances(view.instances);
  const active = activeInstances(view.instances);
  const parts: string[] = [];
  for (const i of active) parts.push(`${i.release}: ${instanceRoleLabel(i.role)}`);
  for (const i of draining) parts.push(`${i.release}: ${instanceRoleLabel(i.role)}`);
  if (parts.length === 0) {
    const promoting = view.items.filter((i) => i.promoting).map((i) => i.sha12);
    return `昇格中: ${promoting.join(", ")}`;
  }
  return `切り替え中 — ${parts.join(" / ")}`;
}

/** いま動いているものの一行（`running` と `GET /health` は同じ値）。 */
export function runningSummary(running: ReleaseRunning): string {
  return `${running.release}（${instanceRoleLabel(running.role)}）`;
}

/** 一覧の 1 行に添える短い説明（`built_at` と `ref` と `schema_version`）。 */
export function releaseSubtitle(item: ReleaseItem): string {
  const parts: string[] = [];
  parts.push(item.built_at ?? "ビルド日時が読めません");
  if (item.ref) parts.push(item.ref);
  if (item.schema_version != null) parts.push(`schema ${item.schema_version}`);
  return parts.join(" · ");
}
