#!/usr/bin/env bash
# scripts/selfdeploy/verify.sh [--dry-run] <sha12> — ADR-0040 D3 の「検証（staging）」段。
#
#   本番 DB の `sqlite3 .backup` スナップショットに対して、新リリースの taskd を **verify モード**で
#   127.0.0.1:7711 に起こし、
#     1. 起動と health.schema_version == 新バイナリの SCHEMA_VERSION
#     2. **件数一致（ADR-0041 D2）**: 同じスナップショットの**マイグレーション前**（`.backup` 直後に
#        `sqlite3` で数えた生の行数）と**マイグレーション後**（staging API）を比べる。対象は
#        tasks / projects / milestones / org_nodes / approvals / reports / messages の件数と
#        tasks の {id,status} のダイジェスト。**本番 API は読まない**（本番は検証の間も動いているので、
#        本番と比べると当たり前にずれて偽陰性になる。ADR-0041 §1-2）
#     3. 主要 GET が 200 かつ JSON（inbox / org/<node>/memory / notify / clusters / providers / config）
#     4. 新リリースの GUI を 127.0.0.1:7701 に起こして主要ページが 200
#     5. N-1 互換: `current` の旧 taskd を、**新バイナリがマイグレーションした後の**同じスナップショットに
#        対して 127.0.0.1:7712 に起こし、1〜3 と同じ検査（件数はスナップショットと比べる。
#        落ちたら live_ok = false）
#     6. **煙試験（ADR-0041 D5）**: staging に `POST /tasks {genre: "smoke", role: "smoke"}` して承認し、
#        60 秒以内に `done` になること、`worker_started` / `worker_finished(done…)` の event があること、
#        （`assignee` を付けられたときは）その run の報告が `GET /reports` に出ることを確かめる。
#        verify の taskd は `genre = "smoke"` だけを dispatch し、役割・分野・プロバイダはすべて
#        **偽のアダプタ**の組み込み（LLM は呼ばない）。**検査 5 の後**に行う（検査 2 / 5 の件数を動かさない）
#   を行い、`~/taskd/releases/<sha12>/verify.json` を書く。`ok` は 1〜4 と 6 が全部真のとき。
#
#   **直列化（ADR-0041 D2）**: `$SD_STAGING/.lock` を `flock` で取る（待ちの上限
#   `SD_VERIFY_LOCK_WAIT`、既定 1800 秒）。取れなければ exit 75（EX_TEMPFAIL）。
#   staging のディレクトリもポート（7711 / 7701 / 7712）も固定なので、2 本同時には走れない。
#
# 本番には触れない: DB は `.backup` と `mode=ro` で読むだけ、**本番 API は一切叩かない**、
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

env:
  SD_VERIFY_LOCK_WAIT  他の verify.sh を待つ上限（秒。既定 1800）。超えたら exit 75

exit:
  0   verify.json.ok == true
  1   検査に落ちた / 前提が揃わない
  75  他の verify.sh が走っていて、待ち時間の上限までに終わらなかった（EX_TEMPFAIL）
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

# ---- スナップショットを数える python3（ADR-0041 D2 の検査 2 の基準側）-------
#
# `.backup` の直後（＝**マイグレーション前**）に、コピーした SQLite を `mode=ro` で開いて生の行数を
# 数える。出す形は下の `collect`（staging API 側）と**同じキー**で、`tasks_digest` は
# **同じ式**（`sorted("<id>:<status>")` を "\n" で連ね、sha256 の先頭 16 桁）で計算する。
# DB の `tasks.status` は API の JSON と同じ snake_case の文字列なので、両側が一致する。
collect_snapshot() {
  python3 - "$1" <<'PY'
import hashlib, json, sqlite3, sys

conn = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
errors = []


def scalar(sql):
    try:
        return int(conn.execute(sql).fetchone()[0])
    except Exception as exc:  # noqa: BLE001
        errors.append(f"{sql}: {exc}")
        return -1


out = {}
out["tasks"] = scalar("SELECT COUNT(*) FROM tasks")
out["tasks_total"] = out["tasks"]
try:
    pairs = sorted(f"{row[0]}:{row[1]}" for row in conn.execute("SELECT id, status FROM tasks"))
    out["tasks_digest"] = hashlib.sha256("\n".join(pairs).encode("utf-8")).hexdigest()[:16]
except Exception as exc:  # noqa: BLE001
    errors.append(f"tasks_digest: {exc}")
    out["tasks_digest"] = ""
out["projects"] = scalar("SELECT COUNT(*) FROM projects")
out["milestones"] = scalar("SELECT COUNT(*) FROM milestones")
out["org"] = scalar("SELECT COUNT(*) FROM org_nodes")
out["approvals"] = scalar("SELECT COUNT(*) FROM approvals")
out["approvals_decided"] = scalar("SELECT COUNT(*) FROM approvals WHERE decision IS NOT NULL")
out["reports"] = scalar("SELECT COUNT(*) FROM reports")
out["messages"] = scalar("SELECT COUNT(*) FROM messages")
# API 側は「組織のノードごと」に数える（reports / messages には全件を返す入口が無い）。
# ノードが消えた後に残っている行はそこから漏れるので、数が合わなかったときの説明に使う。
out["orphan_reports"] = scalar("SELECT COUNT(*) FROM reports WHERE node_id NOT IN (SELECT id FROM org_nodes)")
out["orphan_messages"] = scalar("SELECT COUNT(*) FROM messages WHERE node_id NOT IN (SELECT id FROM org_nodes)")
out["schema_version"] = scalar("SELECT COALESCE(MAX(version), 0) FROM schema_migrations")
out["errors"] = errors
conn.close()
json.dump(out, sys.stdout, ensure_ascii=False)
PY
}

# ---- 直列化（ADR-0041 D2）--------------------------------------------------
#
# ポートの検査より**先に**ロックを取る。先に検査すると、もう 1 本が走っている間は「ポートが塞がって
# いる」で exit 1 になってしまい、「混んでいる（75）」と区別できない。ロックは `$SD_STAGING/.lock`
# （staging を作り直すときもこのファイルだけは消さない。消すと inode が変わって排他が効かなくなる）。
mkdir -p "$SD_STAGING"
sd_lock_or_tempfail 9 "$SD_STAGING/.lock" "$SD_VERIFY_LOCK_WAIT" "verify.sh"
sd_log "staging lock acquired: $SD_STAGING/.lock"

# ---- staging を作り直してスナップショットを取る ----------------------------

sd_require_port_free "$SD_STAGING_API_PORT" "staging taskd"
sd_require_port_free "$SD_STAGING_GUI_PORT" "staging gui"
sd_require_port_free "$SD_STAGING_OLD_API_PORT" "staging N-1 taskd"

# `.lock` 以外を消す（`rm -rf "$SD_STAGING"` だとロックしている実体ごと消えてしまう）。
find "$SD_STAGING" -mindepth 1 -maxdepth 1 ! -name '.lock' -exec rm -rf {} +
mkdir -p "$SD_STAGING/workspaces" "$SD_STAGING/logs"
SNAP="$SD_STAGING/staging.sqlite3"
ST_TOKEN="$SD_STAGING/api.token"

sd_log "snapshot: sqlite3 file://$SD_DB?mode=ro \".backup $SNAP\""
sqlite3 "file:$SD_DB?mode=ro" ".backup '$SNAP'" \
  || sd_die "sqlite3 .backup failed (production DB is only read here)"
SNAP_SCHEMA="$(sqlite3 "file:$SNAP?mode=ro" 'SELECT COALESCE(MAX(version), 0) FROM schema_migrations;')"
SNAP_TASKS="$(sqlite3 "file:$SNAP?mode=ro" 'SELECT COUNT(*) FROM tasks;')"
sd_log "snapshot ok: schema_version=$SNAP_SCHEMA tasks=$SNAP_TASKS ($(du -h "$SNAP" | cut -f1))"

# **マイグレーションの前に**数える（ADR-0041 D2 の検査 2 の基準）。ここから先はこのファイルを
# 新バイナリが書き換えるので、ここでしか取れない。
SNAP_JSON="$SD_STAGING/logs/counts-snapshot.json"
collect_snapshot "$SNAP" >"$SNAP_JSON" || true
sd_json_valid "$SNAP_JSON" || sd_die "could not count the snapshot before migration (see $SNAP_JSON)"
sd_log "snapshot counts: tasks=$(sd_json_get "$SNAP_JSON" tasks) projects=$(sd_json_get "$SNAP_JSON" projects) org=$(sd_json_get "$SNAP_JSON" org) reports=$(sd_json_get "$SNAP_JSON" reports) messages=$(sd_json_get "$SNAP_JSON" messages) digest=$(sd_json_get "$SNAP_JSON" tasks_digest)"

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
snapshot counts   : $SNAP_JSON（マイグレーション前。検査 2 はこれと staging API を比べる）
staging lock      : $SD_STAGING/.lock（取得済み。待ちの上限 ${SD_VERIFY_LOCK_WAIT}s）
staging token     : $ST_TOKEN
ports free        : $SD_STAGING_API_PORT (taskd) / $SD_STAGING_GUI_PORT (gui) / $SD_STAGING_OLD_API_PORT (N-1)
current           : ${CUR:-<none>}  -> live_ok は $( [ -n "$CUR" ] && echo "N-1 の結果しだい" || echo "false（current が無い）" )
production API    : 叩かない（ADR-0041 D2。件数は同じスナップショットの前後で比べる）

--- dry run: 実行するはずのコマンド ---
[1/6] ${NEW_CMD[*]}
[3/6] curl http://127.0.0.1:$SD_STAGING_API_PORT/api/v1/{health,tasks,projects,org,approvals,reports,inbox,notify,clusters,providers,config}
[4/6] ( cd $REL/gui && ${GUI_ENV[*]} /usr/bin/node server.js )
      curl http://127.0.0.1:$SD_STAGING_GUI_PORT/{healthz,,org,projects,projects/<id>,approvals,reports,clusters}
[5/6] $( [ ${#OLD_CMD[@]} -gt 0 ] && echo "${OLD_CMD[*]}" || echo "（current が無いので N-1 検査は行わない。live_ok=false）" )
[6/6] curl -X POST http://127.0.0.1:$SD_STAGING_API_PORT/api/v1/tasks -d '{"title":"smoke","genre":"smoke","role":"smoke",...}'
      → approve → GET /tasks/<id> を 60 秒まで待って done → events → reports（ADR-0041 D5）

何も起こさずに終わる（verify.json は書かない）。
EOF
  sd_log "dry run ok"
  exit 0
fi

# ---- 検査の記録 ------------------------------------------------------------

CHECKS_TSV="$(mktemp)"
printf 'id:i name:s ok:b detail:s task_id:s elapsed_s:f\n' >"$CHECKS_TSV"
# record <id> <name> <ok:true|false> <detail> [task_id] [elapsed_s]
# `task_id` / `elapsed_s` は検査 6（煙試験。ADR-0041 D5）だけが埋める。他の検査では空 / 0。
record() {
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$(printf '%s' "$4" | tr '\t\n' '  ')" \
    "${5:-}" "${6:-0}" >>"$CHECKS_TSV"
  sd_log "check $1 ($2): $3 — $4"
}

# ---- 件数を集める python3 --------------------------------------------------

# `collect <base-url> <token-file>` → JSON（件数と tasks の {id,status} のダイジェスト）。
# 出す形は `collect_snapshot`（生の行数）と**同じキー**。API から「表の全行」を数えるために:
#   - tasks は `next_cursor` で最後まで辿る
#   - reports / messages は 1 回の応答の上限が 500 なので、**組織のノードごと**（messages は
#     さらに案件ごと + 案件なし）に分けて数えて足す。どれかが 500 に達したら `capped` に入れて
#     検査 2 を落とす（黙って少なく数えない）
#   - approvals は `pending` を書かなければ全件
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

out["approvals"] = len((safe("/api/v1/approvals", {"items": []}) or {}).get("items") or [])
out["approvals_decided"] = len((safe("/api/v1/approvals?pending=false", {"items": []}) or {}).get("items") or [])

# 1 応答の上限は 500（`GET /reports` / `GET /org/{id}/messages`）。表の全行を数えるために、
# 行が必ず持つ `node_id`（と messages は `project_id`）で分けて数える。
LIMIT = 500
capped = []


def counted(path, what):
    page = safe(path, {"items": []}) or {}
    n = len(page.get("items") or [])
    if n >= LIMIT:
        capped.append(f"{what} hit the {LIMIT}-row page limit")
    return n


reports = 0
messages = 0
for n in org:
    node = urllib.parse.quote(str(n.get("id")), safe="")
    reports += counted(f"/api/v1/reports?node={node}&limit={LIMIT}", f"reports of {node}")
    messages += counted(f"/api/v1/org/{node}/messages?limit={LIMIT}", f"messages of {node} (no project)")
    for p in projects:
        pid = urllib.parse.quote(str(p.get("id")), safe="")
        messages += counted(
            f"/api/v1/org/{node}/messages?project={pid}&limit={LIMIT}",
            f"messages of {node} in {pid}",
        )
out["reports"] = reports
out["messages"] = messages
out["capped"] = capped

out["errors"] = errors + capped
json.dump(out, sys.stdout, ensure_ascii=False)
PY
}

# ---- 煙試験（ADR-0041 D5 の検査 6）を回す python3 --------------------------
#
# `smoke <base-url> <token-file> [assignee]` → JSON `{ok, task_id, elapsed_s, detail, report}`。
# staging の API だけを叩く（本番には触れない）。中身は
#   POST /tasks（`genre = "smoke"`、受け入れ条件は `true` が exit 0 = 判定に LLM が要らない）
#   → POST /tasks/{id}/approve（draft → ready）
#   → GET /tasks/{id} を 60 秒まで 1 秒ごとに見て `status == "done"` を待つ
#   → GET /tasks/{id}/events に `worker_started` と `worker_finished`（outcome が `done…`）があるか
#   → `assignee` を付けられたときは GET /reports?node=<assignee> にそのタスクの報告が出るか
# の 5 段。`assignee` を付けるのは「終端での報告の生成（ADR-0034）」まで回帰に入れるため
# （報告は `assignee` のあるタスクにだけ作られる）。付けられなければその段だけ飛ばす。
smoke() {
  python3 - "$1" "$2" "${3:-}" <<'PY'
import json, sys, time, urllib.error, urllib.parse, urllib.request

base = sys.argv[1].rstrip("/")
token = open(sys.argv[2], encoding="utf-8").read().strip() if sys.argv[2] else ""
assignee = sys.argv[3] if len(sys.argv) > 3 else ""
TIMEOUT_S = 60

out = {"ok": False, "task_id": "", "elapsed_s": 0.0, "detail": "", "report": ""}


def call(path, body=None):
    req = urllib.request.Request(base + path, method="POST" if body is not None else "GET")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    data = None
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, data, timeout=30) as resp:  # loopback only
        return json.loads(resp.read().decode("utf-8"))


def fail(detail):
    out["detail"] = detail
    json.dump(out, sys.stdout, ensure_ascii=False)
    sys.exit(0)


def problem(exc):
    if isinstance(exc, urllib.error.HTTPError):
        try:
            return f"{exc.code} {exc.read().decode('utf-8', 'replace')[:300]}"
        except Exception:  # noqa: BLE001
            return str(exc.code)
    return str(exc)


spec = {
    "title": "smoke",
    "objective": "検証（staging）の煙試験。偽のアダプタが 1 往復するだけ（ADR-0041 D5）。",
    "acceptance": [{"type": "command", "cmd": "true", "expect_exit": 0}],
    "genre": "smoke",
    # 組み込みの役割（`adapter = "fake"`）を明示する。`assignee` を付けると、役割を省略した場合は
    # そのノードの分野の既定の役割（＝本物のアダプタ）が勝ってしまう。
    "role": "smoke",
    "max_retries": 0,
}
if assignee:
    spec["assignee"] = assignee

started = time.monotonic()
try:
    task = call("/api/v1/tasks", spec)
except Exception as exc:  # noqa: BLE001
    if assignee:
        # `assignee` がこのスナップショットの組織に無いだけかもしれない。付けずにもう一度。
        spec.pop("assignee")
        assignee = ""
        try:
            task = call("/api/v1/tasks", spec)
        except Exception as exc2:  # noqa: BLE001
            fail(f"POST /tasks failed: {problem(exc2)}")
    else:
        fail(f"POST /tasks failed: {problem(exc)}")

task_id = task.get("id") or ""
out["task_id"] = task_id
if not task_id:
    fail(f"POST /tasks returned no id: {json.dumps(task, ensure_ascii=False)[:200]}")
quoted = urllib.parse.quote(str(task_id), safe="")

# draft で作られていれば承認して ready にする（すでに ready なら何もしない）。
if task.get("status") == "draft":
    try:
        call(f"/api/v1/tasks/{quoted}/approve", {})
    except Exception as exc:  # noqa: BLE001
        fail(f"POST /tasks/{task_id}/approve failed: {problem(exc)}")

status = task.get("status") or ""
while time.monotonic() - started < TIMEOUT_S:
    try:
        status = (call(f"/api/v1/tasks/{quoted}") or {}).get("task", {}).get("status") or ""
    except Exception as exc:  # noqa: BLE001
        fail(f"GET /tasks/{task_id} failed: {problem(exc)}")
    if status in ("done", "failed", "cancelled"):
        break
    time.sleep(1)
out["elapsed_s"] = round(time.monotonic() - started, 2)
if status != "done":
    fail(f"the smoke task is {status!r} after {out['elapsed_s']}s (want 'done')")

try:
    events = (call(f"/api/v1/tasks/{quoted}/events") or {}).get("items") or []
except Exception as exc:  # noqa: BLE001
    fail(f"GET /tasks/{task_id}/events failed: {problem(exc)}")
kinds = [(e.get("event") or {}).get("type") for e in events]
if "worker_started" not in kinds:
    fail(f"no worker_started event (events: {kinds})")
finished = [
    (e.get("event") or {}).get("outcome") or ""
    for e in events
    if (e.get("event") or {}).get("type") == "worker_finished"
    and not (e.get("event") or {}).get("role")
]
if not finished:
    fail(f"no worker_finished event (events: {kinds})")
if not any(o.startswith("done") for o in finished):
    fail(f"worker_finished outcomes are {finished} (want one starting with 'done')")

# 報告（ADR-0034 の終端での決定的な生成）。`assignee` を付けられたときだけ確かめる。
if assignee:
    node = urllib.parse.quote(str(assignee), safe="")
    try:
        reports = (call(f"/api/v1/reports?node={node}&limit=500") or {}).get("items") or []
    except Exception as exc:  # noqa: BLE001
        fail(f"GET /reports?node={assignee} failed: {problem(exc)}")
    mine = [r for r in reports if str(r.get("task_id") or "") == str(task_id)]
    if not mine:
        fail(f"no report for the smoke task under node {assignee!r} ({len(reports)} reports there)")
    out["report"] = str(mine[0].get("id") or "")

out["ok"] = True
out["detail"] = (
    f"done in {out['elapsed_s']}s (worker_finished {finished[0]!r}"
    + (f", report {out['report']} for node {assignee}" if assignee else ", no assignee: no report expected")
    + ")"
)
json.dump(out, sys.stdout, ensure_ascii=False)
PY
}

COUNT_KEYS="tasks tasks_total projects milestones org approvals approvals_decided reports messages tasks_digest"

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

# ---- 2. 件数一致（同じスナップショットのマイグレーション前後。ADR-0041 D2）--

# 基準は `$SNAP_JSON`（`.backup` 直後、マイグレーション前の生の行数）。比べる相手は
# staging API（同じファイルを新バイナリがマイグレーションした後）。**本番は出てこない**ので、
# 本番が動いていてもこの検査は揺れない。

OK2=false
STG_JSON="$SD_STAGING/logs/counts-staging.json"
COUNT_DIFF=""
if [ "$OK1" = true ]; then
  collect "$NEW_BASE" "$ST_TOKEN" >"$STG_JSON" || true
  if sd_json_valid "$SNAP_JSON" && sd_json_valid "$STG_JSON"; then
    OK2=true
    for key in $COUNT_KEYS; do
      a="$(sd_json_get "$SNAP_JSON" "$key" || echo "?")"
      b="$(sd_json_get "$STG_JSON" "$key" || echo "??")"
      if [ "$a" != "$b" ]; then
        OK2=false
        COUNT_DIFF="$COUNT_DIFF $key(snapshot=$a staging=$b)"
      fi
    done
    serr="$(sd_json_get "$STG_JSON" errors || echo '[]')"
    if [ "$serr" != "[]" ]; then
      OK2=false
      COUNT_DIFF="$COUNT_DIFF errors(staging=$serr)"
    fi
    if [ "$OK2" = true ]; then
      record 2 counts-match true "snapshot (pre-migration, sqlite3) == staging API (post-migration): $COUNT_KEYS"
    else
      hint=""
      orphans="$(sd_json_get "$SNAP_JSON" orphan_reports || echo 0)/$(sd_json_get "$SNAP_JSON" orphan_messages || echo 0)"
      case "$COUNT_DIFF" in
        *reports* | *messages*)
          hint=" (orphan reports/messages whose node_id is no longer in org_nodes: $orphans — the API counts per node, so those rows cannot be reached)"
          ;;
      esac
      record 2 counts-match false "migration changed the data:$COUNT_DIFF$hint"
    fi
  else
    record 2 counts-match false "could not collect counts (see $SNAP_JSON / $STG_JSON)"
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
    # 比べる相手は**スナップショット**（マイグレーション前の生の行数。ADR-0041 D2）。
    # 本番 API はここでも読まない。
    if sd_json_valid "$OLD_COUNTS" && sd_json_valid "$SNAP_JSON"; then
      for key in $COUNT_KEYS; do
        a="$(sd_json_get "$SNAP_JSON" "$key" || echo "?")"
        b="$(sd_json_get "$OLD_COUNTS" "$key" || echo "??")"
        [ "$a" = "$b" ] || DIFF5="$DIFF5 $key(snapshot=$a old=$b)"
      done
    else
      DIFF5=" could-not-collect"
    fi
    if [ "$OLD_SCHEMA" = "$NEW_SCHEMA" ] && [ -z "$BAD5" ] && [ -z "$DIFF5" ]; then
      LIVE_OK=true
      record 5 n-1-compat true "old taskd ($CUR) reads the migrated snapshot: schema_version=$OLD_SCHEMA, counts match the pre-migration snapshot, main GETs 200"
    else
      record 5 n-1-compat false "old taskd ($CUR) is not compatible: schema=$OLD_SCHEMA(want $NEW_SCHEMA) gets:$BAD5 counts:$DIFF5"
    fi
  else
    record 5 n-1-compat false "old taskd ($CUR) did not become healthy on $OLD_BASE within 60s (SchemaTooNew?); see $OLD_LOG ($(tail -n 3 "$OLD_LOG" | tr '\n' ' '))"
  fi
fi

# ---- 6. 煙試験（ADR-0041 D5）------------------------------------------------
#
# **検査 5 の後**に回す。ここで 1 件タスクを足すので、先に回すと検査 2（件数一致）と検査 5（N-1）の
# 基準がずれてしまう。足したタスクは staging のスナップショットの中だけで、本番には残らない。

OK6=false
SMOKE_JSON="$SD_STAGING/logs/smoke.json"
if [ "$OK1" = true ]; then
  sd_log "smoke: POST $NEW_BASE/api/v1/tasks {genre: smoke, role: smoke} (assignee: ${FIRST_ORG:-<none>})"
  smoke "$NEW_BASE" "$ST_TOKEN" "$FIRST_ORG" >"$SMOKE_JSON" || true
  if sd_json_valid "$SMOKE_JSON"; then
    SMOKE_OK="$(sd_json_get "$SMOKE_JSON" ok || echo false)"
    SMOKE_TASK="$(sd_json_get "$SMOKE_JSON" task_id || echo "")"
    SMOKE_ELAPSED="$(sd_json_get "$SMOKE_JSON" elapsed_s || echo 0)"
    SMOKE_DETAIL="$(sd_json_get "$SMOKE_JSON" detail || echo "")"
    [ "$SMOKE_OK" = true ] && OK6=true
    record 6 smoke "$OK6" "$SMOKE_DETAIL" "$SMOKE_TASK" "$SMOKE_ELAPSED"
  else
    record 6 smoke false "could not run the smoke task (see $SMOKE_JSON and $NEW_LOG)"
  fi
else
  record 6 smoke false "skipped (check 1 failed)"
fi

# ---- verify.json ------------------------------------------------------------

OK=false
if [ "$OK1" = true ] && [ "$OK2" = true ] && [ "$OK3" = true ] && [ "$OK4" = true ] && [ "$OK6" = true ]; then
  OK=true
fi

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
  printf '    "snapshot": %s,\n' "$(counts_or_null "$SNAP_JSON")"
  printf '    "staging": %s\n' "$(counts_or_null "$STG_JSON")"
  printf '  }\n'
  printf '}\n'
} >"$REL/verify.json"
rm -f "$CHECKS_TSV"

sd_log "verify.json: $REL/verify.json (ok=$OK live_ok=$LIVE_OK)"
[ "$OK" = true ] || exit 1
