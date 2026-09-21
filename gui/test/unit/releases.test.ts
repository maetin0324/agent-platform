import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { promoteRelease } from "~/celeris/releases-admin.server";
import type { ReleaseItem, Releases } from "~/celeris/types";
import { instanceRoleLabel } from "~/lib/labels";
import {
  changesSummaryText,
  commitShort,
  handoffInFlight,
  handoffProgressText,
  hasSensitiveChanges,
  notOnMainText,
  promoteAvailability,
  promoteConfirmText,
  promotedAtText,
  promoteFailedText,
  promoteFlashState,
  promoteNeedsTypedSha,
  releaseGateBadgeLabel,
  releaseGateLabel,
  releasePositionLabel,
  releaseSubtitle,
  releaseVerifyBadgeLabel,
  releaseVerifyLabel,
  releaseVerifyState,
  releaseVerifyTone,
  runningSummary,
  sensitiveBadgeText,
  staleChangesText,
  typedShaMatches,
} from "~/lib/releases";
import { loadReleases } from "~/routes/releases";
import { defaultReleasePromoteAccepted, defaultReleases, releaseChanges, releaseItem } from "../mock-celeris/fixtures";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

/**
 * 「リリース」画面（`/releases`、Phase G14。ADR-0040 D6、docs/celeris-api-v1.md §3.66〜3.67）。
 *
 * - `~/lib/releases.ts` の純粋関数（検証状態の出し分け、昇格できるかどうか、引き継ぎの進行の一行）
 * - `loadReleases`（`GET /releases` をそのまま返す。並びは celeris の順を崩さない）
 * - `promoteRelease`（202 / 404 / 409 / 401 を `{ok:false, error}` として返す）
 *
 * 外部ネットワークには出ない（`test/mock-celeris/server.ts` の loopback サーバだけ）。
 */

/** `test/mock-celeris/fixtures.ts` の既定を上書きする短縮形。 */
const item = (overrides: Partial<ReleaseItem> = {}): ReleaseItem => releaseItem(overrides);
const releasesView: Releases = defaultReleases;

describe("releaseVerifyState / ラベル（ADR-0040 D3）", () => {
  it("verify.json が無ければ未検証", () => {
    expect(releaseVerifyState(item({ verify: null }))).toBe("unverified");
    expect(releaseVerifyLabel(item({ verify: null }))).toBe("未検証");
    expect(releaseVerifyTone(item({ verify: null }))).toBe("neutral");
  });

  it("ok && live_ok はライブ引き継ぎ、ok && !live_ok は停止 → 起動", () => {
    expect(releaseVerifyState(item())).toBe("ok_live");
    expect(releaseVerifyLabel(item())).toBe("検証済み（ライブ引き継ぎ）");
    expect(releaseVerifyTone(item())).toBe("success");

    const stopStart = item({ verify: { ok: true, live_ok: false, at: null } });
    expect(releaseVerifyState(stopStart)).toBe("ok_stop_start");
    expect(releaseVerifyLabel(stopStart)).toBe("検証済み（停止 → 起動）");
    expect(releaseVerifyTone(stopStart)).toBe("warning");
  });

  it("ok が偽なら検証に落ちている", () => {
    const ng = item({ verify: { ok: false, live_ok: false, at: "2026-09-19T01:00:00Z" } });
    expect(releaseVerifyState(ng)).toBe("ng");
    expect(releaseVerifyLabel(ng)).toBe("検証に落ちました");
    expect(releaseVerifyTone(ng)).toBe("danger");
  });

  it("gate と 現行 / 直前 の印", () => {
    expect(releaseGateLabel(item())).toBe("gate ✓");
    expect(releaseGateLabel(item({ gate_ok: false }))).toBe("gate ✗");
    expect(releasePositionLabel(item({ is_current: true }))).toBe("現行");
    expect(releasePositionLabel(item({ is_previous: true }))).toBe("直前");
    expect(releasePositionLabel(item())).toBeNull();
  });

  it("フェーズ 72（ADR-0055 D2）: バッジは 1 語（gate/verify とも、空白を含まない）", () => {
    expect(releaseGateBadgeLabel(item())).toBe("通過");
    expect(releaseGateBadgeLabel(item({ gate_ok: false }))).toBe("失敗");
    expect(releaseGateBadgeLabel(item())).not.toMatch(/\s/);
    expect(releaseGateBadgeLabel(item({ gate_ok: false }))).not.toMatch(/\s/);

    expect(releaseVerifyBadgeLabel(item({ verify: null }))).toBe("未検証");
    // ok_live / ok_stop_start は同じ 1 語（切替方法の違いは色と releaseVerifyLabel の詳細に任せる）。
    expect(releaseVerifyBadgeLabel(item())).toBe("検証済み");
    expect(releaseVerifyBadgeLabel(item({ verify: { ok: true, live_ok: false, at: null } }))).toBe("検証済み");
    const ng = item({ verify: { ok: false, live_ok: false, at: "2026-09-19T01:00:00Z" } });
    expect(releaseVerifyBadgeLabel(ng)).toBe("検証NG");
    for (const label of [
      releaseVerifyBadgeLabel(item({ verify: null })),
      releaseVerifyBadgeLabel(item()),
      releaseVerifyBadgeLabel(ng),
    ]) {
      expect(label).not.toMatch(/\s/);
    }
  });

  it("1 行の説明は built_at · ref · schema", () => {
    expect(releaseSubtitle(item())).toBe("2026-09-19T00:00:00Z · main · schema 11");
    expect(releaseSubtitle(item({ built_at: null, ref: null, schema_version: null }))).toBe("ビルド日時が読めません");
  });
});

describe("promoteAvailability（celeris の 409 と同じ理由で先回りして止める）", () => {
  it("検証済みで current でも昇格中でもなければ押せる", () => {
    expect(promoteAvailability(item())).toEqual({ canPromote: true, reason: null });
  });

  it("current は押せない", () => {
    const r = promoteAvailability(item({ is_current: true }));
    expect(r.canPromote).toBe(false);
    expect(r.reason).toContain("いま動いている");
  });

  it("upgrade 中は押せない", () => {
    const r = promoteAvailability(item({ promoting: true }));
    expect(r.canPromote).toBe(false);
    expect(r.reason).toContain("upgrade が走っています");
  });

  it("未検証・検証落ちは押せない", () => {
    expect(promoteAvailability(item({ verify: null })).canPromote).toBe(false);
    expect(promoteAvailability(item({ verify: null })).reason).toContain("未検証");
    expect(promoteAvailability(item({ verify: { ok: false, live_ok: false, at: null } })).canPromote).toBe(false);
  });

  it("manifest/gate が読めないリリースは押せない", () => {
    const broken = item({ problem: "manifest.json is missing or invalid", gate_ok: false, verify: null });
    expect(promoteAvailability(broken).canPromote).toBe(false);
    expect(promoteAvailability(broken).reason).toContain("読めません");
  });

  it("確認文は切り替え方（ライブ / 停止 → 起動）で変わる", () => {
    expect(promoteConfirmText(item())).toContain("止めずに");
    expect(promoteConfirmText(item({ verify: { ok: true, live_ok: false, at: null } }))).toContain("停止");
  });
});

describe("昇格の前に何が変わるか（ADR-0041 D4。Phase G15）", () => {
  it("changes が無いリリース（Phase 48 以前）は差分も安全の印も出ない", () => {
    const plain = item({ changes: null });
    expect(changesSummaryText(plain)).toBeNull();
    expect(hasSensitiveChanges(plain)).toBe(false);
    expect(sensitiveBadgeText(plain)).toBeNull();
    expect(promoteNeedsTypedSha(plain)).toBe(false);
    expect(staleChangesText(plain)).toBeNull();
  });

  it("差分の一行は 起点 · コミット数 · ファイル数", () => {
    expect(changesSummaryText(item({ changes: releaseChanges() }))).toBe(
      "aaaaaaaaaaaa から コミット 2 件 / 変更ファイル 3 件",
    );
    expect(changesSummaryText(item({ changes: releaseChanges({ base: null }) }))).toContain("起点なし");
  });

  it("sensitive が空でなければ赤いバッジの文言が出る（判定は celeris 側の結果を読むだけ）", () => {
    const safe = item({ changes: releaseChanges() });
    expect(hasSensitiveChanges(safe)).toBe(false);
    expect(sensitiveBadgeText(safe)).toBeNull();

    const risky = item({
      changes: releaseChanges({ sensitive: ["scripts/selfdeploy/verify.sh", "crates/celeris/src/releases.rs"] }),
    });
    expect(hasSensitiveChanges(risky)).toBe(true);
    expect(sensitiveBadgeText(risky)).toBe("安全に関わる変更 2 件");
  });

  it("安全に関わる変更があるときだけ sha12 の入力を求め、一致するまで押せない", () => {
    const risky = item({ sha12: "abcdef123456", changes: releaseChanges({ sensitive: ["config/config.toml"] }) });
    expect(promoteNeedsTypedSha(risky)).toBe(true);
    expect(typedShaMatches(risky, "")).toBe(false);
    expect(typedShaMatches(risky, "abcdef12345")).toBe(false);
    expect(typedShaMatches(risky, "abcdef123456")).toBe(true);
    // 前後の空白は落とし、大文字小文字は区別しない（貼り付けで通る）。
    expect(typedShaMatches(risky, "  ABCDEF123456 \n")).toBe(true);

    // 安全に関わる変更が無ければ従来どおり（`window.confirm` の二重確認だけ）。
    expect(promoteNeedsTypedSha(item({ changes: releaseChanges() }))).toBe(false);
  });

  it("base が今の current と違えば「この差分は古い」と断る", () => {
    const stale = item({ changes: releaseChanges({ stale: true, base: "999999999999" }) });
    expect(staleChangesText(stale)).toContain("999999999999");
    expect(staleChangesText(item({ changes: releaseChanges() }))).toBeNull();
  });

  it("コミットは先頭 7 桁で出す", () => {
    expect(commitShort({ sha: "1111111111111111111111111111111111111111" })).toBe("1111111");
  });
});

describe("main への反映と昇格の記録（ADR-0041 D3）", () => {
  it("current が main に入っていなければ ff-only の手順を出す", () => {
    const behind = item({ sha12: "70e3175eeb20", is_current: true, on_main: false });
    expect(notOnMainText(behind)).toBe("本番は main に未反映: git merge --ff-only 70e3175eeb20");
  });

  it("current でない行・main に入っている行・分からない行には出さない", () => {
    expect(notOnMainText(item({ is_current: false, on_main: false }))).toBeNull();
    expect(notOnMainText(item({ is_current: true, on_main: true }))).toBeNull();
    // リポジトリが読めない（null）ときは黙る（「未反映だ」と決めつけない）。
    expect(notOnMainText(item({ is_current: true, on_main: null }))).toBeNull();
  });

  it("promoted_at は promoted.json があるときだけ", () => {
    expect(promotedAtText(item({ promoted_at: "2026-09-19T12:31:36Z" }))).toBe("2026-09-19T12:31:36Z");
    expect(promotedAtText(item())).toBeNull();
  });

  // バグ報告 2026-09-21: 昇格ボタンを押したあと、失敗しても画面に何も出なかった
  // （promote.sh が set -e で死ぬだけで、`promoting` が偽に戻るのと見分けがつかなかった）。
  it("promote_failed があれば、その内容を出す", () => {
    const failed = item({
      promote_failed: { failed_at: "2026-09-19T12:31:36Z", error: "old celeris is still serving" },
    });
    expect(promoteFailedText(failed)).toBe("old celeris is still serving");
  });

  it("一度も失敗していなければ null", () => {
    expect(promoteFailedText(item())).toBeNull();
  });

  it("走っている最中は（promote_failed が残っていても）出さない", () => {
    const stillRunning = item({
      promoting: true,
      promote_failed: { failed_at: "2026-09-19T12:31:36Z", error: "old celeris is still serving" },
    });
    expect(promoteFailedText(stillRunning)).toBeNull();
  });

  it("error が空文字でも、失敗したこと自体は伝える文言を出す", () => {
    const failed = item({ promote_failed: { failed_at: "2026-09-19T12:31:36Z", error: "" } });
    expect(promoteFailedText(failed)).toBe("upgrade に失敗しました（詳しい原因は promote.log を見てください）。");
  });
});

// バグ報告 2026-09-21 その 2: 押したあと、成功したのか失敗したのか、現行のコミットハッシュが変わったのか
// 画面から分からなかった。`ReleasePromoteFlash` はこの状態で「始めました」→「完了しました」に文言を差し替え、
// 失敗（`release-promote-failed` の赤いバナーが別に出る）のときは二重に出さない。
describe("promoteFlashState（upgrade の 202 フラッシュをいつ「完了しました」に差し替えるか）", () => {
  it("is_current になれば succeeded（現行のコミットハッシュ表示もこれで更新される）", () => {
    expect(promoteFlashState(item({ is_current: true, promoting: false }))).toBe("succeeded");
    // 走っている最中に current であることは無いはずだが、succeeded を優先する。
    expect(promoteFlashState(item({ is_current: true, promoting: true }))).toBe("succeeded");
  });

  it("promoting の間は in_progress", () => {
    expect(promoteFlashState(item({ is_current: false, promoting: true }))).toBe("in_progress");
  });

  it("promote_failed が付いたら hidden（赤いバナーに任せる）", () => {
    expect(
      promoteFlashState(
        item({
          is_current: false,
          promoting: false,
          promote_failed: { failed_at: "2026-09-19T12:31:36Z", error: "boom" },
        }),
      ),
    ).toBe("hidden");
  });

  it("202 直後、まだどちらでもなければ started", () => {
    expect(promoteFlashState(item({ is_current: false, promoting: false, promote_failed: null }))).toBe("started");
  });
});

describe("引き継ぎの進行（ADR-0040 D4）", () => {
  it("active が 1 つだけなら進行中ではない", () => {
    expect(handoffInFlight(releasesView)).toBe(false);
    expect(handoffProgressText(releasesView)).toBeNull();
  });

  it("draining が居れば進行中（旧 引き継ぎ中 / 新 稼働中 を出す）", () => {
    const during: Releases = {
      ...releasesView,
      instances: [
        { ...releasesView.instances[0], role: "draining" },
        {
          instance_id: "01MOCKCELERISINSTANCE00002",
          release: "bbbbbbbbbbbb",
          pid: 222,
          role: "active",
          started_at: "2026-09-19T09:00:00Z",
          heartbeat_at: "2026-09-19T09:00:05Z",
        },
      ],
    };
    expect(handoffInFlight(during)).toBe(true);
    const text = handoffProgressText(during) ?? "";
    expect(text).toContain("bbbbbbbbbbbb");
    expect(text).toContain(instanceRoleLabel("active"));
    expect(text).toContain(instanceRoleLabel("draining"));
  });

  it("promote.lock が生きている（promoting）だけでも進行中", () => {
    const promoting: Releases = { ...releasesView, items: [item({ promoting: true })], instances: [] };
    expect(handoffInFlight(promoting)).toBe(true);
    expect(handoffProgressText(promoting)).toContain("upgrade 中");
  });

  it("running の一行は役割を日本語で出す", () => {
    expect(runningSummary(releasesView.running)).toBe("aaaaaaaaaaaa（稼働中）");
  });
});

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});
afterEach(async () => {
  await mock.close();
});

describe("loadReleases", () => {
  it("GET /releases を呼び、応答をそのまま（並びも変えずに）返す", async () => {
    mock.on("GET", "/api/v1/releases", (_req, res) => sendJson(res, 200, releasesView));
    const result = await loadReleases(client, new Request("http://gui.invalid/releases"));
    expect(result.releases).toEqual(releasesView);
    expect(result.releases.items.map((i) => i.sha12)).toEqual(["bbbbbbbbbbbb", "aaaaaaaaaaaa"]);
    expect(result.fetchedAt).toMatch(/T/);
    expect(mock.requests.some((r) => r.method === "GET" && r.url === "/api/v1/releases")).toBe(true);
  });

  it("空の一覧（リリースがまだ 1 つも無い）でも落ちない", async () => {
    const empty: Releases = { ...releasesView, current: null, previous: null, instances: [], items: [] };
    mock.on("GET", "/api/v1/releases", (_req, res) => sendJson(res, 200, empty));
    const result = await loadReleases(client, new Request("http://gui.invalid/releases"));
    expect(result.releases.items).toEqual([]);
    expect(handoffInFlight(result.releases)).toBe(false);
  });

  it("celeris に繋がらなければ loader が Response に変換できるよう reject する", async () => {
    const closed = await startMockCeleris();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new CelerisClient({ baseUrl, timeoutMs: 1000 });
    await expect(loadReleases(unreachable, new Request("http://gui.invalid/releases"))).rejects.toBeTruthy();
  });
});

describe("promoteRelease（POST /releases/{sha12}/promote。管理系）", () => {
  it("202 をそのまま返す（本文は空の JSON を送る）", async () => {
    const accepted = defaultReleasePromoteAccepted;
    mock.on("POST", "/api/v1/releases/bbbbbbbbbbbb/promote", (_req, res) => sendJson(res, 202, accepted));
    const outcome = await promoteRelease(client, "bbbbbbbbbbbb");
    expect(outcome).toEqual({ ok: true, op: "release_promote", sha12: "bbbbbbbbbbbb", accepted });
    // ADR-0041 D4: どちらの `promote.sh` が走ったかも落とさずに運ぶ。
    expect(accepted.script_from).toBe("current");
    const req = mock.requests.find((r) => r.method === "POST" && r.url === "/api/v1/releases/bbbbbbbbbbbb/promote");
    expect(JSON.parse(req?.body ?? "null")).toEqual({});
  });

  it("404 release_not_found は ActionError にして返す（例外にしない）", async () => {
    mock.on("POST", "/api/v1/releases/cccccccccccc/promote", (_req, res) =>
      sendProblem(res, { status: 404, code: "release_not_found", detail: "release not found: cccccccccccc" }),
    );
    const outcome = await promoteRelease(client, "cccccccccccc");
    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.status).toBe(404);
    expect(outcome.error.code).toBe("release_not_found");
  });

  it("409 release_not_promotable（未検証）も ActionError にして返す", async () => {
    mock.on("POST", "/api/v1/releases/bbbbbbbbbbbb/promote", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "release_not_promotable",
        detail: "bbbbbbbbbbbb has no verify.json — run scripts/selfdeploy/verify.sh first",
      }),
    );
    const outcome = await promoteRelease(client, "bbbbbbbbbbbb");
    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.status).toBe(409);
    expect(outcome.error.detail).toContain("verify");
  });

  it("401 unauthorized（トークン未設定）も ActionError にして返す", async () => {
    mock.on("POST", "/api/v1/releases/bbbbbbbbbbbb/promote", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "a valid bearer token is required" }),
    );
    const outcome = await promoteRelease(client, "bbbbbbbbbbbb");
    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("unreachable");
    expect(outcome.error.code).toBe("unauthorized");
  });
});
