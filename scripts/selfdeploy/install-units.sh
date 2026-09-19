#!/usr/bin/env bash
# scripts/selfdeploy/install-units.sh — deploy/systemd/ のテンプレート unit を
# ~/.config/systemd/user/ に置いて `systemctl --user daemon-reload` する（ADR-0040 D4）。
#
# **人が一度だけ実行する**（ワーカーは実行しない。D5）。これ自体は何も起動しない。
# linger は既に有効（`loginctl enable-linger rmaeda`）である前提。
set -euo pipefail

SD_PROG=install-units
# shellcheck source=lib.sh
SD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SD_DIR/lib.sh"

SRC="$(cd "$SD_DIR/../.." && pwd)/deploy/systemd"
DEST="${SD_UNIT_DIR:-$HOME/.config/systemd/user}"

[ -d "$SRC" ] || sd_die "no such directory: $SRC"
mkdir -p "$DEST"

for unit in taskd@.service taskd-gui@.service; do
  [ -f "$SRC/$unit" ] || sd_die "missing $SRC/$unit"
  if [ -f "$DEST/$unit" ] && ! cmp -s "$SRC/$unit" "$DEST/$unit"; then
    cp -p "$DEST/$unit" "$DEST/$unit.bak-$(sd_stamp)"
    sd_log "kept the old $unit as $DEST/$unit.bak-*"
  fi
  install -m 0644 "$SRC/$unit" "$DEST/$unit"
  sd_log "installed $DEST/$unit"
done

systemctl --user daemon-reload
sd_log "systemctl --user daemon-reload done"
sd_log "next: scripts/selfdeploy/promote.sh <sha12> (a human runs this; see docs/selfdeploy.md)"
