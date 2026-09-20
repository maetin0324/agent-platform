#!/usr/bin/env bash
# scripts/selfdeploy/rollback.sh [--restore-db] — ADR-0040 D2 の「戻す」。**人だけが実行する**（D5）。
#
#   `previous` を昇格し直す。旧バイナリの SCHEMA_VERSION が DB の版数より低い（= `SchemaTooNew` で
#   起動できない）ときは拒否し、`--restore-db` を付けたときだけ `backups/` の直近の昇格前コピーを
#   DB に書き戻して停止 → 起動する。**書き戻しは昇格以降の仕事を失う**。人が選ぶ。
set -euo pipefail

SD_PROG=rollback
# shellcheck source=lib.sh
SD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SD_DIR/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: rollback.sh [--restore-db]

  引数なし  : previous を昇格し直す（promote.sh previous と同じ。スキーマが進んでいたら拒否）
  --restore-db: DB を backups/ の直近の `*-pre-*.sqlite3` に書き戻してから停止 → 起動で戻す。
                昇格の後に進んだ仕事は失われる。
EOF
  exit 2
}

RESTORE_DB=false
while [ $# -gt 0 ]; do
  case "$1" in
    --restore-db) RESTORE_DB=true; shift ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
done

sd_require_json_tool
TS="$(sd_stamp)"
mkdir -p "$SD_BACKUPS"
SD_LOG_FILE="$SD_BACKUPS/promote-$TS.log"

PREV="$(sd_previous_sha)"
CUR="$(sd_current_sha)"
[ -n "$PREV" ] || sd_die "no \`previous\` release to roll back to ($SD_PREVIOUS)"
PREV_DIR="$(sd_release_dir "$PREV")"
[ -x "$PREV_DIR/bin/celeris" ] || sd_die "missing $PREV_DIR/bin/celeris"
PREV_SCHEMA="$(sd_json_get "$PREV_DIR/manifest.json" schema_version)" \
  || sd_die "$PREV_DIR/manifest.json has no schema_version"

# DB の版数: 動いている celeris の health を優先し、駄目なら sqlite3 で直接読む（read-only）。
DB_SCHEMA=""
if [ "$(sd_http_status "$SD_PROD_API/api/v1/health")" = 200 ]; then
  tmp="$(mktemp)"
  sd_http_get "$SD_PROD_API/api/v1/health" >"$tmp" || true
  DB_SCHEMA="$(sd_json_get "$tmp" schema_version 2>/dev/null || echo "")"
  rm -f "$tmp"
fi
if [ -z "$DB_SCHEMA" ]; then
  DB_SCHEMA="$(sd_db_schema_version)" || sd_die "cannot read the schema version of $SD_DB"
fi

sd_log "rollback: current=${CUR:-<none>} -> previous=$PREV (previous SCHEMA_VERSION=$PREV_SCHEMA, DB schema_version=$DB_SCHEMA)"

if [ "$PREV_SCHEMA" -ge "$DB_SCHEMA" ] && [ "$RESTORE_DB" != true ]; then
  sd_log "the previous binary can read the current DB; promoting $PREV"
  exec "$SD_DIR/promote.sh" "$PREV"
fi

if [ "$RESTORE_DB" != true ]; then
  sd_die "the previous binary knows schema $PREV_SCHEMA but the DB is at $DB_SCHEMA (SchemaTooNew). Re-run with --restore-db to write back the newest backups/*-pre-*.sqlite3 (work done since the promotion will be lost)."
fi

# ---- --restore-db: 停止 → 書き戻し → 起動 ---------------------------------

SRC=""
for f in $(ls -1t "$SD_BACKUPS"/*-pre-*.sqlite3 2>/dev/null || true); do
  case "$(basename "$f")" in *-pre-rollback.sqlite3) continue ;; esac
  SRC="$f"
  break
done
[ -n "$SRC" ] || sd_die "no $SD_BACKUPS/*-pre-*.sqlite3 to restore from"
sd_log "will restore the DB from $SRC ($(du -h "$SRC" | cut -f1))"

PRE_ROLLBACK="$SD_BACKUPS/$TS-pre-rollback.sqlite3"
sd_log "backing up the current DB to $PRE_ROLLBACK"
sqlite3 "file:$SD_DB?mode=ro" ".backup '$PRE_ROLLBACK'" || sd_die "sqlite3 .backup failed"

stop_unit_or_pid() {
  local sha="$1" pid argv i matched skip
  if [ -n "$sha" ] && systemctl --user is-active --quiet "celeris@$sha"; then
    sd_log "systemctl --user stop celeris@$sha"
    systemctl --user stop "celeris@$sha" || sd_die "failed to stop celeris@$sha"
    return 0
  fi
  for pid in /proc/[0-9]*; do
    pid="${pid#/proc/}"
    if [ "$pid" = "$$" ]; then continue; fi
    if [ ! -r "/proc/$pid/cmdline" ]; then continue; fi
    argv=()
    mapfile -d '' -t argv <"/proc/$pid/cmdline" 2>/dev/null || continue
    if [ "${#argv[@]}" -lt 3 ]; then continue; fi
    if [ "$(basename -- "${argv[0]}")" != celeris ]; then continue; fi
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
    sd_log "SIGTERM celeris pid=$pid"
    kill -TERM "$pid" || true
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 30 ]; do
      sleep 1
      waited=$((waited + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then sd_die "celeris pid $pid did not exit"; fi
    return 0
  done
  sd_log "no running celeris found"
}

stop_unit_or_pid "$CUR"
if [ -n "$CUR" ] && systemctl --user is-active --quiet "celeris-gui@$CUR"; then
  sd_log "systemctl --user stop celeris-gui@$CUR"
  systemctl --user stop "celeris-gui@$CUR" || sd_log "warning: stop celeris-gui@$CUR failed"
fi

sd_log "restoring $SRC over $SD_DB (and dropping -wal / -shm)"
cp -p "$SRC" "$SD_DB"
rm -f "$SD_DB-wal" "$SD_DB-shm"

sd_log "systemctl --user start celeris@$PREV"
systemctl --user start "celeris@$PREV" || sd_die "failed to start celeris@$PREV (the DB was restored from $SRC)"
if ! sd_wait_http_200 "$SD_PROD_API/api/v1/health" 60; then
  sd_die "celeris@$PREV did not become healthy within 60s (the DB was restored from $SRC; $PRE_ROLLBACK holds the DB as it was before this rollback)"
fi
tmp="$(mktemp)"
sd_http_get "$SD_PROD_API/api/v1/health" >"$tmp" || true
GOT="$(sd_json_get "$tmp" schema_version || echo '?')"
rm -f "$tmp"
sd_log "celeris@$PREV is healthy: schema_version=$GOT (previous SCHEMA_VERSION=$PREV_SCHEMA)"

systemctl --user enable "celeris@$PREV" || sd_log "warning: enable celeris@$PREV failed"
if [ -n "$CUR" ]; then
  systemctl --user disable "celeris@$CUR" || sd_log "warning: disable celeris@$CUR failed"
fi
systemctl --user start "celeris-gui@$PREV" || sd_die "celeris is back on $PREV but celeris-gui@$PREV did not start"
sd_wait_http_200 "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz" 60 \
  || sd_log "warning: the GUI did not answer /healthz within 60s"
systemctl --user enable "celeris-gui@$PREV" || sd_log "warning: enable celeris-gui@$PREV failed"
if [ -n "$CUR" ]; then
  systemctl --user disable "celeris-gui@$CUR" || sd_log "warning: disable celeris-gui@$CUR failed"
fi

if [ -n "$CUR" ] && [ -d "$(sd_release_dir "$CUR")" ]; then
  sd_set_link "$SD_PREVIOUS" "$CUR"
fi
sd_set_link "$SD_CURRENT" "$PREV"
sd_log "rolled back to $PREV with a restored DB. Lost: everything after $SRC was taken."
