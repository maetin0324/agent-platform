/**
 * どのルートにも一致しないパスの受け皿（docs/adr/0008 D15）。ここで 404 を投げることで root の middleware
 * （Host 検査 → 認証 → CSRF → nonce 付き CSP 等のヘッダ）が未定義パスにも効き、root の ErrorBoundary が nonce 付きで 404 を描画する。
 * これが無いと React Router は middleware を通さずに素の 404 HTML（CSP 無し、nonce 無しの inline script）を返す。
 */
export function loader() {
  throw new Response("ページが見つかりません。", {
    status: 404,
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}

export default function NotFound() {
  return null;
}
