#!/usr/bin/env bash
# scripts/selfdeploy/install-units.sh [--remove-old] — deploy/systemd/ のテンプレート unit を
# ~/.config/systemd/user/ に置いて `systemctl --user daemon-reload` する（ADR-0040 D4、ADR-0045 D3）。
#
# **人が一度だけ実行する**（ワーカーは実行しない。D5）。これ自体は何も起動しない。
# linger は既に有効（`loginctl enable-linger rmaeda`）である前提。
#
#   --remove-old  改名前のテンプレート unit も消す。**移行のときだけ**使う。消す対象の名前は
#                 環境変数 `SD_OLD_UNITS`（空白区切り）で渡す — 旧い名前を知っているのは
#                 `migrate-to-celeris.sh` だけ、という置き方（ADR-0045 D3）。
#                 その名前の instance が動いていたら、消さずに止まる。
set -euo pipefail

SD_PROG=install-units
# shellcheck source=lib.sh
SD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SD_DIR/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: install-units.sh [--remove-old]

  --remove-old  改名前のテンプレート unit も ~/.config/systemd/user/ から消す（移行のときだけ）。
EOF
  exit 2
}

REMOVE_OLD=false
while [ $# -gt 0 ]; do
  case "$1" in
    --remove-old) REMOVE_OLD=true; shift ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
done

SRC="$(cd "$SD_DIR/../.." && pwd)/deploy/systemd"
DEST="${SD_UNIT_DIR:-$HOME/.config/systemd/user}"

[ -d "$SRC" ] || sd_die "no such directory: $SRC"
mkdir -p "$DEST"

for unit in celeris@.service celeris-gui@.service; do
  [ -f "$SRC/$unit" ] || sd_die "missing $SRC/$unit"
  if [ -f "$DEST/$unit" ] && ! cmp -s "$SRC/$unit" "$DEST/$unit"; then
    cp -p "$DEST/$unit" "$DEST/$unit.bak-$(sd_stamp)"
    sd_log "kept the old $unit as $DEST/$unit.bak-*"
  fi
  install -m 0644 "$SRC/$unit" "$DEST/$unit"
  sd_log "installed $DEST/$unit"
done

if [ "$REMOVE_OLD" = true ]; then
  if [ -z "${SD_OLD_UNITS:-}" ]; then
    sd_log "--remove-old: SD_OLD_UNITS is empty; nothing to remove (migrate-to-celeris.sh sets it)"
  fi
  for unit in ${SD_OLD_UNITS:-}; do
    [ -f "$DEST/$unit" ] || continue
    # 動いているものは消さない（テンプレートを消してから止められなくなる）。
    if systemctl --user list-units --all --plain --no-legend "${unit%@.service}@*.service" 2>/dev/null | grep -q .; then
      sd_die "$unit still has instances; stop them before removing the template"
    fi
    rm -f "$DEST/$unit"
    sd_log "removed the pre-rename template $DEST/$unit"
  done
fi

systemctl --user daemon-reload
sd_log "systemctl --user daemon-reload done"
sd_log "next: scripts/selfdeploy/promote.sh <sha12> (a human runs this; see docs/selfdeploy.md)"
