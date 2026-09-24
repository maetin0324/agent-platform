import { useCallback, useEffect, useRef, useState } from "react";
import { useRevalidator } from "react-router";
import { buttonClass } from "~/components/ui/button";
import { useResumeRevalidate } from "~/hooks/useResumeRevalidate";
import { nextRetryDelay } from "~/lib/recovery";

/**
 * ErrorBoundary の中に置く自動復帰。一時的な失敗（戻った直後の fetch 失敗、celeris / GUI の再起動中）で出た
 * エラー画面を、リロード無しで直す。間隔を空けて loader を再検証し（成功すればこの ErrorBoundary ごと消える）、
 * 戻る操作（visible / online / pageshow）でも即再試行する。尽きたら手動の「再試行」ボタンが残る。
 */
export function RouteRecovery() {
  const revalidator = useRevalidator();
  const [attempt, setAttempt] = useState(0);
  const revalidate = revalidator.revalidate;
  const busy = revalidator.state !== "idle";
  const busyRef = useRef(busy);
  busyRef.current = busy;

  const retry = useCallback(() => {
    if (!busyRef.current) revalidate();
  }, [revalidate]);

  useEffect(() => {
    const delay = nextRetryDelay(attempt);
    if (delay === null) return;
    const id = setTimeout(() => {
      retry();
      setAttempt((n) => n + 1);
    }, delay);
    return () => clearTimeout(id);
  }, [attempt, retry]);

  useResumeRevalidate(retry);

  const exhausted = nextRetryDelay(attempt) === null;
  return (
    <div className="mt-4 flex flex-wrap items-center gap-3" data-testid="route-recovery">
      <button
        type="button"
        className={buttonClass({ variant: "primary" })}
        disabled={busy}
        onClick={() => {
          setAttempt(0);
          retry();
        }}
      >
        {busy ? "再読み込み中…" : "再試行"}
      </button>
      <span className="text-xs text-fg-muted" role="status">
        {exhausted ? "自動で復帰できませんでした。再試行してください。" : "自動で再試行しています…"}
      </span>
    </div>
  );
}
