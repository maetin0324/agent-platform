#!/usr/bin/env bash
# scripts/selfdeploy/verify.sh [--dry-run] <sha12> — ADR-0040 D3 の「検証（staging）」段。
#
#   本番 DB の `sqlite3 .backup` スナップショットに対して、新リリースの taskd を **verify モード**で
#   127.0.0.1:7711 に起こし、
#     1. 起動と health.schema_version == 新バイナリの SCHEMA_VERSION
#     2. 本番 API（127.0.0.1:7710、読むだけ）との件数一致（tasks / projects / milestones / org /
#        approvals(pending=false) / reports / messages）と tasks の {id,status} 集合の一致
#     3. 主要 GET が 200 かつ JSON（inbox / org/<node>/memory / notify / clusters / providers / config）
#     4. 新リリースの GUI を 127.0.0.1:7701 に起こして主要ページが 200
#     5. N-1 互換: `current` の旧 taskd を、**新バイナリがマイグレーションした後の**同じスナップショットに
#        対して 127.0.0.1:7712 に起こし、1〜3 と同じ検査（落ちたら live_ok = false）
#   を行い、`~/taskd/releases/<sha12>/verify.json` を書く。`ok` は 1〜4 が全部真のとき。
#
# 本番には触れない: DB は `.backup` と `mode=ro` で読むだけ、本番 API は GET だけ、
# 本番のプロセスには一切シグナルを送らない。127.0.0.1:7710 / 0.0.0.0:7700 には bind しない。
set -euo pipefail

SD_PROG=verify
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: verify.sh [--dry-run] <sha12>

  --dry-run   何も起こさずに、前提（スナップショットが取れる / ポート 7711・7701・7712 が空いている /
              current の状態）だけ確かめ、実行するはずのコマンドを表示して終わる。
              Phase 47（--mode / --db / --listen …）が本番の `current` に入る前でも使える。
EOF
  exit 2
}

DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    -h | --help) usage ;;
    -*) usage ;;
    *) break ;;
  esac
done
[ $# -eq 1 ] || usage
SHA12="$1"

sd_require_json_tool
command -v sqlite3 >/dev/null 2>&1 || sd_die "sqlite3 not found"
command -v curl >/dev/null 2>&1 || sd_die "curl not found"
[ "$SD_JSON_TOOL" = python3 ] || sd_die "verify.sh needs python3 (the API collectors are written in it)"

REL="$(sd_release_dir "$SHA12")"
[ -d "$REL" ] || sd_die "no such release: $REL (run release.sh first)"
[ -x "$REL/bin/taskd" ] || sd_die "missing $REL/bin/taskd"
[ -f "$REL/gui/server.js" ] || sd_die "missing $REL/gui/server.js"
[ -f "$SD_CONFIG" ] || sd_die "missing $SD_CONFIG"
[ -r "$SD_DB" ] || sd_die "cannot read $SD_DB"
[ "$(sd_json_get "$REL/gate.json" ok 2>/dev/null || echo false)" = true ] \
  || sd_die "gate.json of $SHA12 is not ok; refusing to verify"

NEW_SCHEMA="$(sd_json_get "$REL/manifest.json" schema_version)" \
  || sd_die "manifest.json has no schema_version"
CUR="$(sd_current_sha)"

# ---- 起こしたプロセスだけを止める後始末 ------------------------------------

SD_PIDS=()
sd_track() { SD_PIDS+=("$1"); }
sd_cleanup() {
  local pid waited
  for pid in ${SD_PIDS[@]+"${SD_PIDS[@]}"}; do
    kill -0 "$pid" 2>/dev/null || continue
    sd_log "cleanup: SIGTERM $pid"
    kill -TERM "$pid" 2>/dev/null || true
  done
  for pid in ${SD_PIDS[@]+"${SD_PIDS[@]}"}; do
    waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 15 ]; do
      sleep 1
      waited=$((waited + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
      sd_log "cleanup: SIGKILL $pid"
      kill -KILL "$pid" 2>/dev/null || true
    fi
  done
}
trap sd_cleanup EXIT INT TERM

# ---- staging を作り直してスナップショットを取る ----------------------------

sd_require_port_free "$SD_STAGING_API_PORT" "staging taskd"
sd_require_port_free "$SD_STAGING_GUI_PORT" "staging gui"
sd_require_port_free "$SD_STAGING_OLD_API_PORT" "staging N-1 taskd"

rm -rf "$SD_STAGING"
mkdir -p "$SD_STAGING/workspaces" "$SD_STAGING/logs"
SNAP="$SD_STAGING/staging.sqlite3"
ST_TOKEN="$SD_STAGING/api.token"

sd_log "snapshot: sqlite3 file://$SD_DB?mode=ro \".backup $SNAP\""
sqlite3 "file:$SD_DB?mode=ro" ".backup '$SNAP'" \
  || sd_die "sqlite3 .backup failed (production DB is only read here)"
SNAP_SCHEMA="$(sqlite3 "file:$SNAP?mode=ro" 'SELECT COALESCE(MAX(version), 0) FROM schema_migrations;')"
SNAP_TASKS="$(sqlite3 "file:$SNAP?mode=ro" 'SELECT COUNT(*) FROM tasks;')"
sd_log "snapshot ok: schema_version=$SNAP_SCHEMA tasks=$SNAP_TASKS ($(du -h "$SNAP" | cut -f1))"

head -c 32 /dev/urandom | base64 | tr -d '\n' >"$ST_TOKEN"
chmod 600 "$ST_TOKEN"

NEW_CMD=("$REL/bin/taskd" --config "$SD_CONFIG" --mode verify --db "$SNAP"
  --listen "127.0.0.1:$SD_STAGING_API_PORT" --workspace-root "$SD_STAGING/workspaces"
  --token-file "$ST_TOKEN" --release "$SHA12")
GUI_ENV=(TASKD_API_URL="http://127.0.0.1:$SD_STAGING_API_PORT"
  TASKD_API_TOKEN_FILE="$ST_TOKEN"
  TASKD_GUI_BIND="127.0.0.1:$SD_STAGING_GUI_PORT"
  TASKD_GUI_RELEASE="$SHA12"
  NODE_ENV=production)
OLD_CMD=()
if [ -n "$CUR" ] && [ -x "$SD_CURRENT/bin/taskd" ]; then
  OLD_CMD=("$SD_CURRENT/bin/taskd" --config "$SD_CONFIG" --mode verify --db "$SNAP"
    --listen "127.0.0.1:$SD_STAGING_OLD_API_PORT" --workspace-root "$SD_STAGING/workspaces-n1"
    --token-file "$ST_TOKEN" --release "$CUR")
fi

if [ "$DRY_RUN" = true ]; then
  cat >&2 <<EOF

--- dry run: 前提 ---
release           : $REL (schema_version=$NEW_SCHEMA)
snapshot          : $SNAP (schema_version=$SNAP_SCHEMA, tasks=$SNAP_TASKS)
staging token     : $ST_TOKEN
ports free        : $SD_STAGING_API_PORT (taskd) / $SD_STAGING_GUI_PORT (gui) / $SD_STAGING_OLD_API_PORT (N-1)
current           : ${CUR:-<none>}  -> live_ok は $( [ -n "$CUR" ] && echo "N-1 の結果しだい" || echo "false（current が無い）" )
production API    : $SD_PROD_API (読むだけ。token $SD_API_TOKEN_FILE)

--- dry run: 実行するはずのコマンド ---
[1/5] ${NEW_CMD[*]}
[3/5] curl http://127.0.0.1:$SD_STAGING_API_PORT/api/v1/{health,tasks,projects,org,approvals,reports,inbox,notify,clusters,providers,config}
[4/5] ( cd $REL/gui && ${GUI_ENV[*]} /usr/bin/node server.js )
      curl http://127.0.0.1:$SD_STAGING_GUI_PORT/{healthz,,org,projects,projects/<id>,approvals,reports,clusters}
[5/5] $( [ ${#OLD_CMD[@]} -gt 0 ] && echo "${OLD_CMD[*]}" || echo "（current が無いので N-1 検査は行わない。live_ok=false）" )

何も起こさずに終わる（verify.json は書かない）。
EOF
  sd_log "dry run ok"
  exit 0
fi

# ---- 検査の記録 ------------------------------------------------------------

CHECKS_TSV="$(mktemp)"
printf 'id:i name:s ok:b detail:s\n' >"$CHECKS_TSV"
record() { # record <id> <name> <ok:true|false> <detail>
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$(printf '%s' "$4" | tr '\t\n' '  ')" >>"$CHECKS_TSV"
  sd_log "check $1 ($2): $3 — $4"
}

# ---- 件数を集める python3 --------------------------------------------------

# `collect <base-url> <token-file>` → JSON（件数と tasks の {id,status} のダイジェスト）
collect() {
  python3 - "$1" "$2" <<'PY'
import hashlib, json, sys, urllib.error, urllib.parse, urllib.request

base = sys.argv[1].rstrip("/")
token = open(sys.argv[2], encoding="utf-8").read().strip() if len(sys.argv) > 2 and sys.argv[2] else ""
errors = []


def get(path):
    req = urllib.request.Request(base + path)
    if token:
        req.add_header("Authorization", "Bearer " + token)
    with urllib.request.urlopen(req, timeout=30) as resp:  # loopback only
        return json.loads(resp.read().decode("utf-8"))


def safe(path, default=None):
    try:
        return get(path)
    except Exception as exc:  # noqa: BLE001
        errors.append(f"{path}: {exc}")
        return default


out = {}

tasks = []
cursor = None
while True:
    q = "/api/v1/tasks?limit=500" + (f"&cursor={urllib.parse.quote(str(cursor), safe='')}" if cursor else "")
    page = safe(q, {"items": [], "next_cursor": None, "total": 0})
    tasks.extend(page.get("items") or [])
    cursor = page.get("next_cursor")
    if not cursor:
        out["tasks_total"] = page.get("total", len(tasks))
        break
out["tasks"] = len(tasks)
pairs = sorted(f"{t.get('id')}:{t.get('status')}" for t in tasks)
out["tasks_digest"] = hashlib.sha256("\n".join(pairs).encode("utf-8")).hexdigest()[:16]

projects = (safe("/api/v1/projects", {"items": []}) or {}).get("items") or []
out["projects"] = len(projects)
out["latest_project"] = projects[0].get("id") if projects else ""

milestones = 0
for p in projects:
    detail = safe(f"/api/v1/projects/{urllib.parse.quote(str(p.get('id')), safe='')}", {}) or {}
    milestones += len(detail.get("milestones") or [])
out["milestones"] = milestones

org = (safe("/api/v1/org", {"items": []}) or {}).get("items") or []
out["org"] = len(org)
out["first_org"] = org[0].get("id") if org else ""

out["approvals_decided"] = len((safe("/api/v1/approvals?pending=false", {"items": []}) or {}).get("items") or [])
out["reports"] = len((safe("/api/v1/reports?limit=500", {"items": []}) or {}).get("items") or [])

messages = 0
for n in org:
    page = safe(f"/api/v1/org/{urllib.parse.quote(str(n.get('id')), safe='')}/messages?limit=5000", {"items": []}) or {}
    messages += len(page.get("items") or [])
out["messages"] = messages

out["errors"] = errors
json.dump(out, sys.stdout, ensure_ascii=False)
PY
}

COUNT_KEYS="tasks tasks_total projects milestones org approvals_decided reports messages tasks_digest"

# ---- 1. 新リリースを verify モードで起こす ---------------------------------

NEW_LOG="$SD_STAGING/logs/taskd-new.log"
sd_log "starting: ${NEW_CMD[*]}"
"${NEW_CMD[@]}" >"$NEW_LOG" 2>&1 &
NEW_PID=$!
sd_track "$NEW_PID"

NEW_BASE="http://127.0.0.1:$SD_STAGING_API_PORT"
OK1=false
if sd_wait_http_200 "$NEW_BASE/api/v1/health" 60; then
  HEALTH="$SD_STAGING/logs/health-new.json"
  sd_http_get "$NEW_BASE/api/v1/health" >"$HEALTH" || true
  GOT_SCHEMA="$(sd_json_get "$HEALTH" schema_version || echo "?")"
  GOT_MODE="$(sd_json_get "$HEALTH" mode 2>/dev/null || echo "-")"
  GOT_RELEASE="$(sd_json_get "$HEALTH" release 2>/dev/null || echo "-")"
  if [ "$GOT_SCHEMA" = "$NEW_SCHEMA" ]; then
    OK1=true
    record 1 start-and-migrate true "health 200, schema_version=$GOT_SCHEMA, mode=$GOT_MODE, release=$GOT_RELEASE"
  else
    record 1 start-and-migrate false "health.schema_version=$GOT_SCHEMA but the release manifest says $NEW_SCHEMA"
  fi
else
  record 1 start-and-migrate false "no 200 from $NEW_BASE/api/v1/health within 60s; see $NEW_LOG ($(tail -n 3 "$NEW_LOG" | tr '\n' ' '))"
fi

# ---- 2. 件数一致（本番 API は読むだけ） ------------------------------------

OK2=false
PROD_JSON="$SD_STAGING/logs/counts-prod.json"
STG_JSON="$SD_STAGING/logs/counts-staging.json"
PROD_AFTER_JSON="$SD_STAGING/logs/counts-prod-after.json"
COUNT_DIFF=""
if [ "$OK1" = true ]; then
  collect "$SD_PROD_API" "$SD_API_TOKEN_FILE" >"$PROD_JSON" || true
  collect "$NEW_BASE" "$ST_TOKEN" >"$STG_JSON" || true
  if sd_json_valid "$PROD_JSON" && sd_json_valid "$STG_JSON"; then
    OK2=true
    for key in $COUNT_KEYS; do
      a="$(sd_json_get "$PROD_JSON" "$key" || echo "?")"
      b="$(sd_json_get "$STG_JSON" "$key" || echo "??")"
      if [ "$a" != "$b" ]; then
        OK2=false
        COUNT_DIFF="$COUNT_DIFF $key(prod=$a staging=$b)"
      fi
    done
    perr="$(sd_json_get "$PROD_JSON" errors || echo '[]')"
    serr="$(sd_json_get "$STG_JSON" errors || echo '[]')"
    if [ "$perr" != "[]" ] || [ "$serr" != "[]" ]; then
      OK2=false
      COUNT_DIFF="$COUNT_DIFF errors(prod=$perr staging=$serr)"
    fi
    # 本番は動き続けているので、検査の後にもう一度数えてドリフトが見えるようにする。
    collect "$SD_PROD_API" "$SD_API_TOKEN_FILE" >"$PROD_AFTER_JSON" || true
    if [ "$OK2" = true ]; then
      record 2 counts-match true "all of: $COUNT_KEYS"
    else
      drift=""
      if sd_json_valid "$PROD_AFTER_JSON" \
        && [ "$(sd_json_get "$PROD_JSON" tasks_digest || echo x)" != "$(sd_json_get "$PROD_AFTER_JSON" tasks_digest || echo y)" ]; then
        drift=" (production changed during verify: tasks_digest moved — rerun when it is quiet)"
      fi
      record 2 counts-match false "mismatch:$COUNT_DIFF$drift"
    fi
  else
    record 2 counts-match false "could not collect counts (see $PROD_JSON / $STG_JSON)"
  fi
else
  record 2 counts-match false "skipped (check 1 failed)"
fi

# ---- 3. 主要 GET ------------------------------------------------------------

FIRST_ORG="$(sd_json_get "$STG_JSON" first_org 2>/dev/null || echo secretary)"
[ -n "$FIRST_ORG" ] || FIRST_ORG=secretary
LATEST_PROJECT="$(sd_json_get "$STG_JSON" latest_project 2>/dev/null || echo "")"

# `probe_api <base> <token-file>` → 200 かつ JSON でないパスを空白区切りで返す
probe_api() {
  local base="$1" token="$2" path bad="" body tmp code
  tmp="$(mktemp)"
  for path in "/api/v1/inbox" "/api/v1/org/$FIRST_ORG/memory" "/api/v1/notify" "/api/v1/clusters" \
    "/api/v1/providers" "/api/v1/config"; do
    code="$(sd_http_status "$base$path" "$token")"
    if [ "$code" != 200 ]; then
      bad="$bad $path($code)"
      continue
    fi
    body="$(sd_http_get "$base$path" "$token" || true)"
    printf '%s' "$body" >"$tmp"
    sd_json_valid "$tmp" || bad="$bad $path(not-json)"
  done
  rm -f "$tmp"
  printf '%s' "$bad"
}

OK3=false
if [ "$OK1" = true ]; then
  BAD3="$(probe_api "$NEW_BASE" "$ST_TOKEN")"
  if [ -z "$BAD3" ]; then
    OK3=true
    record 3 main-gets true "inbox, org/$FIRST_ORG/memory, notify, clusters, providers, config all 200 + JSON"
  else
    record 3 main-gets false "failing:$BAD3"
  fi
else
  record 3 main-gets false "skipped (check 1 failed)"
fi

# ---- 4. GUI -----------------------------------------------------------------

OK4=false
GUI_LOG="$SD_STAGING/logs/gui.log"
if [ "$OK1" = true ]; then
  sd_log "starting staging gui: ( cd $REL/gui && node server.js ) on 127.0.0.1:$SD_STAGING_GUI_PORT"
  ( cd "$REL/gui" && env "${GUI_ENV[@]}" /usr/bin/node server.js ) >"$GUI_LOG" 2>&1 &
  GUI_PID=$!
  sd_track "$GUI_PID"
  GUI_BASE="http://127.0.0.1:$SD_STAGING_GUI_PORT"
  if sd_wait_http_200 "$GUI_BASE/healthz" 60; then
    sd_http_get "$GUI_BASE/healthz" >"$SD_STAGING/logs/healthz.json" || true
    GUI_RELEASE="$(sd_json_get "$SD_STAGING/logs/healthz.json" release || echo "-")"
    BAD4=""
    PAGES=("/" "/org" "/projects" "/approvals" "/reports" "/clusters")
    [ -n "$LATEST_PROJECT" ] && PAGES+=("/projects/$LATEST_PROJECT")
    # `/` は `/tasks` へ 302 するので、リダイレクトを追った先の status を見る。
    for page in "${PAGES[@]}"; do
      code="$(sd_http_status_follow "$GUI_BASE$page")"
      [ "$code" = 200 ] || BAD4="$BAD4 $page($code)"
    done
    if [ -z "$BAD4" ] && [ "$GUI_RELEASE" = "$SHA12" ]; then
      OK4=true
      record 4 gui true "healthz release=$GUI_RELEASE, pages 200: ${PAGES[*]}"
    elif [ -z "$BAD4" ]; then
      record 4 gui false "pages are 200 but /healthz reports release=$GUI_RELEASE (want $SHA12)"
    else
      record 4 gui false "failing:$BAD4 (healthz release=$GUI_RELEASE); see $GUI_LOG"
    fi
  else
    record 4 gui false "no 200 from $GUI_BASE/healthz within 60s; see $GUI_LOG ($(tail -n 3 "$GUI_LOG" | tr '\n' ' '))"
  fi
else
  record 4 gui false "skipped (check 1 failed)"
fi

# ---- 5. N-1 互換（live_ok） -------------------------------------------------

LIVE_OK=false
OLD_LOG="$SD_STAGING/logs/taskd-old.log"
if [ ${#OLD_CMD[@]} -eq 0 ]; then
  record 5 n-1-compat false "no \`current\` release (first migration): live_ok = false"
elif [ "$OK1" != true ]; then
  record 5 n-1-compat false "skipped (check 1 failed): live_ok = false"
else
  mkdir -p "$SD_STAGING/workspaces-n1"
  sd_log "starting N-1: ${OLD_CMD[*]}"
  "${OLD_CMD[@]}" >"$OLD_LOG" 2>&1 &
  OLD_PID=$!
  sd_track "$OLD_PID"
  OLD_BASE="http://127.0.0.1:$SD_STAGING_OLD_API_PORT"
  if sd_wait_http_200 "$OLD_BASE/api/v1/health" 60; then
    OLD_HEALTH="$SD_STAGING/logs/health-old.json"
    sd_http_get "$OLD_BASE/api/v1/health" >"$OLD_HEALTH" || true
    OLD_SCHEMA="$(sd_json_get "$OLD_HEALTH" schema_version || echo "?")"
    OLD_COUNTS="$SD_STAGING/logs/counts-n1.json"
    collect "$OLD_BASE" "$ST_TOKEN" >"$OLD_COUNTS" || true
    BAD5="$(probe_api "$OLD_BASE" "$ST_TOKEN")"
    DIFF5=""
    if sd_json_valid "$OLD_COUNTS" && sd_json_valid "$STG_JSON"; then
      for key in $COUNT_KEYS; do
        a="$(sd_json_get "$STG_JSON" "$key" || echo "?")"
        b="$(sd_json_get "$OLD_COUNTS" "$key" || echo "??")"
        [ "$a" = "$b" ] || DIFF5="$DIFF5 $key(new=$a old=$b)"
      done
    else
      DIFF5=" could-not-collect"
    fi
    if [ "$OLD_SCHEMA" = "$NEW_SCHEMA" ] && [ -z "$BAD5" ] && [ -z "$DIFF5" ]; then
      LIVE_OK=true
      record 5 n-1-compat true "old taskd ($CUR) reads the migrated snapshot: schema_version=$OLD_SCHEMA, same counts, main GETs 200"
    else
      record 5 n-1-compat false "old taskd ($CUR) is not compatible: schema=$OLD_SCHEMA(want $NEW_SCHEMA) gets:$BAD5 counts:$DIFF5"
    fi
  else
    record 5 n-1-compat false "old taskd ($CUR) did not become healthy on $OLD_BASE within 60s (SchemaTooNew?); see $OLD_LOG ($(tail -n 3 "$OLD_LOG" | tr '\n' ' '))"
  fi
fi

# ---- verify.json ------------------------------------------------------------

OK=false
if [ "$OK1" = true ] && [ "$OK2" = true ] && [ "$OK3" = true ] && [ "$OK4" = true ]; then OK=true; fi

CHECKS_JSON="$(sd_tsv_to_json "$CHECKS_TSV")"
counts_or_null() { if sd_json_valid "$1"; then cat "$1"; else printf 'null'; fi; }

{
  printf '{\n'
  printf '  "sha12": %s,\n' "$(sd_json_str "$SHA12")"
  printf '  "at": %s,\n' "$(sd_json_str "$(sd_ts)")"
  printf '  "ok": %s,\n' "$OK"
  printf '  "live_ok": %s,\n' "$LIVE_OK"
  printf '  "schema_version": %s,\n' "$NEW_SCHEMA"
  printf '  "snapshot_schema_version": %s,\n' "$SNAP_SCHEMA"
  printf '  "current": %s,\n' "$(sd_json_str "$CUR")"
  printf '  "staging_dir": %s,\n' "$(sd_json_str "$SD_STAGING")"
  printf '  "checks": %s,\n' "$CHECKS_JSON"
  printf '  "counts": {\n'
  printf '    "prod": %s,\n' "$(counts_or_null "$PROD_JSON")"
  printf '    "staging": %s,\n' "$(counts_or_null "$STG_JSON")"
  printf '    "prod_after": %s\n' "$(counts_or_null "$PROD_AFTER_JSON")"
  printf '  }\n'
  printf '}\n'
} >"$REL/verify.json"
rm -f "$CHECKS_TSV"

sd_log "verify.json: $REL/verify.json (ok=$OK live_ok=$LIVE_OK)"
[ "$OK" = true ] || exit 1
