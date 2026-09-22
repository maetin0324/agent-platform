import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { sendNotifyTest } from "~/celeris/notify-admin.server";
import { markReportsNotified, markReportsRead } from "~/celeris/reports-admin.server";
import type {
  NotifyRecent,
  NotifyView,
  OrgList,
  OrgNode,
  Project,
  ProjectList,
  Report,
  ReportDetail,
  ReportList,
} from "~/celeris/types";
import {
  absoluteDateLabel,
  buildReportsQuery,
  dateTimeLabel,
  filterReportsByKind,
  notificationMessage,
  relativeTimeLabel,
  reportNodeName,
  reportProjectName,
  reportsBadgeTone,
  reportsNotificationKey,
  shouldFireNotification,
} from "~/lib/reports";
import { loadReports } from "~/routes/reports";
import { loadReportDetail } from "~/routes/reports.$id";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

const report = (id: string, over: Partial<Report> = {}): Report => ({
  id,
  kind: "result",
  level: 0,
  node_id: "coding-poc",
  headline: id,
  created_at: "2026-09-17T00:00:00Z",
  ...over,
});

const project = (id: string, over: Partial<Project> = {}): Project => ({
  id,
  title: id,
  request: "…",
  status: "proposed",
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

const orgNode = (id: string, over: Partial<OrgNode> = {}): OrgNode => ({
  id,
  parent_id: null,
  name: id,
  kind: "section",
  position: 0,
  created_at: "2026-09-17T00:00:00Z",
  updated_at: "2026-09-17T00:00:00Z",
  ...over,
});

describe("buildReportsQuery (docs/gui/api.md §3.50)", () => {
  it("既定（クエリなし）は秘書レベルの未読 — level=0 & unread=true", () => {
    expect(buildReportsQuery(new URLSearchParams())).toEqual({ unread: true, level: 0 });
  });

  it("filter=all は unread を送らない", () => {
    expect(buildReportsQuery(new URLSearchParams("filter=all"))).toEqual({ level: 0 });
  });

  it("level= （空文字）はすべてのレベル — level キーを送らない", () => {
    expect(buildReportsQuery(new URLSearchParams("level="))).toEqual({ unread: true });
  });

  it("level=2, project, limit をそのまま転送する", () => {
    expect(buildReportsQuery(new URLSearchParams("level=2&project=p1&limit=10"))).toEqual({
      unread: true,
      level: 2,
      project: "p1",
      limit: "10",
    });
  });

  it("kind は送らない（celeris の GET /reports は kind を受け付けず、知らないクエリキーは 400 になるため）", () => {
    const q = buildReportsQuery(new URLSearchParams("kind=bad_news&kind=result"));
    expect(q).not.toHaveProperty("kind");
  });
});

describe("filterReportsByKind", () => {
  const items = [
    report("r1", { kind: "bad_news" }),
    report("r2", { kind: "result" }),
    report("r3", { kind: "proposal" }),
  ];

  it("空配列は絞り込まない", () => {
    expect(filterReportsByKind(items, [])).toEqual(items);
  });

  it("指定した kind だけ残す", () => {
    expect(filterReportsByKind(items, ["bad_news", "proposal"]).map((r) => r.id)).toEqual(["r1", "r3"]);
  });
});

describe("reportProjectName / reportNodeName", () => {
  const projects = [project("p1", { title: "Pluvio の新テーマ" })];
  const org = [orgNode("coding-poc", { name: "PoC・R&D 課" })];

  it("project_id が無ければ「案件なし」", () => {
    expect(reportProjectName({ project_id: null }, projects)).toBe("案件なし");
    expect(reportProjectName({ project_id: undefined }, projects)).toBe("案件なし");
  });

  it("project_id があれば title を解決する", () => {
    expect(reportProjectName({ project_id: "p1" }, projects)).toBe("Pluvio の新テーマ");
  });

  it("見つからない project_id は id をそのまま出す", () => {
    expect(reportProjectName({ project_id: "missing" }, projects)).toBe("missing");
  });

  it("node_id から組織ノードの名前を解決する。見つからなければ id をそのまま", () => {
    expect(reportNodeName({ node_id: "coding-poc" }, org)).toBe("PoC・R&D 課");
    expect(reportNodeName({ node_id: "missing" }, org)).toBe("missing");
  });
});

// ADR-0055 D2 ラウンド 7（Phase 75、P-G30-1）: 1 単位に丸めた日本語の相対表示（10 秒未満は「たった今」、
// 7 日を超えたら絶対日付）。
describe("relativeTimeLabel", () => {
  it.each<[string, string, string]>([
    ["2026-09-17T00:00:00Z", "2026-09-17T00:00:05Z", "たった今"], // 10 秒未満
    ["2026-09-17T00:00:00Z", "2026-09-17T00:00:15Z", "15秒前"],
    ["2026-09-17T00:00:00Z", "2026-09-17T00:03:12Z", "3分前"], // 1 単位に丸める（秒は出さない）
    ["2026-09-17T00:00:00Z", "2026-09-17T01:12:00Z", "1時間前"],
    ["2026-09-17T00:00:00Z", "2026-09-18T01:00:00Z", "1日前"],
    ["2026-09-17T00:00:00Z", "2026-09-23T01:00:00Z", "6日前"], // 7 日未満はまだ相対表示
    ["2026-09-10T00:00:00Z", "2026-09-17T00:00:00Z", "9/10"], // ちょうど 7 日で絶対日付（同じ年は M/D）
    ["2025-12-31T00:00:00Z", "2026-09-17T00:00:00Z", "2025/12/31"], // 年をまたぐと YYYY/M/D
  ])("relativeTimeLabel(%s, %s) === %s", (iso, fetchedAtIso, expected) => {
    expect(relativeTimeLabel(iso, fetchedAtIso)).toBe(expected);
  });
});

// Phase 90（U-G37-1 の解消）: Phase 84 では絶対日付フォールバックに `Date` のローカル getter（実行環境
// 依存。`process.env.TZ` で切り替えていた）を使っていたが、SSR（GUI サーバー、通常 UTC）と CSR（ブラウザ、
// 視聴者のタイムゾーン）が食い違う実配置ではハイドレーション後に表示が変わってしまっていた
// （`docs/PROGRESS.md` Phase 84 の未解決事項 U-G37-1）。`Date` のローカル/UTC getter をやめ、
// `Intl.DateTimeFormat` + **明示的な `timeZone` 引数**にしたので、`process.env.TZ` を切り替えるのではなく
// 同じ実行環境のまま `timeZone` 引数を変えて、同じ ISO でも結果が変わることを直接確認する
// （既定値のままなら両ケースとも同じ値になってしまうので、これが違うこと自体が「明示的な引数で決まる」
// ことの証拠になる。`~/components/LocalTime.tsx` がサーバでは常に `"UTC"`、ハイドレーション後は
// 視聴者の解決済みタイムゾーンを渡す）。
describe("absoluteDateLabel / relativeTimeLabel の絶対日付フォールバック — 明示的な timeZone（Phase 90, U-G37-1）", () => {
  it("既定（引数省略）は UTC", () => {
    expect(absoluteDateLabel("2025-12-31T23:00:00Z", "2026-09-17T00:00:00Z")).toBe("2025/12/31");
    expect(relativeTimeLabel("2025-12-31T23:00:00Z", "2026-09-17T00:00:00Z")).toBe("2025/12/31");
  });

  it("UTC+14（Pacific/Kiritimati）を明示すると日付が 1 日進み、年またぎが解消されて M/D になる", () => {
    expect(absoluteDateLabel("2025-12-31T23:00:00Z", "2026-09-17T00:00:00Z", "Pacific/Kiritimati")).toBe("1/1");
    expect(relativeTimeLabel("2025-12-31T23:00:00Z", "2026-09-17T00:00:00Z", "Pacific/Kiritimati")).toBe("1/1");
  });

  it("America/Chicago（DST 中は UTC-5）を明示すると同じ瞬間でも日付が変わりうる", () => {
    expect(absoluteDateLabel("2026-09-17T02:00:00Z", "2026-09-17T12:00:00Z", "UTC")).toBe("9/17");
    expect(absoluteDateLabel("2026-09-17T02:00:00Z", "2026-09-17T12:00:00Z", "America/Chicago")).toBe("9/16");
  });
});

describe('dateTimeLabel（`~/components/LocalTime.tsx` の mode="datetime" 用。Phase 90）', () => {
  it("既定は UTC の年月日時分", () => {
    expect(dateTimeLabel("2026-09-22T05:06:00Z")).toBe("2026/9/22 05:06");
  });

  it("Asia/Tokyo（UTC+9）を明示すると時刻がずれる", () => {
    expect(dateTimeLabel("2026-09-22T05:06:00Z", "Asia/Tokyo")).toBe("2026/9/22 14:06");
  });
});

describe("reportsBadgeTone", () => {
  it("null は neutral", () => {
    expect(reportsBadgeTone(null)).toBe("neutral");
  });
  it("bad_news が 0 件なら neutral", () => {
    expect(reportsBadgeTone({ unread_secretary: 3, unread_bad_news: 0, notify_now: false })).toBe("neutral");
  });
  it("bad_news があれば danger", () => {
    expect(reportsBadgeTone({ unread_secretary: 3, unread_bad_news: 1, notify_now: true })).toBe("danger");
  });
});

describe("notificationMessage / reportsNotificationKey", () => {
  it("bad_news が 0 件なら未読の件数だけ", () => {
    expect(notificationMessage({ unread_secretary: 2, unread_bad_news: 0 })).toBe("未読の報告 2 件");
  });
  it("bad_news があれば先頭に出す", () => {
    expect(notificationMessage({ unread_secretary: 3, unread_bad_news: 1 })).toBe("悪い知らせ 1 件 / 未読の報告 3 件");
  });
  it("件数の組が鍵になる", () => {
    expect(reportsNotificationKey({ unread_secretary: 2, unread_bad_news: 1 })).toBe("2:1");
  });
});

describe("shouldFireNotification (ADR-0034 D6)", () => {
  const live = { unread_secretary: 2, unread_bad_news: 1, notify_now: true };

  it("reportsLive が無ければ鳴らさない", () => {
    expect(shouldFireNotification(null, "granted", null)).toEqual({ fire: false, key: null });
  });

  it("許可が granted でなければ鳴らさない", () => {
    expect(shouldFireNotification(live, "default", null)).toEqual({ fire: false, key: null });
    expect(shouldFireNotification(live, "denied", null)).toEqual({ fire: false, key: null });
  });

  it("notify_now が false なら鳴らさない", () => {
    expect(shouldFireNotification({ ...live, notify_now: false }, "granted", null)).toEqual({
      fire: false,
      key: null,
    });
  });

  it("初回（lastFiredKey が null）は鳴らし、鍵を返す", () => {
    expect(shouldFireNotification(live, "granted", null)).toEqual({ fire: true, key: "2:1" });
  });

  it("同じ件数の組のままなら再度は鳴らさない（GUI 側の重複排除）", () => {
    expect(shouldFireNotification(live, "granted", "2:1")).toEqual({ fire: false, key: "2:1" });
  });

  it("件数が変わったら（新しい悪い知らせ等）再び鳴らす", () => {
    expect(shouldFireNotification({ ...live, unread_bad_news: 2 }, "granted", "2:1")).toEqual({
      fire: true,
      key: "2:2",
    });
  });
});

describe("loadReports (app/routes/reports.tsx)", () => {
  it("既定のクエリ（level=0&unread=true）で GET /reports を呼び、案件・組織を束ねる", async () => {
    mock.on("GET", "/api/v1/reports", (_req, res) =>
      sendJson(res, 200, { items: [report("r1")] } satisfies ReportList),
    );
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendJson(res, 200, { items: [project("p1")] } satisfies ProjectList),
    );
    mock.on("GET", "/api/v1/org", (_req, res) =>
      sendJson(res, 200, { items: [orgNode("coding-poc")] } satisfies OrgList),
    );

    const result = await loadReports(client, new Request("http://gui.invalid/reports"));

    expect(result.reports.items.map((r) => r.id)).toEqual(["r1"]);
    expect(result.projects.map((p) => p.id)).toEqual(["p1"]);
    expect(result.org.map((n) => n.id)).toEqual(["coding-poc"]);
    expect(typeof result.fetchedAt).toBe("string");

    const req = mock.requests.find((r) => r.url.startsWith("/api/v1/reports"));
    expect(req).toBeDefined();
    const url = new URL(req?.url ?? "", "http://mock-celeris.invalid");
    expect(url.searchParams.get("unread")).toBe("true");
    expect(url.searchParams.get("level")).toBe("0");
    expect(url.searchParams.has("kind")).toBe(false);
  });

  it("filter=all&level= のときは unread も level も送らない", async () => {
    mock.on("GET", "/api/v1/reports", (_req, res) => sendJson(res, 200, { items: [] } satisfies ReportList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));

    await loadReports(client, new Request("http://gui.invalid/reports?filter=all&level="));

    const req = mock.requests.find((r) => r.url.startsWith("/api/v1/reports"));
    const url = new URL(req?.url ?? "", "http://mock-celeris.invalid");
    expect(url.searchParams.has("unread")).toBe(false);
    expect(url.searchParams.has("level")).toBe(false);
  });

  it("GET /projects や GET /org が失敗しても報告は返す（空配列に落とす）", async () => {
    mock.on("GET", "/api/v1/reports", (_req, res) =>
      sendJson(res, 200, { items: [report("r1")] } satisfies ReportList),
    );
    mock.on("GET", "/api/v1/projects", (_req, res) =>
      sendProblem(res, { status: 500, code: "internal", detail: "boom" }),
    );
    mock.on("GET", "/api/v1/org", (_req, res) => sendProblem(res, { status: 500, code: "internal", detail: "boom" }));

    const result = await loadReports(client, new Request("http://gui.invalid/reports"));
    expect(result.reports.items).toHaveLength(1);
    expect(result.projects).toEqual([]);
    expect(result.org).toEqual([]);
  });
});

const notifyRecent = (over: Partial<NotifyRecent> = {}): NotifyRecent => ({
  kind: "bad_news",
  key: "r1",
  created_at: "2026-09-18T00:00:00Z",
  attempts: 1,
  ok: true,
  ...over,
});

const notifyView = (over: Partial<NotifyView> = {}): NotifyView => ({
  configured: true,
  secret_id: "discord-webhook",
  fingerprint: "3f9a1c02",
  recent: [notifyRecent()],
  ...over,
});

describe("loadReports — GET /notify (ADR-0037 D4, docs/gui/api.md §3.64)", () => {
  it("GET /notify を束ねて ReportsData.notify に入れる", async () => {
    mock.on("GET", "/api/v1/reports", (_req, res) => sendJson(res, 200, { items: [] } satisfies ReportList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/notify", (_req, res) => sendJson(res, 200, notifyView()));

    const result = await loadReports(client, new Request("http://gui.invalid/reports"));
    expect(result.notify).toEqual(notifyView());
    expect(result.notifyError).toBeNull();
  });

  it("未設定のときは configured: false・fingerprint 無しをそのまま渡す", async () => {
    mock.on("GET", "/api/v1/reports", (_req, res) => sendJson(res, 200, { items: [] } satisfies ReportList));
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/notify", (_req, res) =>
      sendJson(res, 200, {
        configured: false,
        secret_id: "discord-webhook",
        recent: [notifyRecent({ ok: false, error: "discord webhook is not configured" })],
      } satisfies NotifyView),
    );

    const result = await loadReports(client, new Request("http://gui.invalid/reports"));
    expect(result.notify?.configured).toBe(false);
    expect(result.notify?.fingerprint).toBeUndefined();
    expect(result.notify?.recent[0]?.error).toBe("discord webhook is not configured");
  });

  it("GET /notify が失敗しても報告は返す（notify は null、notifyError に理由が入る）", async () => {
    mock.on("GET", "/api/v1/reports", (_req, res) =>
      sendJson(res, 200, { items: [report("r1")] } satisfies ReportList),
    );
    mock.on("GET", "/api/v1/projects", (_req, res) => sendJson(res, 200, { items: [] } satisfies ProjectList));
    mock.on("GET", "/api/v1/org", (_req, res) => sendJson(res, 200, { items: [] } satisfies OrgList));
    mock.on("GET", "/api/v1/notify", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );

    const result = await loadReports(client, new Request("http://gui.invalid/reports"));
    expect(result.reports.items).toHaveLength(1);
    expect(result.notify).toBeNull();
    expect(result.notifyError?.status).toBe(401);
    expect(result.notifyError?.code).toBe("unauthorized");
  });
});

describe("sendNotifyTest (ADR-0037 D4, docs/gui/api.md §3.65. 管理系)", () => {
  it("POST /notify/test — success（result.ok: true）", async () => {
    mock.on("POST", "/api/v1/notify/test", (_req, res) =>
      sendJson(res, 200, { ok: true, detail: "the test message was delivered" }),
    );
    const result = await sendNotifyTest(client);
    expect(result).toEqual({
      ok: true,
      op: "notify_test",
      result: { ok: true, detail: "the test message was delivered" },
    });
  });

  it("POST /notify/test — success だが送れなかった（result.ok: false）", async () => {
    mock.on("POST", "/api/v1/notify/test", (_req, res) => sendJson(res, 200, { ok: false, detail: "http status 404" }));
    const result = await sendNotifyTest(client);
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.result).toEqual({ ok: false, detail: "http status 404" });
  });

  it("POST /notify/test — 401 unauthorized（管理系。token_file 未設定でも拒否される）", async () => {
    mock.on("POST", "/api/v1/notify/test", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const result = await sendNotifyTest(client);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.op).toBe("notify_test");
      expect(result.error.status).toBe(401);
      expect(result.error.code).toBe("unauthorized");
    }
  });

  it("POST /notify/test — 409 notify_unavailable（秘密が未登録）", async () => {
    mock.on("POST", "/api/v1/notify/test", (_req, res) =>
      sendProblem(res, { status: 409, code: "notify_unavailable", detail: "discord-webhook" }),
    );
    const result = await sendNotifyTest(client);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.status).toBe(409);
      expect(result.error.code).toBe("notify_unavailable");
      expect(result.error.conflict).toBe(true);
    }
  });
});

describe("loadReportDetail (app/routes/reports.$id.tsx, resource route)", () => {
  it("GET /reports/{id} をそのまま返す（sources_expanded を含む）", async () => {
    const detail: ReportDetail = { report: report("r2"), sources_expanded: [report("r1")] };
    mock.on("GET", "/api/v1/reports/r2", (_req, res) => sendJson(res, 200, detail));

    const result = await loadReportDetail(client, "r2", new Request("http://gui.invalid/reports/r2"));
    expect(result).toEqual(detail);
  });
});

describe("markReportsRead / markReportsNotified (ADR-0033 D3, docs/gui/api.md §3.52〜3.53)", () => {
  it("POST /reports/read — success", async () => {
    mock.on("POST", "/api/v1/reports/read", (_req, res) => sendJson(res, 200, { updated: 2 }));
    const result = await markReportsRead(client, ["r1", "r2"]);
    expect(result).toEqual({ ok: true, op: "reports_read", ids: ["r1", "r2"], result: { updated: 2 } });
    const req = mock.requests.find((r) => r.url === "/api/v1/reports/read");
    expect(JSON.parse(req?.body ?? "{}")).toEqual({ ids: ["r1", "r2"] });
  });

  it("POST /reports/read — 401 unauthorized（管理系。token_file 未設定でも拒否される）", async () => {
    mock.on("POST", "/api/v1/reports/read", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const result = await markReportsRead(client, ["r1"]);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.op).toBe("reports_read");
      expect(result.error.status).toBe(401);
      expect(result.error.code).toBe("unauthorized");
    }
  });

  it("POST /reports/read — 404 report_not_found の文言をそのまま返す（ULID でない id）", async () => {
    mock.on("POST", "/api/v1/reports/read", (_req, res) =>
      sendProblem(res, { status: 404, code: "report_not_found", detail: "not a ulid" }),
    );
    const result = await markReportsRead(client, ["not-a-ulid"]);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.code).toBe("report_not_found");
  });

  it("POST /reports/notified — success", async () => {
    mock.on("POST", "/api/v1/reports/notified", (_req, res) =>
      sendJson(res, 200, { last_notified_at: "2026-09-17T01:00:00Z" }),
    );
    const result = await markReportsNotified(client);
    expect(result).toEqual({
      ok: true,
      op: "reports_notified",
      result: { last_notified_at: "2026-09-17T01:00:00Z" },
    });
  });

  it("POST /reports/notified — 401 unauthorized", async () => {
    mock.on("POST", "/api/v1/reports/notified", (_req, res) =>
      sendProblem(res, { status: 401, code: "unauthorized", detail: "token required" }),
    );
    const result = await markReportsNotified(client);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.status).toBe(401);
  });
});
