import { useEffect } from "react";
import { createResumeGate } from "~/lib/recovery";

/**
 * タブ / アプリへ戻ったとき（visibilitychange→visible、pageshow（bfcache 復元）、online、focus）に `onResume` を呼ぶ。
 * `onResume` には再検証と SSE 再接続を渡す。最小間隔で束ねるので、複数のイベントが同時に来ても 1 回。
 */
export function useResumeRevalidate(onResume: () => void, enabled = true): void {
  useEffect(() => {
    if (!enabled) return;
    const gate = createResumeGate(onResume);
    const onVisibility = () => {
      if (document.visibilityState === "visible") gate.fire();
    };
    const onPageShow = () => {
      gate.fire();
    };
    const onOnline = () => {
      gate.fire();
    };
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("pageshow", onPageShow);
    window.addEventListener("online", onOnline);
    window.addEventListener("focus", onOnline);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("pageshow", onPageShow);
      window.removeEventListener("online", onOnline);
      window.removeEventListener("focus", onOnline);
    };
  }, [onResume, enabled]);
}
