#!/usr/bin/env bash
# scripts/selfdeploy/tests/pid_resolution_test.sh — Phase 119 D3/D5:
#
#   sd_resolve_old_daemon_pid / sd_find_old_celeris_pid / sd_list_stale_celeris_units
#   （scripts/selfdeploy/lib.sh）の単体テスト。
#
# 本番のパス・ポート・systemctl・本物の /proc には触れない: `systemctl` は PATH の先頭に置いた
# 偽スタブで差し替え、`/proc` 相当は一時ディレクトリに `<pid>/cmdline` を作って渡す（どちらの
# 関数も引数で差し替えられるように書かれている。lib.sh 参照）。
#
# 実行: bash scripts/selfdeploy/tests/pid_resolution_test.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib.sh
. "$HERE/../lib.sh"

FAIL=0

assert_eq() {
  local desc="$1" want="$2" got="$3"
  if [ "$want" != "$got" ]; then
    echo "FAIL: $desc (want=[$want] got=[$got])" >&2
    FAIL=1
  else
    echo "ok: $desc"
  fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

make_proc_entry() {
  # make_proc_entry <proc_dir> <pid> <argv...>
  local dir="$1" pid="$2"
  shift 2
  mkdir -p "$dir/$pid"
  printf '%s\0' "$@" >"$dir/$pid/cmdline"
}

FAKE_BIN="$WORK/bin"
mkdir -p "$FAKE_BIN"
CONFIG_PATH="/home/x/.config/celeris/config.toml"
export SD_CONFIG="$CONFIG_PATH"

# ---- ケース 1: systemd が current の release を知っていれば、その MainPID だけを返す -------
#      （実機 2026-09-24 の事故: 複数の draining インスタンスが同じ --config を持つ状態で、
#      /proc 走査が最も古い無関係な pid〈ここでは pid 100〉を拾ってしまっていた）。
cat >"$FAKE_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  "--user is-active --quiet celeris@old12") exit 0 ;;
  "--user show -p MainPID --value celeris@old12") echo 4242; exit 0 ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$FAKE_BIN/systemctl"

PROC1="$WORK/proc1"
mkdir -p "$PROC1"
make_proc_entry "$PROC1" 100 /path/to/celeris --config "$CONFIG_PATH" --release ancient00

GOT="$(PATH="$FAKE_BIN:$PATH" sd_resolve_old_daemon_pid old12 "$PROC1" 999999)"
assert_eq "prefers the systemd MainPID over any /proc scan" 4242 "$GOT"

# ---- ケース 2: systemd がまだこの release を知らない（初回の移行）→ /proc の完全一致に
#      フォールバックする。argv[0]!=celeris（grep 等）や --mode verify（staging）は無視する。
cat >"$FAKE_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$FAKE_BIN/systemctl"

PROC2="$WORK/proc2"
mkdir -p "$PROC2"
make_proc_entry "$PROC2" 200 /usr/bin/grep celeris --config "$CONFIG_PATH"
make_proc_entry "$PROC2" 300 /path/to/celeris --mode verify --config "$CONFIG_PATH"
make_proc_entry "$PROC2" 400 /path/to/celeris --config "$CONFIG_PATH" --release cold00

GOT="$(PATH="$FAKE_BIN:$PATH" sd_resolve_old_daemon_pid "" "$PROC2" 999999)"
assert_eq "falls back to the exact /proc match (grep and --mode verify are skipped)" 400 "$GOT"

GOT_DIRECT="$(sd_find_old_celeris_pid "$PROC2" 999999)"
assert_eq "sd_find_old_celeris_pid alone finds the same exact match" 400 "$GOT_DIRECT"

# ---- ケース 3: 設定パスが違えば拾わない（完全一致） ----------------------------------------
PROC3="$WORK/proc3"
mkdir -p "$PROC3"
make_proc_entry "$PROC3" 500 /path/to/celeris --config /somewhere/else/config.toml
if sd_find_old_celeris_pid "$PROC3" 999999 >/dev/null 2>&1; then
  echo "FAIL: a celeris with a different --config must not match" >&2
  FAIL=1
else
  echo "ok: a celeris with a different --config must not match"
fi

# ---- ケース 4: 他に残っている celeris@* を stale として列挙する（current/new を除く） -------
cat >"$FAKE_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  "--user list-units --type=service --state=active --no-legend --plain celeris@*.service")
    printf 'celeris@old12.service loaded active running\nceleris@new34.service loaded active running\nceleris@stray56.service loaded active running\n'
    exit 0 ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$FAKE_BIN/systemctl"

GOT="$(PATH="$FAKE_BIN:$PATH" sd_list_stale_celeris_units new34 old12)"
assert_eq "only the unrelated unit is reported as stale" stray56 "$GOT"

# ---- ケース 5: current/new 以外に何も残っていなければ空 -------------------------------------
cat >"$FAKE_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  "--user list-units --type=service --state=active --no-legend --plain celeris@*.service")
    printf 'celeris@old12.service loaded active running\nceleris@new34.service loaded active running\n'
    exit 0 ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$FAKE_BIN/systemctl"

GOT="$(PATH="$FAKE_BIN:$PATH" sd_list_stale_celeris_units new34 old12)"
assert_eq "nothing stale when only current/new are active" "" "$GOT"

if [ "$FAIL" -ne 0 ]; then
  echo "pid_resolution_test.sh: FAILED" >&2
  exit 1
fi
echo "pid_resolution_test.sh: all ok"
