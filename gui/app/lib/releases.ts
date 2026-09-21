import type { DaemonInstance, ReleaseCommit, ReleaseItem, ReleaseRunning, Releases } from "~/celeris/types";
import type { IconName } from "~/components/ui/Icon";
import type { Tone } from "~/components/ui/tone";
import { instanceRoleLabel, sensitiveChangesLabel, staleChangesLabel } from "~/lib/labels";

/**
 * 「リリース」画面（`/releases`、Phase G14。ADR-0040 D6、docs/celeris-api-v1.md §3.66〜3.67）の純粋関数。
 *
 * この画面には DOM の unit テストが無い（G10-U1）ので、**表示の判断は全部ここに集めて**
 * `gui/test/unit/releases.test.ts` で試す（`~/lib/reports.ts` / `clusterConnectPanelState` と同じ方針）。
 *
 * GUI は判断をしない: 昇格できるかどうかの本当の判定は celeris と `promote.sh` が持っていて
 * （`verify.json.ok` が真でなければ拒否。`--force` は無い。ADR-0040 D2）、ここはその結果を
 * **先回りして同じ理由で灰色にするだけ**。押せてしまっても celeris が 409 で断る。
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

/**
 * ADR-0055 D2「状態はバッジ 1 語 + 色」向けの短い語（フェーズ 72）。`ok_live` / `ok_stop_start` は
 * どちらも「検証済み」の 1 語にし（切替方法の違いは色と、行の下の `releaseVerifyLabel` の詳細に任せる）、
 * `ng` は「検証NG」にする。`releaseVerifyLabel` はこのまま行の下の詳細文として使い続ける。
 */
const VERIFY_BADGE_LABEL: Record<ReleaseVerifyState, string> = {
  unverified: "未検証",
  ok_live: "検証済み",
  ok_stop_start: "検証済み",
  ng: "検証NG",
};

export function releaseVerifyBadgeLabel(item: Pick<ReleaseItem, "verify">): string {
  return VERIFY_BADGE_LABEL[releaseVerifyState(item)];
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

/**
 * U13（フェーズ 72 の未解決事項。ADR-0055 D2「状態はバッジ 1 語 + 色」）: `ok_live` と `ok_stop_start`
 * を同じ「検証済み」の 1 語にしたので、バッジの文字だけでは切替方法が分からなくなっていた。色（成功=緑・
 * 警告=黄）に加えて、アイコンでも見分けられるようにする（`releaseVerifyLabel` の詳細行は残したまま。
 * 色だけに頼らない、という判断は `STATUS_TONE` の「文字列は status 名をそのまま出す」と同じ考え方）。
 * `unverified`/`ng` はアイコンを付けない（バッジの文字だけで意味が通るため）。
 */
const VERIFY_ICON: Record<ReleaseVerifyState, IconName | null> = {
  unverified: null,
  ok_live: "zap",
  ok_stop_start: "rotate",
  ng: null,
};

export function releaseVerifyIcon(item: Pick<ReleaseItem, "verify">): IconName | null {
  return VERIFY_ICON[releaseVerifyState(item)];
}

/**
 * 昇格したときの切替方法（ADR-0040 D4）を 1 語で（ADR-0055 ラウンド 11、Phase 86）。
 * `releaseVerifyBadgeLabel` は `ok_live`/`ok_stop_start` をどちらも「検証済み」の 1 語にまとめている
 * （U13、色とアイコンで見分ける方針）が、こちらは逆にその 2 つを英語 1 語のまま出す
 * バッジ用（`live`/`stop-start`）。アイコン・色は既存の `releaseVerifyIcon`/`releaseVerifyTone` を
 * そのまま流用する（Phase 73 相当の割り当て。二重に定義しない）。
 */
export type ReleaseModeWord = "live" | "stop-start" | "unknown";

const MODE_WORD: Record<ReleaseVerifyState, ReleaseModeWord> = {
  unverified: "unknown",
  ok_live: "live",
  ok_stop_start: "stop-start",
  ng: "unknown",
};

export function releaseModeWord(item: Pick<ReleaseItem, "verify">): ReleaseModeWord {
  return MODE_WORD[releaseVerifyState(item)];
}

/**
 * 検証（`scripts/selfdeploy/verify.sh`）1〜6・4b の結果をコンパクトな一覧にする
 * （ADR-0055 D1-3「状態バッジは 1 語」、Phase 86）。
 *
 * `GET /releases` の `ReleaseVerify` は個別の検査結果を運ばず、`ok`（検査 1〜4・4b・6 が全部真）と
 * `live_ok`（検査 5、N-1 互換）の 2 つの集計値だけを持つ（`verify.sh` の doc コメントどおり）。
 * GUI は celeris が持たない内訳を作らない（gui/CLAUDE.md「派生値の再計算・判断ロジックの再実装」の禁止）
 * ので、**この 2 つの実在する値をそのまま 2 行として見せるだけ**（`ok` が偽でも「どの検査が落ちたか」は
 * 分からないので、6 個の偽の内訳を捏造しない）。
 */
export type ReleaseCheckWord = "通過" | "失敗" | "未実施";

export interface ReleaseVerifyCheckGroup {
  key: "main" | "n1";
  label: string;
  word: ReleaseCheckWord;
}

export function releaseVerifyCheckGroups(item: Pick<ReleaseItem, "verify">): ReleaseVerifyCheckGroup[] {
  const verify = item.verify;
  return [
    { key: "main", label: "検査 1〜4・4b・6", word: !verify ? "未実施" : verify.ok ? "通過" : "失敗" },
    { key: "n1", label: "検査 5（N-1 互換）", word: !verify ? "未実施" : verify.live_ok ? "通過" : "失敗" },
  ];
}

/** gate（`release.sh` の 7 段）の一言。 */
export function releaseGateLabel(item: Pick<ReleaseItem, "gate_ok">): string {
  return item.gate_ok ? "gate ✓" : "gate ✗";
}

/** ADR-0055 D2「状態はバッジ 1 語 + 色」向けの短い語（フェーズ 72）。詳細は `releaseGateLabel`。 */
export function releaseGateBadgeLabel(item: Pick<ReleaseItem, "gate_ok">): string {
  return item.gate_ok ? "通過" : "失敗";
}

/** リリースの「いまの位置」（現行 / 直前 / それ以外）。 */
export function releasePositionLabel(item: Pick<ReleaseItem, "is_current" | "is_previous">): string | null {
  if (item.is_current) return "現行";
  if (item.is_previous) return "直前";
  return null;
}

/**
 * 「昇格」ボタンを出すか・押せるか。`reason` が `null` のときだけ押せる。
 * 判定の順は celeris（`celeris::releases::start_promote`）と同じにしてあるので、文言もほぼ同じになる。
 */
export function promoteAvailability(item: ReleaseItem): { canPromote: boolean; reason: string | null } {
  if (item.is_current) return { canPromote: false, reason: "いま動いているリリースです" };
  if (item.promoting) return { canPromote: false, reason: "upgrade が走っています" };
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
      : "celeris と GUI をいったん停止してから起動し直します（数十秒、API と画面が止まります）";
  return `${item.sha12} に upgrade します。${how}。よろしいですか？`;
}

// ---- 昇格の前に何が変わるか（ADR-0041 D4。Phase G15）----------------------

/**
 * **安全に関わる変更**（`changes.sensitive`）を含むか。
 *
 * 判定は celeris の外（`scripts/selfdeploy/lib.sh` の `SD_SENSITIVE_PATTERNS`）で済んでいて、
 * GUI は**その結果が空かどうかを見るだけ**（パターンを GUI 側に写さない。判断を 2 か所に置かない）。
 */
export function hasSensitiveChanges(item: Pick<ReleaseItem, "changes">): boolean {
  return (item.changes?.sensitive?.length ?? 0) > 0;
}

/** 赤いバッジの文言（`sensitive` が空なら `null`）。 */
export function sensitiveBadgeText(item: Pick<ReleaseItem, "changes">): string | null {
  const n = item.changes?.sensitive?.length ?? 0;
  return n > 0 ? sensitiveChangesLabel(n) : null;
}

/**
 * 「昇格」を押す前に **sha12 を打たせるか**（ADR-0041 D4）。
 * 安全に関わる変更があるときだけ。無いときは従来どおり `window.confirm` の二重確認。
 */
export function promoteNeedsTypedSha(item: ReleaseItem): boolean {
  return hasSensitiveChanges(item);
}

/** 打たれた文字列が sha12 と一致するか（前後の空白は落とす。大文字小文字は区別しない）。 */
export function typedShaMatches(item: Pick<ReleaseItem, "sha12">, typed: string): boolean {
  return typed.trim().toLowerCase() === item.sha12.toLowerCase();
}

/** 差分の一行（コミット数・ファイル数。`changes.json` が無ければ `null`）。 */
export function changesSummaryText(item: Pick<ReleaseItem, "changes">): string | null {
  const c = item.changes;
  if (!c) return null;
  const base = c.base ? `${c.base} から` : "起点なし（current が無いときに作られたリリース）";
  return `${base} コミット ${c.commit_count} 件 / 変更ファイル ${c.file_count} 件`;
}

/** `changes.base` がいまの `current` と違う（この差分は「いま昇格したら」の話ではない）。 */
export function staleChangesText(item: Pick<ReleaseItem, "changes">): string | null {
  return item.changes?.stale ? staleChangesLabel(item.changes.base ?? null) : null;
}

/** コミットの短い sha（GUI は先頭 7 桁）。 */
export function commitShort(commit: Pick<ReleaseCommit, "sha">): string {
  return commit.sha.slice(0, 7);
}

/**
 * 本番のコードが `main` に戻っていないときの一行（ADR-0041 D3）。
 * **`current` の行にだけ**出す（他のリリースが `main` に居ないのは普通のこと）。
 * `on_main` が `null`（リポジトリが読めない）なら何も出さない。
 */
export function notOnMainText(item: Pick<ReleaseItem, "sha12" | "is_current" | "on_main">): string | null {
  if (!item.is_current || item.on_main !== false) return null;
  return `本番は main に未反映: git merge --ff-only ${item.sha12}`;
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
    return `upgrade 中: ${promoting.join(", ")}`;
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

/** 昇格の記録（`promoted.json`）の一行。一度も昇格していなければ `null`。 */
export function promotedAtText(item: Pick<ReleaseItem, "promoted_at">): string | null {
  return item.promoted_at ?? null;
}

/**
 * 直近の昇格の試みが失敗したか（バグ報告 2026-09-21: 押したあと成功も失敗も画面に出なかった）。
 * `promoting` の最中は「走っている」の帯（`release-promoting`）を優先するので、そちらが出ているときは
 * `false`（celeris 側が新しい試みを始めるときに `promote_failed.json` を消すので、実際には同時に
 * 両方立つことは無いはずだが、表示の優先順位として二重に出さない）。
 */
export function promoteFailedText(item: Pick<ReleaseItem, "promoting" | "promote_failed">): string | null {
  if (item.promoting || !item.promote_failed) return null;
  return item.promote_failed.error || "upgrade に失敗しました（詳しい原因は promote.log を見てください）。";
}

/**
 * 「upgrade を始めました」フラッシュ（`ReleasePromoteFlash`）をどう出すか（バグ報告 2026-09-21 その 2:
 * 押したあと成功したのか失敗したのか、現行のコミットハッシュが変わったのか画面から分からなかった）。
 *
 * - `succeeded`: `is_current` になった（＝`current` の symlink がこのリリースを指した。現行のコミット
 *   ハッシュ表示もこれで更新される）。「完了しました」に文言を差し替える。
 * - `in_progress`: まだ `promoting`（`promote.lock` の pid が生きている）。「始めました」のまま。
 * - `hidden`: 失敗して `promote_failed` が付いた。`release-promote-failed` の赤いバナーが別に出るので、
 *   ここで二重に出さない。
 * - `started`: 202 を受けた直後、まだ最初の再読み込みが来ていない一瞬。
 */
export type PromoteFlashState = "started" | "in_progress" | "succeeded" | "hidden";

export function promoteFlashState(
  item: Pick<ReleaseItem, "is_current" | "promoting" | "promote_failed">,
): PromoteFlashState {
  if (item.is_current) return "succeeded";
  if (item.promoting) return "in_progress";
  if (item.promote_failed) return "hidden";
  return "started";
}
