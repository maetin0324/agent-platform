#!/usr/bin/env bash
# scripts/selfdeploy/install-units.sh [--remove-old] [--remove-qwen-tunnel] — deploy/systemd/ の
# テンプレート unit を ~/.config/systemd/user/ に置いて `systemctl --user daemon-reload` する
# （ADR-0040 D4、ADR-0045 D3）。
#
# **人が一度だけ実行する**（ワーカーは実行しない。D5）。これ自体は何も起動しない。
# linger は既に有効（`loginctl enable-linger rmaeda`）である前提。
#
#   --remove-old  改名前のテンプレート unit も消す。**移行のときだけ**使う。消す対象の名前は
#                 環境変数 `SD_OLD_UNITS`（空白区切り）で渡す — 旧い名前を知っているのは
#                 `migrate-to-celeris.sh` だけ、という置き方（ADR-0045 D3）。
#                 その名前の instance が動いていたら、消さずに止まる。
#
#   --remove-qwen-tunnel  celeris が自分でトンネルを張るようになった（ADR-0053 D3、Phase 66）ので、
#                 旧い `celeris-qwen-tunnel.service` / `.timer`（手で ~/.config/systemd/user/ に置いた
#                 もの。このリポジトリの `deploy/systemd/` には元から無い）を無効化して消す。
#                 **celeris の新しい版が `[[clusters.forwards]]` を張れていることを確認してから**
#                 実行する（無効化した後にトンネルが必要になったら celeris の設定を直す。ADR-0053 D3）。
#                 unit が居なければ何もしない（既に消えている環境でも安全に呼べる）。
set -euo pipefail

SD_PROG=install-units
# shellcheck source=lib.sh
SD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SD_DIR/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: install-units.sh [--remove-old] [--remove-qwen-tunnel]

  --remove-old          改名前のテンプレート unit も ~/.config/systemd/user/ から消す（移行のときだけ）。
  --remove-qwen-tunnel  旧い celeris-qwen-tunnel.service/.timer を無効化して消す（ADR-0053 D3）。
EOF
  exit 2
}

REMOVE_OLD=false
REMOVE_QWEN_TUNNEL=false
while [ $# -gt 0 ]; do
  case "$1" in
    --remove-old) REMOVE_OLD=true; shift ;;
    --remove-qwen-tunnel) REMOVE_QWEN_TUNNEL=true; shift ;;
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

# ADR-0053 D3（Phase 66）: celeris が `[[clusters.forwards]]` を自分で張るようになったので、
# 手で置いた旧い Qwen トンネルの unit（`celeris-qwen-tunnel.service`/`.timer`）を無効化して消す。
# `celeris-qwen-tunnel.*` は `--remove-old` の対象（`SD_OLD_UNITS`）とは別枠 — あれは改名前の
# **テンプレート**（`@.service`）の移行専用で、こちらは名前の変わらない単発 unit なので、
# 同じ「動いていたら止まる」規律を保ったまま別の分岐にする。
if [ "$REMOVE_QWEN_TUNNEL" = true ]; then
  removed_any=false
  for unit in celeris-qwen-tunnel.timer celeris-qwen-tunnel.service; do
    if ! systemctl --user list-unit-files --plain --no-legend "$unit" 2>/dev/null | grep -q .; then
      [ -f "$DEST/$unit" ] || continue
    fi
    # 動いている／有効になっていたら、まず止めて無効化する（テンプレートと違い実 unit なので
    # これ自体は「起動する」操作ではない。ADR-0053 D3 の前提「celeris が forward を張れることを
    # 確認してから」は呼び出し側の責任）。
    systemctl --user disable --now "$unit" >/dev/null 2>&1 || true
    if [ -f "$DEST/$unit" ]; then
      rm -f "$DEST/$unit"
      sd_log "removed the old Qwen tunnel unit $DEST/$unit"
      removed_any=true
    fi
  done
  if [ "$removed_any" = false ]; then
    sd_log "--remove-qwen-tunnel: no celeris-qwen-tunnel.service/.timer found; nothing to do"
  fi
fi

systemctl --user daemon-reload
sd_log "systemctl --user daemon-reload done"
sd_log "next: scripts/selfdeploy/promote.sh <sha12> (a human runs this; see docs/selfdeploy.md)"
