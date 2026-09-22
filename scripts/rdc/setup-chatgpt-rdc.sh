#!/usr/bin/env bash
# setup-chatgpt-rdc.sh — ChatGPT の Remote Desktop Commander（RDC）用に、権限最小の専用 Linux ユーザーを整える。
#
# 人が **自分の端末で sudo 付きで** 実行する（celeris の実装エージェントや auto mode の Claude は実行しない）。
# 何度実行しても同じ結果になる（冪等）。
#
#   bash scripts/rdc/setup-chatgpt-rdc.sh [TOKEN_FILE]
#
#   TOKEN_FILE: `celerisctl mcp client add chatgpt-rdc --scope …` が出したトークンを保存した 600 のファイル。
#               与えると専用ユーザーの ~/.config/celeris/mcp-token に 600 で置く（値は表示しない）。
#
# 作るもの（docs/mcp.md §8、ADR-0056 Phase 101 追記）:
#   - ユーザー chatgpt-rdc（sudo 無し・補助グループ無し・ssh 鍵無し・celeris の DB や設定に触れない）
#   - ~chatgpt-rdc/.local/bin/celerisctl   … 現行リリースの celerisctl のコピー（`mcp call` は DB を開かない）
#   - ~chatgpt-rdc/.local/bin/celeris-chat … scripts/rdc/celeris-chat の写し（RDC から呼ぶ唯一の入口）
#   - ~chatgpt-rdc/.config/celeris/mcp-token … MCP client のトークン（600）
# celeris をリリースし直しても `celerisctl mcp call` の互換が保たれる限り再実行は不要。変えたときはもう一度実行する。
set -euo pipefail

RDC_USER=${RDC_USER:-chatgpt-rdc}
CELERIS_HOME=${CELERIS_HOME:-$HOME/.local/celeris}
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
TOKEN_SRC=${1:-}

die() { echo "setup-chatgpt-rdc: $*" >&2; exit 1; }
[ "$(id -u)" -ne 0 ] || die "root ではなく、sudo の使える通常ユーザーで実行してください"
sudo -n true 2>/dev/null || die "sudo が使えません（パスワード付きなら先に 'sudo -v' を実行）"
[ -x "$CELERIS_HOME/current/bin/celerisctl" ] || die "celerisctl が見つかりません: $CELERIS_HOME/current/bin/celerisctl"
[ -f "$REPO/scripts/rdc/celeris-chat" ] || die "scripts/rdc/celeris-chat がありません（Phase 101 以降のリポジトリで実行）"
if [ -n "$TOKEN_SRC" ]; then
  [ -f "$TOKEN_SRC" ] || die "TOKEN_FILE が無い: $TOKEN_SRC"
  [ "$(stat -c '%a' "$TOKEN_SRC")" = "600" ] || die "TOKEN_FILE は 600 にしてください: $TOKEN_SRC"
fi

if id "$RDC_USER" >/dev/null 2>&1; then
  echo "user $RDC_USER: exists"
else
  sudo -n useradd -m -s /bin/bash -c "ChatGPT Remote Desktop Commander agent (Celeris MCP only)" "$RDC_USER"
  echo "user $RDC_USER: created"
fi
# 補助グループ（sudo / docker など）が付いていたら外す。
if [ -n "$(id -nG "$RDC_USER" | tr ' ' '\n' | grep -vx "$RDC_USER" || true)" ]; then
  sudo -n usermod -G "" "$RDC_USER"
  echo "user $RDC_USER: removed supplementary groups"
fi
HOME_DIR=$(getent passwd "$RDC_USER" | cut -d: -f6)
sudo -n chmod 700 "$HOME_DIR"
sudo -n install -d -m 700 -o "$RDC_USER" -g "$RDC_USER" "$HOME_DIR/.config" "$HOME_DIR/.config/celeris"
sudo -n install -d -m 755 -o "$RDC_USER" -g "$RDC_USER" "$HOME_DIR/.local" "$HOME_DIR/.local/bin"
sudo -n install -m 755 -o "$RDC_USER" -g "$RDC_USER" "$CELERIS_HOME/current/bin/celerisctl" "$HOME_DIR/.local/bin/celerisctl"
sudo -n install -m 755 -o "$RDC_USER" -g "$RDC_USER" "$REPO/scripts/rdc/celeris-chat" "$HOME_DIR/.local/bin/celeris-chat"
echo "installed: $HOME_DIR/.local/bin/{celerisctl,celeris-chat} (from $(readlink -f "$CELERIS_HOME/current"))"
if [ -n "$TOKEN_SRC" ]; then
  sudo -n install -m 600 -o "$RDC_USER" -g "$RDC_USER" "$TOKEN_SRC" "$HOME_DIR/.config/celeris/mcp-token"
  echo "installed: $HOME_DIR/.config/celeris/mcp-token (600; value not shown)"
fi
# PATH に ~/.local/bin を足す（bash のログインシェル）。
if ! sudo -n grep -qs 'HOME/.local/bin' "$HOME_DIR/.profile"; then
  printf '\n# celeris-chat / celerisctl\nexport PATH="$HOME/.local/bin:$PATH"\n' | sudo -n tee -a "$HOME_DIR/.profile" >/dev/null
fi

echo
echo "check: sudo rights of $RDC_USER (expect: none)"
sudo -n -l -U "$RDC_USER" 2>&1 | tail -1
echo "check: groups = $(id -nG "$RDC_USER")"
echo
cat <<NEXT
next (人が行う):
  1. 動作確認:  sudo -iu $RDC_USER -- celeris-chat --list
  2. RDC の端末を専用ユーザーで開いて対にする（ブラウザで device verification）:
       sudo -iu $RDC_USER
       tmux new -s rdc     # 端末を閉じると切れるので tmux の中で
       npx @wonderwhy-er/desktop-commander@latest remote
  3. ChatGPT: Settings → Apps & Connectors → Advanced settings → Developer mode → Create connector →
       https://mcp.desktopcommander.app/mcp
  4. RDC の allowedDirectories を \$HOME（$HOME_DIR）だけにし、blockedCommands に curl / wget / ssh / scp / sqlite3 を足す
     （guardrail であって sandbox ではない。境界は Linux ユーザーと MCP client の scope）。
NEXT
