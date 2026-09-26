#!/usr/bin/env bash
# ADR-0051: 取り込み済みSHAのrelease/verifyだけ。本番昇格は行わない。
set -euo pipefail
[ "$#" -eq 2 ] || exit 2
sha="$1"
result_dir="$2"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || exit 2
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$result_dir"
ok=false
finish() {
    printf '{"ok":%s}\n' "$ok" > "$result_dir/result.json.tmp"
    mv "$result_dir/result.json.tmp" "$result_dir/result.json"
}
trap finish EXIT
# A clean release rebuild can spend most of an hour in Cargo tests alone.
# Keep the whole release (including packaging and worktree cleanup) bounded,
# but leave enough headroom after the gate succeeds.
timeout --signal=TERM --kill-after=30s 7200 bash "$script_dir/release.sh" "$sha"
timeout --signal=TERM --kill-after=30s 900 bash "$script_dir/verify.sh" "${sha:0:12}"
ok=true
