#!/usr/bin/env bash
# scripts/selfdeploy/promote.sh <sha12> — ADR-0040 D4 の「昇格」段。**人だけが実行する**（D5）。
#
#   verify.json.ok が真でなければ拒否する（`--force` は無い）。DB をバックアップし、
#     - ライブ引き継ぎ（verify.json.live_ok が真で、いま動いている taskd の /health が `role` を持つ）
#     - 停止 → 起動（live_ok が偽、または /health に `role` が無い＝この ADR 以前の版。初回の移行はこれ）
#   のどちらかで切り替え、`current` / `previous` を更新する。すべて
#   ~/taskd/backups/promote-<ts>.log に残る。
set -euo pipefail

SD_PROG=promote
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: promote.sh <sha12>

  verify.sh が `ok` を出したリリースだけを昇格できる。`--force` は無い（ADR-0040 D2）。
  systemd の unit が要る: scripts/selfdeploy/install-units.sh を一度だけ実行しておくこと。
EOF
  exit 2
}

[ $# -eq 1 ] || usage
SHA12="$1"

sd_require_json_tool
TS="$(sd_stamp)"
mkdir -p "$SD_BACKUPS"
SD_LOG_FILE="$SD_BACKUPS/promote-$TS.log"
sd_log "promote $SHA12 (log: $SD_LOG_FILE)"

REL="$(sd_release_dir "$SHA12")"
[ -d "$REL" ] || sd_die "no such release: $REL"
[ -x "$REL/bin/taskd" ] || sd_die "missing $REL/bin/taskd"
[ -f "$REL/verify.json" ] || sd_die "$SHA12 has no verify.json — run verify.sh first"
[ "$(sd_json_get "$REL/verify.json" ok 2>/dev/null || echo false)" = true ] \
  || sd_die "verify.json of $SHA12 is not ok — refusing to promote (there is no --force)"

LIVE_OK="$(sd_json_get "$REL/verify.json" live_ok 2>/dev/null || echo false)"
WANT_SCHEMA="$(sd_json_get "$REL/manifest.json" schema_version)"
OLD="$(sd_current_sha)"
sd_log "release=$SHA12 schema_version=$WANT_SCHEMA verify.live_ok=$LIVE_OK current=${OLD:-<none>}"

# ---- systemd の unit があるか ----------------------------------------------

systemctl --user cat taskd@.service >/dev/null 2>&1 \
  || sd_die "systemd user template taskd@.service is not installed; run scripts/selfdeploy/install-units.sh first"
systemctl --user cat taskd-gui@.service >/dev/null 2>&1 \
  || sd_die "systemd user template taskd-gui@.service is not installed; run scripts/selfdeploy/install-units.sh first"

# ---- いま動いているものを見る（読むだけ） ----------------------------------

PROD_HEALTH="$SD_BACKUPS/promote-$TS.health-before.json"
LIVE_ROLE=""
LIVE_RELEASE=""
if [ "$(sd_http_status "$SD_PROD_API/api/v1/health")" = 200 ]; then
  sd_http_get "$SD_PROD_API/api/v1/health" >"$PROD_HEALTH" || true
  LIVE_ROLE="$(sd_json_get "$PROD_HEALTH" role 2>/dev/null || echo "")"
  LIVE_RELEASE="$(sd_json_get "$PROD_HEALTH" release 2>/dev/null || echo "")"
  sd_log "running taskd: release=${LIVE_RELEASE:-<unknown>} role=${LIVE_ROLE:-<none>} schema_version=$(sd_json_get "$PROD_HEALTH" schema_version || echo '?')"
else
  sd_log "no healthy taskd on $SD_PROD_API right now"
fi

if [ "$LIVE_RELEASE" = "$SHA12" ]; then
  sd_die "the running taskd already reports release=$SHA12; nothing to promote"
fi

MODE=stop-start
if [ "$LIVE_OK" = true ] && [ -n "$LIVE_ROLE" ]; then MODE=live; fi
sd_log "promotion mode: $MODE"

# ---- 道具 ------------------------------------------------------------------

BACKUP="$SD_BACKUPS/$TS-pre-$SHA12.sqlite3"
backup_db() {
  sd_log "backing up the production DB (read-only) to $BACKUP"
  sqlite3 "file:$SD_DB?mode=ro" ".backup '$BACKUP'" || sd_die "sqlite3 .backup failed"
  sd_log "backup ok: $(du -h "$BACKUP" | cut -f1)"
}

# `poll_release <url> <json-path-of-release> <want> <role-path|-> <want-role|-> <timeout> ` —
# SO_REUSEPORT で新旧が同じポートを共有するので、1 回の応答では足りない。
# 毎秒 5 回サンプルし、**全部**が期待どおりになったら成功（＝旧はもう listener を閉じている）。
poll_release() {
  local url="$1" rel_path="$2" want="$3" role_path="$4" want_role="$5" timeout="$6"
  local waited=0 i all tmp got_rel got_role
  tmp="$(mktemp)"
  while [ "$waited" -lt "$timeout" ]; do
    all=true
    for i in 1 2 3 4 5; do
      if ! sd_http_get "$url" >"$tmp" 2>/dev/null; then all=false; break; fi
      sd_json_valid "$tmp" || { all=false; break; }
      got_rel="$(sd_json_get "$tmp" "$rel_path" 2>/dev/null || echo "")"
      [ "$got_rel" = "$want" ] || { all=false; break; }
      if [ "$role_path" != "-" ]; then
        got_role="$(sd_json_get "$tmp" "$role_path" 2>/dev/null || echo "")"
        [ "$got_role" = "$want_role" ] || { all=false; break; }
      fi
    done
    if [ "$all" = true ]; then
      rm -f "$tmp"
      if [ "$role_path" != "-" ]; then
        sd_log "poll $url: release=$want role=$want_role after ${waited}s (5/5 samples)"
      else
        sd_log "poll $url: release=$want after ${waited}s (5/5 samples)"
      fi
      return 0
    fi
    sleep 1
    waited=$((waited + 1))
  done
  sd_log "poll $url: gave up after ${timeout}s (last release=${got_rel:-?} role=${got_role:-?})"
  rm -f "$tmp"
  return 1
}

# 本番の taskd の pid を**設定パスまで含めた完全一致**で探す（ADR-0040 D4）。
# `pgrep -f` は自分のシェルのコマンドラインにも当たるので使わない（過去に踏んだ）。/proc を直接見る。
find_old_taskd_pid() {
  local pid i argv matched skip
  for pid in /proc/[0-9]*; do
    pid="${pid#/proc/}"
    if [ "$pid" = "$$" ]; then continue; fi
    if [ ! -r "/proc/$pid/cmdline" ]; then continue; fi
    argv=()
    mapfile -d '' -t argv <"/proc/$pid/cmdline" 2>/dev/null || continue
    if [ "${#argv[@]}" -lt 3 ]; then continue; fi
    # argv[0] が `taskd` そのものでなければ対象外（`grep`、`bash -c`、`sh -c` はここで落ちる）。
    if [ "$(basename -- "${argv[0]}")" != taskd ]; then continue; fi
    # staging（verify.sh が起こす `--mode verify --db … --listen …`）は本番ではない。
    skip=false
    for i in "${argv[@]}"; do
      case "$i" in --mode | --db | --listen | --workspace-root | --token-file) skip=true ;; esac
    done
    if [ "$skip" = true ]; then continue; fi
    matched=false
    for i in $(seq 0 $((${#argv[@]} - 2))); do
      if [ "${argv[$i]}" = "--config" ] && [ "${argv[$((i + 1))]}" = "$SD_CONFIG" ]; then matched=true; fi
    done
    if [ "$matched" != true ]; then continue; fi
    printf '%s' "$pid"
    return 0
  done
  return 1
}

# 0.0.0.0:7700 で LISTEN している node の pid（初回の移行で旧 GUI を止めるため）。
find_old_gui_pid() {
  local pid
  pid="$(ss -ltnp "sport = :$SD_PROD_GUI_PORT" 2>/dev/null | sed -n 's/.*pid=\([0-9]\+\).*/\1/p' | head -n 1)"
  [ -n "$pid" ] || return 1
  printf '%s' "$pid"
}

kill_grace_secs() {
  local v
  v="$(sed -n 's/^[[:space:]]*kill_grace_secs[[:space:]]*=[[:space:]]*\([0-9]\+\).*$/\1/p' "$SD_CONFIG" | head -n 1)"
  printf '%s' "${v:-10}"
}

update_links() {
  if [ -n "$OLD" ] && [ -d "$(sd_release_dir "$OLD")" ]; then
    sd_set_link "$SD_PREVIOUS" "$OLD"
    sd_log "previous -> releases/$OLD"
  fi
  sd_set_link "$SD_CURRENT" "$SHA12"
  sd_log "current  -> releases/$SHA12"
}

# ---- ライブ引き継ぎ --------------------------------------------------------

promote_live() {
  backup_db
  sd_log "systemctl --user start taskd@$SHA12"
  systemctl --user start "taskd@$SHA12" || sd_die "failed to start taskd@$SHA12"
  if ! poll_release "$SD_PROD_API/api/v1/health" release "$SHA12" role active 60; then
    sd_log "handoff did not complete within 60s; stopping taskd@$SHA12 and leaving the old one alone"
    systemctl --user stop "taskd@$SHA12" || true
    sd_die "live handoff failed (the old taskd is still serving; nothing was changed)"
  fi
  sd_log "handoff done: the new taskd is active"

  systemctl --user enable "taskd@$SHA12" || sd_log "warning: enable taskd@$SHA12 failed"
  if [ -n "$OLD" ]; then
    systemctl --user disable "taskd@$OLD" || sd_log "warning: disable taskd@$OLD failed"
  fi

  sd_log "systemctl --user start taskd-gui@$SHA12"
  systemctl --user start "taskd-gui@$SHA12" || sd_die "failed to start taskd-gui@$SHA12 (taskd is already the new one)"
  if ! poll_release "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz" release "$SHA12" - - 60; then
    systemctl --user stop "taskd-gui@$SHA12" || true
    sd_die "the new GUI did not take over :$SD_PROD_GUI_PORT within 60s (taskd is already the new one; fix the GUI by hand)"
  fi
  if [ -n "$OLD" ] && systemctl --user is-active --quiet "taskd-gui@$OLD"; then
    sd_log "systemctl --user stop taskd-gui@$OLD"
    systemctl --user stop "taskd-gui@$OLD" || sd_log "warning: stop taskd-gui@$OLD failed"
  fi
  systemctl --user enable "taskd-gui@$SHA12" || sd_log "warning: enable taskd-gui@$SHA12 failed"
  if [ -n "$OLD" ]; then
    systemctl --user disable "taskd-gui@$OLD" || sd_log "warning: disable taskd-gui@$OLD failed"
  fi

  update_links
  sd_log "live handoff complete. The old taskd keeps draining its runs; watch it with status.sh."
}

# ---- 停止 → 起動（初回の移行はこれ） --------------------------------------

promote_stop_start() {
  local old_pid grace waited old_gui_pid
  grace="$(kill_grace_secs)"

  if old_pid="$(find_old_taskd_pid)"; then
    sd_log "old taskd pid=$old_pid (exact match on: taskd --config $SD_CONFIG)"
    sd_log "SIGTERM $old_pid; waiting up to $((grace + 10))s"
    kill -TERM "$old_pid"
    waited=0
    while kill -0 "$old_pid" 2>/dev/null && [ "$waited" -lt $((grace + 10)) ]; do
      sleep 1
      waited=$((waited + 1))
    done
    if kill -0 "$old_pid" 2>/dev/null; then
      sd_die "old taskd (pid $old_pid) did not exit within $((grace + 10))s; stopping here (nothing was changed)"
    fi
    sd_log "old taskd exited after ${waited}s"
  elif [ -n "$OLD" ] && systemctl --user is-active --quiet "taskd@$OLD"; then
    sd_log "systemctl --user stop taskd@$OLD"
    systemctl --user stop "taskd@$OLD" || sd_die "failed to stop taskd@$OLD"
  else
    sd_log "no running taskd found; starting the new one on a cold DB"
  fi

  # 旧が止まってからバックアップを取る（ADR-0040 D4 の順。引き継ぎ中の仕事も入る）。
  backup_db

  sd_log "systemctl --user start taskd@$SHA12"
  if ! systemctl --user start "taskd@$SHA12"; then
    sd_log "start taskd@$SHA12 failed"
    restore_old_daemon
    sd_die "could not start taskd@$SHA12"
  fi
  if ! sd_wait_http_200 "$SD_PROD_API/api/v1/health" 60; then
    sd_log "no 200 from $SD_PROD_API/api/v1/health within 60s"
    systemctl --user stop "taskd@$SHA12" || true
    restore_old_daemon
    sd_die "the new taskd did not become healthy. The DB was NOT restored (the schema may have moved forward): use rollback.sh --restore-db if you need the pre-promotion DB ($BACKUP)"
  fi
  sd_http_get "$SD_PROD_API/api/v1/health" >"$SD_BACKUPS/promote-$TS.health-after.json" || true
  GOT_SCHEMA="$(sd_json_get "$SD_BACKUPS/promote-$TS.health-after.json" schema_version || echo "?")"
  if [ "$GOT_SCHEMA" != "$WANT_SCHEMA" ]; then
    sd_log "health.schema_version=$GOT_SCHEMA but the release says $WANT_SCHEMA"
    systemctl --user stop "taskd@$SHA12" || true
    restore_old_daemon
    sd_die "schema_version mismatch after start. The DB was NOT restored ($BACKUP is the pre-promotion copy)"
  fi
  sd_log "new taskd is healthy: schema_version=$GOT_SCHEMA"
  systemctl --user enable "taskd@$SHA12" || sd_log "warning: enable taskd@$SHA12 failed"
  if [ -n "$OLD" ]; then
    systemctl --user disable "taskd@$OLD" || sd_log "warning: disable taskd@$OLD failed"
  fi

  # GUI: 初回の移行では systemd の外で動いている `node server.js` を止める。
  if old_gui_pid="$(find_old_gui_pid)"; then
    sd_log "old GUI pid=$old_gui_pid on :$SD_PROD_GUI_PORT; SIGTERM"
    kill -TERM "$old_gui_pid" || true
    waited=0
    while kill -0 "$old_gui_pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
      sleep 1
      waited=$((waited + 1))
    done
    if kill -0 "$old_gui_pid" 2>/dev/null; then sd_log "warning: old GUI pid $old_gui_pid is still alive"; fi
  else
    sd_log "nothing is listening on :$SD_PROD_GUI_PORT"
  fi
  sd_log "systemctl --user start taskd-gui@$SHA12"
  systemctl --user start "taskd-gui@$SHA12" || sd_die "taskd is the new one, but taskd-gui@$SHA12 did not start"
  if ! sd_wait_http_200 "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz" 60; then
    sd_die "taskd is the new one, but the GUI did not answer /healthz on :$SD_PROD_GUI_PORT within 60s"
  fi
  systemctl --user enable "taskd-gui@$SHA12" || sd_log "warning: enable taskd-gui@$SHA12 failed"
  if [ -n "$OLD" ]; then
    systemctl --user disable "taskd-gui@$OLD" || sd_log "warning: disable taskd-gui@$OLD failed"
  fi

  update_links
  sd_log "stop-start promotion complete"
}

# 失敗したときに旧 unit を起こし直す（unit が無い＝初回の移行なら、人に任せる）。
restore_old_daemon() {
  if [ -n "$OLD" ] && systemctl --user cat "taskd@$OLD" >/dev/null 2>&1; then
    sd_log "restarting the old unit: systemctl --user start taskd@$OLD"
    systemctl --user start "taskd@$OLD" || sd_log "warning: could not restart taskd@$OLD"
  else
    sd_log "there is no old systemd unit to restart (this was the first migration)."
    sd_log "the old binary is at ${SD_REPO}/target/debug/taskd — a human decides what to start."
  fi
}

case "$MODE" in
  live) promote_live ;;
  stop-start) promote_stop_start ;;
esac

# ---- promoted.json（ADR-0041 D3）------------------------------------------
#
# 「このリリースが、いつ、どの版から、どうやって昇格したか」を**リリースの中に**残す。
# `GET /releases` の `promoted_at` はこれを読むだけ（Phase 48 の逸脱 2「どこにも書かれていない」の解消）。
# **git リポジトリには触れない**（人のチェックアウトを機械が fast-forward しない。ADR-0041 D3）。
# `main` に反映されているかは `GET /releases` の `on_main` が読み取りで見せる。
{
  printf '{\n'
  printf '  "promoted_at": %s,\n' "$(sd_json_str "$(sd_ts)")"
  printf '  "mode": %s,\n' "$(sd_json_str "$MODE")"
  if [ -n "$OLD" ]; then
    printf '  "from": %s\n' "$(sd_json_str "$OLD")"
  else
    printf '  "from": null\n'
  fi
  printf '}\n'
} >"$REL/promoted.json"
sd_log "promoted.json: $REL/promoted.json (mode=$MODE from=${OLD:-<none>})"

sd_log "promoted $SHA12 (mode=$MODE). backup: $BACKUP"
sd_log "the working checkout was NOT touched. If main is behind this release, a human runs: git -C $SD_REPO merge --ff-only $SHA12"
