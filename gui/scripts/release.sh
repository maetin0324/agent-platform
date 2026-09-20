#!/usr/bin/env bash
# pnpm release — pnpm build してから dist/celeris-gui-<version>.tar.gz を作る（docs/DESIGN.md §9、docs/adr/0008 D9）。
# 同梱: build/ server.js package.json pnpm-lock.yaml pnpm-workspace.yaml README.md deploy/celeris-gui.service Dockerfile
# tar のトップディレクトリは celeris-gui-<version>/。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(node -p "require('./package.json').version")"
[ -n "$VERSION" ] || { echo "release.sh: could not read version from package.json" >&2; exit 1; }

echo "release.sh: building celeris-gui $VERSION"
pnpm build

DIST_DIR="$ROOT/dist"
STAGE_NAME="celeris-gui-$VERSION"
STAGE_ROOT="$(mktemp -d)"
trap 'rm -rf "$STAGE_ROOT"' EXIT
STAGE_DIR="$STAGE_ROOT/$STAGE_NAME"
mkdir -p "$STAGE_DIR"

for item in build server.js package.json pnpm-lock.yaml pnpm-workspace.yaml README.md deploy/celeris-gui.service Dockerfile; do
  src="$ROOT/$item"
  [ -e "$src" ] || { echo "release.sh: missing required item: $item" >&2; exit 1; }
  dest="$STAGE_DIR/$item"
  mkdir -p "$(dirname "$dest")"
  cp -r "$src" "$dest"
done

mkdir -p "$DIST_DIR"
TARBALL="$DIST_DIR/$STAGE_NAME.tar.gz"
rm -f "$TARBALL"
tar -czf "$TARBALL" -C "$STAGE_ROOT" "$STAGE_NAME"

echo "release.sh: wrote $TARBALL"
echo "release.sh: contents (first lines):"
tar tzf "$TARBALL" | head -20 || true
echo "release.sh: size: $(du -h "$TARBALL" | cut -f1)"
