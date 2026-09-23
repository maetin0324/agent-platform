#!/usr/bin/env bash
# scripts/selfdeploy/relocate-db.sh <new-path> [--dry-run] — ADR-0064 D2（Phase 110a）。
#
#   DB ファイルだけをローカルディスク（例 /var/lib/celeris/celeris.sqlite3）へ移す。状態ディレクトリ
#   （$CELERIS_STATE_DIR = 既定 ~/.local/celeris: workspaces/releases/backups/tools）はそのまま。
#
#   **人だけが素のコマンドで実行する**（実装エージェントは本番で実行しない。ADR-0040 D5 と同じ規律）。
#
#   手順:
#     1. in-flight（running + reviewing）が 0 であることを API（GET /api/v1/tasks の
#        counts_by_status）で確認する。0 でなければ何もせず止まる。
#     2. celeris@* / celeris-gui@* を止める（`current` の sha。動いていなければ何もしない）。
#     3. `sqlite3` があれば `VACUUM INTO`、無ければ現在のリリースの `celerisctl db backup` で
#        <new-path> へコピーする。
#     4. <new-path> に対して `PRAGMA integrity_check`（`sqlite3` か `celerisctl db integrity-check`）。
#        `ok` でなければ、<new-path> を消して止まる（旧 DB とサービスは無傷のまま）。
#     5. config.toml の `db =`（文字列 or `[db].path` テーブル）を書き換える。`.bak-<ts>` を残す。
#     6. 旧ファイルを `<old>.moved-<ts>` にリネームする（**削除しない**）。
#     7. celeris@ / celeris-gui@ を起こす。
#     8. `GET /api/v1/config`（認証つき。`/health` は DB のパスを無認証で出さないので使わない。
#        ADR-0064 D1）の `db` が <new-path> になっていることを確認する。
#
#   各段階で失敗したら、その時点までに済んだことと戻し方を stderr に出して exit 1 で止まる
#   （自動では戻さない。DB を触る操作の巻き戻しを無人で行うのは危険なため）。
#
#   --dry-run   何も止めず・書かず・コピーせずに、計画（元・先・現在の sha・止める unit）と
#               in-flight の確認結果だけを出す。
#
#   ディレクトリは作らない。<new-path> の親ディレクトリが無ければ、人が sudo で作る前提で
#   分かりやすく止まる（例: `sudo install -d -o $(whoami) -g $(whoami) -m 0750 /var/lib/celeris`）。
#
#   冪等: 現在の設定の `db` が既に <new-path> なら、何もせず「already relocated」で exit 0
#   （--dry-run でも同じ）。
set -euo pipefail

SD_PROG=relocate-db
# shellcheck source=lib.sh
SD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SD_DIR/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: relocate-db.sh <new-path> [--dry-run]

  <new-path>  DB ファイルの新しい絶対パス（例 /var/lib/celeris/celeris.sqlite3）。
              親ディレクトリは先に人が作っておくこと（このスクリプトはディレクトリを作らない）。
  --dry-run   何も変えずに計画と in-flight の確認結果だけを出す。

env:
  CELERIS_CONFIG_DIR  既定 ~/.config/celeris（lib.sh と同じ）
  CELERIS_STATE_DIR   既定 ~/.local/celeris
EOF
  exit 2
}

DRY_RUN=false
NEW=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    -h | --help) usage ;;
    -*) usage ;;
    *)
      [ -z "$NEW" ] || usage
      NEW="$1"
      shift
      ;;
  esac
done
[ -n "$NEW" ] || usage

sd_require_json_tool

case "$NEW" in
  /*) ;;
  *) sd_die "<new-path> must be absolute (got: $NEW)" ;;
esac

OLD="$SD_DB"
[ -f "$OLD" ] || sd_die "current db $OLD does not exist (check $SD_CONFIG's db=, or \$CELERIS_DB)"

# ---- 冪等性: 既に新パスなら何もしない --------------------------------------

OLD_REAL="$(readlink -f "$OLD" 2>/dev/null || printf '%s' "$OLD")"
NEW_REAL_PARENT_CHECK="$(dirname "$NEW")"
if [ "$OLD_REAL" = "$(readlink -f "$NEW" 2>/dev/null || printf '%s' "$NEW")" ]; then
  sd_log "already relocated: db is already $NEW"
  exit 0
fi

if [ -e "$NEW" ]; then
  sd_die "$NEW already exists; choose a fresh path or remove it first (this script never overwrites)"
fi

[ -d "$NEW_REAL_PARENT_CHECK" ] \
  || sd_die "parent directory $NEW_REAL_PARENT_CHECK does not exist; create it first (e.g. sudo install -d -o \$(whoami) -g \$(whoami) -m 0750 $NEW_REAL_PARENT_CHECK)"

TS="$(sd_stamp)"
mkdir -p "$SD_BACKUPS"
SD_LOG_FILE="$SD_BACKUPS/relocate-db-$TS.log"

sd_log "relocate-db: $OLD -> $NEW (dry_run=$DRY_RUN)"

# ---- 1. in-flight の確認（読むだけ。--dry-run でも行う） --------------------

RUNNING=0
REVIEWING=0
if [ "$(sd_http_status "$SD_PROD_API/api/v1/health")" = 200 ]; then
  TASKS_JSON="$(mktemp)"
  if sd_http_get "$SD_PROD_API/api/v1/tasks?limit=1" "$SD_API_TOKEN_FILE" >"$TASKS_JSON" 2>/dev/null; then
    RUNNING="$(sd_json_get "$TASKS_JSON" counts_by_status.running 2>/dev/null || echo 0)"
    REVIEWING="$(sd_json_get "$TASKS_JSON" counts_by_status.reviewing 2>/dev/null || echo 0)"
    [ -n "$RUNNING" ] || RUNNING=0
    [ -n "$REVIEWING" ] || REVIEWING=0
  else
    rm -f "$TASKS_JSON"
    sd_die "could not read GET /api/v1/tasks (is \$CELERIS_CONFIG_DIR/api.token current?); refusing to guess in-flight is 0"
  fi
  rm -f "$TASKS_JSON"
else
  sd_log "celeris API is not reachable at $SD_PROD_API (already stopped?); treating in-flight as unknown"
  sd_log "if celeris is really down, in-flight is vacuously 0; proceeding, but double-check status.sh first"
fi
IN_FLIGHT=$((RUNNING + REVIEWING))
sd_log "in-flight: running=$RUNNING reviewing=$REVIEWING (total $IN_FLIGHT)"
if [ "$IN_FLIGHT" -gt 0 ]; then
  sd_die "in-flight is $IN_FLIGHT (running=$RUNNING reviewing=$REVIEWING); wait for it to drain or let it finish, then retry"
fi

CUR="$(sd_current_sha)"
UNITS=()
if [ -n "$CUR" ]; then
  if systemctl --user is-active --quiet "celeris@$CUR" 2>/dev/null; then UNITS+=("celeris@$CUR"); fi
  if systemctl --user is-active --quiet "celeris-gui@$CUR" 2>/dev/null; then UNITS+=("celeris-gui@$CUR"); fi
fi

if [ "$DRY_RUN" = true ]; then
  sd_log "dry-run plan:"
  sd_log "  1. stop: ${UNITS[*]:-（見つからない。既に止まっている？）}"
  sd_log "  2. copy: $OLD -> $NEW (VACUUM INTO if sqlite3, else celerisctl db backup)"
  sd_log "  3. integrity check $NEW"
  sd_log "  4. rewrite db in $SD_CONFIG (backup to $SD_CONFIG.bak-$TS)"
  sd_log "  5. rename $OLD -> $OLD.moved-$TS (not deleted)"
  sd_log "  6. start: ${UNITS[*]:-（celeris@<current sha>・celeris-gui@<current sha>）}"
  sd_log "  7. verify GET /api/v1/config db == $NEW"
  sd_log "dry-run: nothing was changed"
  exit 0
fi

# ---- 現在のリリースの celerisctl（sqlite3 が無い環境のフォールバック） -----

CELERISCTL="$SD_CURRENT/bin/celerisctl"

fail_with_recovery() {
  local stage="$1"
  shift
  sd_log "ERROR at stage '$stage': $*"
  sd_log "recovery:"
  case "$stage" in
    stop-units)
      sd_log "  nothing was stopped or changed; just fix the reported problem and retry"
      ;;
    copy | integrity-check)
      sd_log "  the old db ($OLD) and the running units were not touched."
      sd_log "  remove the partial copy if it exists: rm -f '$NEW' '$NEW-wal' '$NEW-shm'"
      if [ "${#UNITS[@]}" -gt 0 ]; then
        sd_log "  the units were already stopped; start them again: systemctl --user start ${UNITS[*]}"
      fi
      ;;
    rewrite-config)
      sd_log "  $NEW is a verified copy but $SD_CONFIG may be half-written."
      sd_log "  restore it: cp '$SD_CONFIG.bak-$TS' '$SD_CONFIG' (if that backup exists)"
      if [ "${#UNITS[@]}" -gt 0 ]; then
        sd_log "  then start the units again: systemctl --user start ${UNITS[*]}"
      fi
      ;;
    move-old)
      sd_log "  $SD_CONFIG now points at $NEW (a verified copy) but the old file at $OLD was not moved."
      sd_log "  that is harmless (both files exist); just start the units:"
      [ "${#UNITS[@]}" -gt 0 ] && sd_log "    systemctl --user start ${UNITS[*]}"
      ;;
    start-units)
      sd_log "  $SD_CONFIG points at $NEW and the old db was moved to $OLD.moved-$TS."
      sd_log "  the unit(s) failed to start; check: journalctl --user -u ${UNITS[*]:-celeris@} -n 100"
      sd_log "  to fully revert: cp '$SD_CONFIG.bak-$TS' '$SD_CONFIG' && mv '$OLD.moved-$TS' '$OLD' && systemctl --user start ${UNITS[*]:-celeris@$CUR celeris-gui@$CUR}"
      ;;
    verify)
      sd_log "  the units are running on $NEW, but GET /api/v1/config did not confirm it."
      sd_log "  check manually: curl -sS -H \"Authorization: Bearer \$(cat $SD_API_TOKEN_FILE)\" $SD_PROD_API/api/v1/config"
      sd_log "  to revert: systemctl --user stop ${UNITS[*]:-celeris@$CUR celeris-gui@$CUR}; cp '$SD_CONFIG.bak-$TS' '$SD_CONFIG'; mv '$OLD.moved-$TS' '$OLD'; systemctl --user start ${UNITS[*]:-celeris@$CUR celeris-gui@$CUR}"
      ;;
  esac
  exit 1
}

# ---- 2. 止める --------------------------------------------------------------

if [ "${#UNITS[@]}" -gt 0 ]; then
  sd_log "systemctl --user stop ${UNITS[*]}"
  systemctl --user stop "${UNITS[@]}" || fail_with_recovery stop-units "systemctl stop failed"
else
  sd_log "no active celeris@/celeris-gui@ units found for current=$CUR; continuing (already stopped?)"
fi

# ---- 3. コピー --------------------------------------------------------------

if command -v sqlite3 >/dev/null 2>&1; then
  sd_log "sqlite3 VACUUM INTO '$NEW'"
  sqlite3 "$OLD" "VACUUM INTO '$NEW';" || fail_with_recovery copy "sqlite3 VACUUM INTO failed"
elif [ -x "$CELERISCTL" ]; then
  sd_log "$CELERISCTL db backup $NEW --src $OLD (no sqlite3 on PATH)"
  "$CELERISCTL" db backup "$NEW" --src "$OLD" || fail_with_recovery copy "celerisctl db backup failed"
else
  fail_with_recovery copy "neither sqlite3 nor a current-release celerisctl ($CELERISCTL) is available"
fi

# ---- 4. 整合性確認 -----------------------------------------------------------

INTEGRITY_OK=false
if command -v sqlite3 >/dev/null 2>&1; then
  RESULT="$(sqlite3 "$NEW" "PRAGMA integrity_check;" 2>/dev/null || echo "")"
  [ "$RESULT" = "ok" ] && INTEGRITY_OK=true
elif [ -x "$CELERISCTL" ]; then
  "$CELERISCTL" db integrity-check "$NEW" >/dev/null 2>&1 && INTEGRITY_OK=true
fi
if [ "$INTEGRITY_OK" != true ]; then
  rm -f "$NEW" "$NEW-wal" "$NEW-shm"
  fail_with_recovery integrity-check "PRAGMA integrity_check on $NEW did not return ok; removed the partial copy"
fi
sd_log "integrity check ok: $NEW"

# ---- 5. config.toml を書き換える -------------------------------------------

cp -p "$SD_CONFIG" "$SD_CONFIG.bak-$TS" || fail_with_recovery rewrite-config "could not back up $SD_CONFIG"
python3 - "$SD_CONFIG" "$NEW" <<'PY' || fail_with_recovery rewrite-config "python3 rewrite failed"
import re
import sys

path, new_db = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as fh:
    text = fh.read()

# トップレベルの `db = "..."` を書き換える（`[db]` テーブルより前、他の節の中の同名キーには触れない）。
# `^\[` は行頭のテーブル見出しにマッチする（MULTILINE なのでファイル先頭が `[db]` でも拾える）。
first_table = re.search(r"^\[", text, re.MULTILINE)
if first_table:
    head, rest = text[: first_table.start()], text[first_table.start() :]
else:
    head, rest = text, ""

# 行末の `[ \t]*(#.*)?$` は改行を消費せず（`\s*$` だと、置換対象がブロック末尾＝文字列の末尾に
# ちょうど接するとき改行ごと飲み込んでしまい、直後のテーブル見出しと連結して壊れる）、行末コメント
# （`db = "..."  # ...`）も許す（`config/celeris.example.toml` の `db` 行がまさにこの形）。
TRAILING = r'[ \t]*(#.*)?$'
pattern = re.compile(r'^db\s*=\s*"[^"]*"' + TRAILING, re.MULTILINE)
escaped = new_db.replace("\\", "\\\\").replace('"', '\\"')
replacement = f'db = "{escaped}"'
if pattern.search(head):
    head = pattern.sub(replacement, head, count=1)
elif re.search(r"^\[db\]", rest, re.MULTILINE):
    # `[db]` テーブルの `path = "..."` を書き換える。
    def sub_table(m: "re.Match[str]") -> str:
        block = m.group(0)
        path_pattern = re.compile(r'^(\s*path\s*=\s*)"[^"]*"' + TRAILING, re.MULTILINE)
        if path_pattern.search(block):
            return path_pattern.sub(lambda pm: f'{pm.group(1)}"{escaped}"', block, count=1)
        # `path` が無い（既定を使っていた）テーブルなら先頭に足す。
        return block.replace("[db]", f'[db]\npath = "{escaped}"', 1)

    rest = re.sub(r"\[db\][^\[]*", sub_table, rest, count=1, flags=re.DOTALL)
else:
    # どちらも書いていなかった（既定のパスを使っていた）。トップレベルの先頭に足す。
    head = f'db = "{escaped}"\n' + head

with open(path, "w", encoding="utf-8") as fh:
    fh.write(head + rest)
PY
sd_log "rewrote db in $SD_CONFIG (backup: $SD_CONFIG.bak-$TS)"

# ---- 6. 旧ファイルを退避（消さない） ---------------------------------------

mv "$OLD" "$OLD.moved-$TS" || fail_with_recovery move-old "could not rename $OLD"
for ext in wal shm; do
  [ -f "$OLD-$ext" ] && mv "$OLD-$ext" "$OLD.moved-$TS-$ext"
done
sd_log "moved old db aside: $OLD -> $OLD.moved-$TS"

# ---- 7. 起こす --------------------------------------------------------------

if [ "${#UNITS[@]}" -gt 0 ]; then
  sd_log "systemctl --user start ${UNITS[*]}"
  systemctl --user start "${UNITS[@]}" || fail_with_recovery start-units "systemctl start failed"
else
  sd_log "no units were stopped in step 2, so none are started here (celeris was already down)"
fi

# ---- 8. 確認 ----------------------------------------------------------------

if [ "${#UNITS[@]}" -gt 0 ]; then
  sd_wait_http_200 "$SD_PROD_API/api/v1/health" 30 || fail_with_recovery verify "celeris did not come back up within 30s"
  CONFIG_JSON="$(mktemp)"
  if sd_http_get "$SD_PROD_API/api/v1/config" "$SD_API_TOKEN_FILE" >"$CONFIG_JSON" 2>/dev/null; then
    GOT_DB="$(sd_json_get "$CONFIG_JSON" db 2>/dev/null || echo "")"
    rm -f "$CONFIG_JSON"
    if [ "$GOT_DB" != "$NEW" ]; then
      fail_with_recovery verify "GET /api/v1/config db is '$GOT_DB', expected '$NEW'"
    fi
    sd_log "confirmed: GET /api/v1/config db == $NEW"
  else
    rm -f "$CONFIG_JSON"
    fail_with_recovery verify "could not read GET /api/v1/config after restart"
  fi
else
  sd_log "celeris was not running before this script; not starting it or verifying (config now points at $NEW)"
fi

sd_log "relocate-db done: $OLD -> $NEW"
