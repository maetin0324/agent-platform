import { StrictMode, startTransition } from "react";
import { hydrateRoot } from "react-dom/client";
import { HydratedRouter } from "react-router/dom";
import { shouldReloadForChunkError } from "~/lib/recovery";

// デプロイ後に古いタブへ戻ると、旧ビルドのチャンクが無く動的 import が失敗する。1 回だけ再読み込みして新ビルドを取り直す
// （60 秒以内に繰り返すときはループを避けて ErrorBoundary の「再試行」に任せる）。
window.addEventListener("vite:preloadError", (event) => {
  if (shouldReloadForChunkError(window.sessionStorage)) {
    event.preventDefault();
    window.location.reload();
  }
});

startTransition(() => {
  hydrateRoot(
    document,
    <StrictMode>
      <HydratedRouter />
    </StrictMode>,
  );
});
