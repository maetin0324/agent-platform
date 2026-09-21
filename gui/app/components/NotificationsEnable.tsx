import { Icon } from "~/components/ui/Icon";
import { cn } from "~/lib/utils";

/**
 * 「通知を有効にする」（SPEC §3.5、ADR-0034 D6）。ブラウザの Notification の許可をここで求める
 * （許可が無ければ `NotificationsWatcher` は何もしない）。`celeris` には問い合わせない、純粋にブラウザ API だけの操作。
 *
 * Phase G13f-1（監査 9）でナビゲーションから**報告の画面の中**へ移した（ナビの項目に見えてしまい、
 * 押すと別の画面へ行くように見えていた）。
 */
export function NotificationsEnableButton({ className }: { className?: string }) {
  const handleClick = () => {
    if (typeof Notification === "undefined") return;
    if (Notification.permission === "default") {
      void Notification.requestPermission();
    }
  };
  return (
    <button
      type="button"
      onClick={handleClick}
      data-testid="notifications-enable"
      className={cn(
        // ADR-0055 D1-2/D1-4: タップ領域 44 以上、モバイルは text-sm、デスクトップは lg: で元の text-xs のまま。
        "inline-flex min-h-11 items-center gap-1.5 rounded-lg border border-border px-2.5 py-1.5 text-sm font-medium text-fg-muted transition-colors hover:bg-surface-2 hover:text-fg lg:text-xs",
        className,
      )}
      title="ブラウザ通知を有効にします（悪い知らせは即座に、それ以外は数時間単位）"
    >
      <Icon name="alert" className="size-3.5" />
      通知を有効にする
    </button>
  );
}
