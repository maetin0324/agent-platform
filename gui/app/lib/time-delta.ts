/**
 * `/daemon` の in_flight 経過時間・`/providers` の cooldown 残り時間の表示用（docs/adr/0007 D3）。
 * celeris の値（`since` / `until` / `DaemonView.now`）の単純な差分表示であり、判断ロジックの再実装ではない。
 */
export function secondsBetween(fromIso: string, toIso: string): number {
  return (new Date(toIso).getTime() - new Date(fromIso).getTime()) / 1000;
}

/** `"3m12s"` / `"45s"` のような概略表示（負値は 0 として扱う）。 */
export function formatDuration(totalSeconds: number): string {
  const s = Math.max(0, Math.round(totalSeconds));
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return m > 0 ? `${m}m${rem}s` : `${rem}s`;
}
