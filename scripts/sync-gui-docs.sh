#!/usr/bin/env bash
# taskd が持つ API 仕様（docs/gui/api.md）を GUI 側の写し（gui/docs/taskd-api-v1.md）に反映する（ADR-0020 D4）。
#
#   scripts/sync-gui-docs.sh          写しを更新する（変わったら 0、変更なしでも 0）
#   scripts/sync-gui-docs.sh --check  ずれているかだけ見る（ずれていたら exit 1。CI / ランナー用）
#
# 同期するのは **taskd が正である 1 ファイルだけ**。gui/docs/DESIGN.md や gui/docs/adr/* は GUI 側が育てるので触らない。
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$REPO/docs/gui/api.md"
DST="${GUI_REPO:-$REPO/gui}/docs/taskd-api-v1.md"
CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1

[ -f "$SRC" ] || { echo "sync-gui-docs: $SRC が無い" >&2; exit 2; }
[ -d "$(dirname "$DST")" ] || { echo "sync-gui-docs: $(dirname "$DST") が無い（GUI をまだ立ち上げていない）" >&2; exit 0; }

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
# 写し先ではファイル名が taskd-api-v1.md になる（docs/gui/bootstrap/README.md §1 の対応表）。
sed -E 's#\bapi\.md\b#taskd-api-v1.md#g' "$SRC" > "$tmp"

if cmp -s "$tmp" "$DST"; then
  echo "sync-gui-docs: up to date"
  exit 0
fi
if [ "$CHECK" = "1" ]; then
  echo "sync-gui-docs: $DST が docs/gui/api.md とずれている（scripts/sync-gui-docs.sh で更新する）" >&2
  diff -u "$DST" "$tmp" | head -40 >&2
  exit 1
fi
cp "$tmp" "$DST"
echo "sync-gui-docs: updated $DST"
