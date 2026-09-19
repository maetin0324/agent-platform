import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { instanceRoleLabel } from "~/lib/labels";
import {
  handoffInFlight,
  handoffProgressText,
  promoteAvailability,
  promoteConfirmText,
  releaseGateLabel,
  releasePositionLabel,
  releaseSubtitle,
  releaseVerifyLabel,
  releaseVerifyState,
  releaseVerifyTone,
  runningSummary,
} from "~/lib/releases";
import { loadReleases } from "~/routes/releases";
import { TaskdClient } from "~/taskd/client.server";
import { promoteRelease } from "~/taskd/releases-admin.server";
import type { ReleaseItem, Releases } from "~/taskd/types";
import { defaultReleasePromoteAccepted, defaultReleases, releaseItem } from "../mock-taskd/fixtures";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * 「リリース」画面（`/releases`、Phase G14。ADR-0040 D6、docs/taskd-api-v1.md §3.66〜3.67）。
 *
 * - `~/lib/releases.ts` の純粋関数（検証状態の出し分け、昇格できるかどうか、引き継ぎの進行の一行）
 * - `loadReleases`（`GET /releases` をそのまま返す。並びは taskd の順を崩さない）
 * - `promoteRelease`（202 / 404 / 409 / 401 を `{ok:false, error}` として返す）
 *
 * 外部ネットワークには出ない（`test/mock-taskd/server.ts` の loopback サーバだけ）。
 */

/** `test/mock-taskd/fixtures.ts` の既定を上書きする短縮形。 */
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

  it("1 行の説明は built_at · ref · schema", () => {
    expect(releaseSubtitle(item())).toBe("2026-09-19T00:00:00Z · main · schema 11");
    expect(releaseSubtitle(item({ built_at: null, ref: null, schema_version: null }))).toBe("ビルド日時が読めません");
  });
});

describe("promoteAvailability（taskd の 409 と同じ理由で先回りして止める）", () => {
  it("検証済みで current でも昇格中でもなければ押せる", () => {
    expect(promoteAvailability(item())).toEqual({ canPromote: true, reason: null });
  });

  it("current は押せない", () => {
    const r = promoteAvailability(item({ is_current: true }));
    expect(r.canPromote).toBe(false);
    expect(r.reason).toContain("いま動いている");
  });

  it("昇格中は押せない", () => {
    const r = promoteAvailability(item({ promoting: true }));
    expect(r.canPromote).toBe(false);
    expect(r.reason).toContain("昇格が走っています");
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
          instance_id: "01MOCKTASKDINSTANCE00002",
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
    expect(handoffProgressText(promoting)).toContain("昇格中");
  });

  it("running の一行は役割を日本語で出す", () => {
    expect(runningSummary(releasesView.running)).toBe("aaaaaaaaaaaa（稼働中）");
  });
});

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
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

  it("taskd に繋がらなければ loader が Response に変換できるよう reject する", async () => {
    const closed = await startMockTaskd();
    const baseUrl = closed.baseUrl;
    await closed.close();
    const unreachable = new TaskdClient({ baseUrl, timeoutMs: 1000 });
    await expect(loadReleases(unreachable, new Request("http://gui.invalid/releases"))).rejects.toBeTruthy();
  });
});

describe("promoteRelease（POST /releases/{sha12}/promote。管理系）", () => {
  it("202 をそのまま返す（本文は空の JSON を送る）", async () => {
    const accepted = defaultReleasePromoteAccepted;
    mock.on("POST", "/api/v1/releases/bbbbbbbbbbbb/promote", (_req, res) => sendJson(res, 202, accepted));
    const outcome = await promoteRelease(client, "bbbbbbbbbbbb");
    expect(outcome).toEqual({ ok: true, op: "release_promote", sha12: "bbbbbbbbbbbb", accepted });
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
