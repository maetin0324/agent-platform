import { describe, expect, it } from "vitest";
import {
  NOTIFY_NOT_CONFIGURED_ERROR,
  notifyKindLabel,
  notifyResultLabel,
  notifyResultTone,
  notifyTargetHref,
} from "~/lib/notify";
import type { NotificationKind, NotifyRecent } from "~/taskd/types";

/**
 * Discord への通知（ADR-0037、Phase 39）の純粋関数（`~/lib/notify.ts`）。DOM を描画する unit テストが無い
 * （G10-U1）ため、`/reports` の Discord 区画が使う判断・計算はここで検証する。
 */

const KINDS: NotificationKind[] = [
  "milestone_ready",
  "approval_pending",
  "question_blocked",
  "bad_news",
  "secretary_reply",
];

describe("notifyKindLabel (ADR-0037 D1、SPEC の言葉で)", () => {
  it("5 種すべてに日本語のラベルがある", () => {
    for (const kind of KINDS) {
      expect(notifyKindLabel(kind)).not.toBe(kind);
      expect(typeof notifyKindLabel(kind)).toBe("string");
    }
  });

  it("知らない kind はそのまま返す（taskd が種を増やしても壊れない）", () => {
    expect(notifyKindLabel("something_new" as NotificationKind)).toBe("something_new");
  });
});

const recent = (over: Partial<NotifyRecent> = {}): NotifyRecent => ({
  kind: "bad_news",
  key: "01J000000000000000000001",
  created_at: "2026-09-18T00:00:00Z",
  attempts: 1,
  ...over,
});

describe("notifyResultLabel (Phase 39 判断 2)", () => {
  it("ok: true は「送れた」", () => {
    expect(notifyResultLabel(recent({ ok: true }))).toBe("送れた");
  });

  it("ok: false かつ NOT_CONFIGURED の文言は「未設定のため送っていません」", () => {
    expect(notifyResultLabel(recent({ ok: false, error: NOTIFY_NOT_CONFIGURED_ERROR }))).toBe(
      "未設定のため送っていません",
    );
  });

  it("ok: false かつ他の理由は「失敗（理由）」", () => {
    expect(notifyResultLabel(recent({ ok: false, error: "http status 404" }))).toBe("失敗（http status 404）");
  });

  it("ok: false で error が無ければ「失敗」だけ", () => {
    expect(notifyResultLabel(recent({ ok: false, error: undefined }))).toBe("失敗");
  });

  it("ok が無い（まだ決着していない）ときは送信待ち", () => {
    expect(notifyResultLabel(recent({ ok: undefined }))).toBe("送信待ち（次の tick で再送）");
    expect(notifyResultLabel(recent({ ok: null }))).toBe("送信待ち（次の tick で再送）");
  });
});

describe("notifyResultTone", () => {
  it("ok: true は success、false は danger、未決は neutral", () => {
    expect(notifyResultTone({ ok: true })).toBe("success");
    expect(notifyResultTone({ ok: false })).toBe("danger");
    expect(notifyResultTone({ ok: undefined })).toBe("neutral");
  });
});

describe("notifyTargetHref (対象へのリンク)", () => {
  it("approval_pending は /approvals のその行へ", () => {
    expect(notifyTargetHref({ kind: "approval_pending", key: "a1" })).toBe("/approvals#approval-a1");
  });

  it("question_blocked は /tasks/{id} へ", () => {
    expect(notifyTargetHref({ kind: "question_blocked", key: "t1" })).toBe("/tasks/t1");
  });

  it("bad_news は /reports のその行へ", () => {
    expect(notifyTargetHref({ kind: "bad_news", key: "r1" })).toBe("/reports#report-r1");
  });

  it("secretary_reply は /projects/{id} へ", () => {
    expect(notifyTargetHref({ kind: "secretary_reply", key: "p1" })).toBe("/projects/p1");
  });

  it("milestone_ready は project_id が無ければ null（古い記録・GET /milestones/{id} が taskd に無いため）", () => {
    expect(notifyTargetHref({ kind: "milestone_ready", key: "m1" })).toBeNull();
  });

  it("milestone_ready は project_id があれば /projects/{project_id} へ（Phase 40 追従、G13i-P1 の解消）", () => {
    expect(notifyTargetHref({ kind: "milestone_ready", key: "m1", project_id: "p1" })).toBe("/projects/p1");
  });

  it("secretary_reply は project_id があればそちらを優先する（Phase 40 追従）", () => {
    expect(notifyTargetHref({ kind: "secretary_reply", key: "p-old", project_id: "p-new" })).toBe("/projects/p-new");
  });

  it("key を URI エンコードする", () => {
    expect(notifyTargetHref({ kind: "secretary_reply", key: "p/1 x" })).toBe("/projects/p%2F1%20x");
  });
});
