/**
 * `sha256` 不一致の警告。`ArtifactView.sha256_matches` は taskd が計算済みの値をそのまま見せるだけで、GUI は再計算しない。
 * `matches` が `true` または `null`（未計算・存在しない）のときは何も表示しない（`null` を警告として扱わない）。
 */
export function Sha256Badge({
  recorded,
  current,
  matches,
}: {
  recorded: string;
  current?: string | null;
  matches?: boolean | null;
}) {
  if (matches !== false) return null;
  return (
    <p
      data-testid="sha256-mismatch"
      className="my-2 rounded border border-red-300 bg-red-50 px-3 py-2 text-sm text-red-700"
    >
      sha256 が一致しません（記録: {recorded} / 現在: {current ?? "-"}）
    </p>
  );
}
