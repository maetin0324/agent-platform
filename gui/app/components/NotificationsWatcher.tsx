import { useEffect, useRef } from "react";
import { useFetcher } from "react-router";
import { notificationMessage, shouldFireNotification } from "~/lib/reports";
import type { ReportsLive } from "~/taskd/types";

/**
 * ブラウザ通知（SPEC §3.5「通知は GUI から飛んでくる。数分単位ではなく、数時間単位」、ADR-0034 D6）。
 * `notify_now` は taskd が決定的に決める（GUI はそれに従うだけ）。判断（`shouldFireNotification`、
 * 重複排除の鍵）は `~/lib/reports.ts` の純粋関数に切り出してあり、ここは Notification API と
 * `POST /reports/notified`（`~/routes/reports.tsx` の action）への配線だけ。root で 1 回だけ
 * マウントする（`useTaskdStream` と同じ形。SSE の `daemon` イベント → root 再検証 → この props が更新される）。
 */
export function NotificationsWatcher({ reportsLive }: { reportsLive: ReportsLive | null }) {
  const fetcher = useFetcher();
  const lastFiredKey = useRef<string | null>(null);

  useEffect(() => {
    if (typeof window === "undefined" || typeof Notification === "undefined") return;
    const { fire, key } = shouldFireNotification(reportsLive, Notification.permission, lastFiredKey.current);
    lastFiredKey.current = key;
    if (!fire || !reportsLive) return;
    new Notification("taskd: 報告", { body: notificationMessage(reportsLive) });
    fetcher.submit({ intent: "reports_notified" }, { method: "post", action: "/reports" });
    // `fetcher` 全体ではなく `.submit` だけを依存にする（`useTaskdStream` の `revalidator.revalidate` と同じ理由。
    // `useFetcher()` はレンダーのたびに新しい参照を返しうる）。
  }, [reportsLive, fetcher.submit]);

  return null;
}
