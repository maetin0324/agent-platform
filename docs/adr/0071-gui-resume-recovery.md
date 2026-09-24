# ADR-0071: GUI の離脱→復帰でエラー画面に固まらない

## 状況
スマホ等でタブを離れて戻ると「エラー」画面（「予期しないエラーが起きました。」）になり、リロードするまで戻らなかった。

## 原因
- バックグラウンド中はブラウザが fetch / EventSource を止める。戻った直後、SSE 由来の `revalidate()`（`useCelerisStream`）や画面遷移の
  loader 取得が「Failed to fetch」/ 5xx（GUI・celeris 再起動中）で失敗すると、React Router は ErrorBoundary を出す。
- どの ErrorBoundary にも再試行手段が無く、復帰イベント（visibilitychange / pageshow / online）を誰も聞いていなかった。
  SSE は CLOSED になると自動再接続されない。
- デプロイ後に戻った旧タブは旧ビルドのチャンクが 404 になり動的 import が失敗する。

## 決定
- D1: `RouteRecovery`（全 ErrorBoundary の汎用フォールバックに配置）。1.5/4/10/30 秒で loader を再検証し、成功すればエラー画面が消える。尽きたら「再試行」ボタン。
- D2: `useResumeRevalidate`: visible / pageshow / online / focus で再検証（1 秒で束ねる）。`useCelerisStream` はこれで EventSource も張り直し、CLOSED なら 5 秒後に再接続。
- D3: `vite:preloadError` で 60 秒に 1 回だけ再読み込み（ループ防止）。
- D4: 純粋ロジックは `app/lib/recovery.ts` に置き単体テストする。
