/**
 * 画面から離れて戻ったとき・接続が一時的に切れたときの自動復帰の純粋なロジック（DOM に依存しない）。
 * 背景: スマホでタブをバックグラウンドにするとブラウザが fetch / EventSource を止める。戻った直後の
 * 再検証（SSE 再接続や `task.event` で走る `revalidate()`）が「Failed to fetch」や 5xx（celeris / GUI の再起動中）で
 * 失敗すると、React Router は ErrorBoundary を出し、以後は誰も再検証しないためリロードするまで固まっていた。
 */

/** ErrorBoundary が自動で再検証を試みる間隔（ms）。尽きたら手動の「再試行」ボタンと、復帰イベントに任せる。 */
export const AUTO_RETRY_DELAYS_MS: readonly number[] = [1_500, 4_000, 10_000, 30_000];

/** 復帰イベント（visible / pageshow / online / focus）が連続しても再検証を潰し合わない最小間隔。 */
export const RESUME_MIN_GAP_MS = 1_000;

/** 何度目の自動再試行までの待ち時間。尽きたら null。 */
export function nextRetryDelay(attempt: number): number | null {
  return AUTO_RETRY_DELAYS_MS[attempt] ?? null;
}

/**
 * 復帰イベントをまとめて `run` を最小間隔で 1 回にする。`now` を注入できるのでタイマー無しでテストできる。
 */
export function createResumeGate(run: () => void, minGapMs = RESUME_MIN_GAP_MS, now: () => number = Date.now) {
  let last = Number.NEGATIVE_INFINITY;
  return {
    fire(): boolean {
      const t = now();
      if (t - last < minGapMs) return false;
      last = t;
      run();
      return true;
    },
  };
}

/** 古いビルドのチャンクを取れなかった（デプロイ後に戻った）ときのエラーか。再読み込みで直る。 */
export function isChunkLoadError(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : typeof e === "string" ? e : "";
  return (
    /Failed to fetch dynamically imported module/i.test(msg) ||
    /error loading dynamically imported module/i.test(msg) ||
    /Importing a module script failed/i.test(msg) ||
    /Unable to preload CSS/i.test(msg)
  );
}

/** チャンク取得失敗による再読み込みは、ループを避けるため一定時間内に 1 回だけ。 */
export const CHUNK_RELOAD_KEY = "celeris:chunk-reload-at";
export const CHUNK_RELOAD_WINDOW_MS = 60_000;

export function shouldReloadForChunkError(storage: Pick<Storage, "getItem" | "setItem">, now = Date.now()): boolean {
  try {
    const prev = Number(storage.getItem(CHUNK_RELOAD_KEY) ?? "0");
    if (Number.isFinite(prev) && now - prev < CHUNK_RELOAD_WINDOW_MS) return false;
    storage.setItem(CHUNK_RELOAD_KEY, String(now));
  } catch {
    return false;
  }
  return true;
}
