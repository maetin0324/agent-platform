#!/usr/bin/env bash
# Celeris の既定のワーカーイメージ `celeris-worker:latest` を作る（ADR-0043 D3。Phase 56 = A3）。
#
#   scripts/containers/build-worker.sh [タグ]
#
# runtime は `[containers] runtime` と同じ規則で選ぶ（podman が使えれば podman、駄目なら docker）。
# **`cargo test` の一部ではない**（ネットワークに出る。手で叩くか、人が用意する）。
set -euo pipefail

tag="${1:-celeris-worker:latest}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
context="$here/deploy/containers/celeris-worker"

pick_runtime() {
  if [ -n "${CELERIS_CONTAINER_RUNTIME:-}" ]; then
    echo "$CELERIS_CONTAINER_RUNTIME"
    return
  fi
  for candidate in podman docker; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" info >/dev/null 2>&1; then
      echo "$candidate"
      return
    fi
  done
  echo "コンテナ runtime が使えません（podman / docker のどちらも \`info\` が通りません）" >&2
  exit 1
}

runtime="$(pick_runtime)"
echo "runtime: $runtime"
echo "context: $context"
echo "tag:     $tag"

# この LXC では素の build の RUN がネットワークに出られない（PF_NETLINK が塞がれる）。ADR-0043 Phase 56 追記 P56-8 と同じく host で。
network="${CELERIS_BUILD_NETWORK:-host}"
"$runtime" build --network "$network" -t "$tag" -f "$context/Dockerfile" "$context"

echo
echo "できました:"
"$runtime" image inspect "$tag" --format '{{.Id}} {{.Size}} bytes' 2>/dev/null || true
echo
echo "確認:"
echo "  $runtime run --rm $tag sh -lc 'claude --version; codex --version; cargo --version; uv --version; gh --version | head -1'"
