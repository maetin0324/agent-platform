#!/usr/bin/env bash
# taskd（$TASKD_REPO、既定 ../agent-platform）を fake ワーカー + [api] の設定で扱う補助スクリプト（docs/DESIGN.md §10.0）。
#   scripts/taskd.sh build                 cargo build -p taskd -p taskctl
#   scripts/taskd.sh start <name>          .run/<name>/ に taskd.toml と DB・workspaces/ を作りバックグラウンド起動
#   scripts/taskd.sh stop <name>           停止（SIGTERM → 猶予後 SIGKILL）
#   scripts/taskd.sh status <name>         起動中か、/health が返るか
#   scripts/taskd.sh logs <name>           .run/<name>/taskd.log を表示
#   scripts/taskd.sh taskctl <name> ...    taskctl --db .run/<name>/taskd.sqlite3 ... を実行
#   scripts/taskd.sh fixture <scenario>    既知の DB を作る（シナリオは各フェーズで追加。G0 では骨組みのみ）
# 環境変数: TASKD_REPO、TASKD_API_LISTEN（既定 127.0.0.1:7710）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TASKD_REPO="$(cd "${TASKD_REPO:-$ROOT/../agent-platform}" 2>/dev/null && pwd || echo "${TASKD_REPO:-$ROOT/../agent-platform}")"
TASKD_BIN="$TASKD_REPO/target/debug/taskd"
TASKCTL_BIN="$TASKD_REPO/target/debug/taskctl"
RUN_ROOT="$ROOT/.run"
API_LISTEN="${TASKD_API_LISTEN:-127.0.0.1:7710}"
TMPL="$ROOT/test/taskd/taskd.toml.tmpl"
DEFAULT_WORKER="$ROOT/test/taskd/fake-worker.sh"

usage() { sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2; }
die() { echo "taskd.sh: $*" >&2; exit 1; }
need_name() { [ $# -ge 1 ] && [ -n "$1" ] || die "name is required"; }
run_dir() { echo "$RUN_ROOT/$1"; }
pid_of() { local f; f="$(run_dir "$1")/taskd.pid"; [ -f "$f" ] && cat "$f" || true; }
alive() { local p; p="$(pid_of "$1")"; [ -n "$p" ] && kill -0 "$p" 2>/dev/null; }
health_url() { echo "http://$API_LISTEN/api/v1/health"; }

cmd_build() {
  [ -d "$TASKD_REPO" ] || die "TASKD_REPO not found: $TASKD_REPO"
  (cd "$TASKD_REPO" && cargo build -p taskd -p taskctl)
  [ -x "$TASKD_BIN" ] && [ -x "$TASKCTL_BIN" ] || die "binaries not found after build"
}

# .run/<name>/ を用意する（既存の DB は残す）。fake-worker.sh が無ければ既定のものを置く。
prepare() {
  local name="$1" dir; dir="$(run_dir "$name")"
  mkdir -p "$dir/workspaces"
  [ -f "$dir/fake-worker.sh" ] || cp "$DEFAULT_WORKER" "$dir/fake-worker.sh"
  sed -e "s#@RUN_DIR@#$dir#g" -e "s#@API_LISTEN@#$API_LISTEN#g" "$TMPL" > "$dir/taskd.toml"
}

cmd_start() {
  need_name "$@"; local name="$1" dir; dir="$(run_dir "$name")"
  [ -x "$TASKD_BIN" ] || die "taskd binary not found ($TASKD_BIN); run 'scripts/taskd.sh build' first"
  if alive "$name"; then echo "taskd '$name' already running (pid $(pid_of "$name"))"; return 0; fi
  # 追跡していないプロセスが既に API のポートを掴んでいたら起動しない（別 name の taskd や取り残し）
  if curl -sf -o /dev/null "$(health_url)"; then die "something already answers $(health_url); stop it first (scripts/taskd.sh stop <name>)"; fi
  prepare "$name"
  # exec で置き換えるので $! が taskd 自身の pid になる
  ( cd "$dir" && exec nohup "$TASKD_BIN" --config "$dir/taskd.toml" --log-format text >> "$dir/taskd.log" 2>&1 ) &
  echo $! > "$dir/taskd.pid"
  local i
  for i in $(seq 1 50); do
    alive "$name" || { echo "taskd '$name' exited early; last log lines:" >&2; tail -n 20 "$dir/taskd.log" >&2; rm -f "$dir/taskd.pid"; return 1; }
    if curl -sf -o /dev/null "$(health_url)"; then
      echo "taskd '$name' started (pid $(pid_of "$name"), api http://$API_LISTEN, dir $dir)"; return 0
    fi
    sleep 0.2
  done
  die "taskd '$name' did not answer $(health_url) within 10s (see $dir/taskd.log)"
}

cmd_stop() {
  need_name "$@"; local name="$1" dir p i; dir="$(run_dir "$name")"; p="$(pid_of "$name")"
  if [ -z "$p" ]; then echo "taskd '$name' is not running"; return 0; fi
  if kill -0 "$p" 2>/dev/null; then
    kill -TERM "$p" 2>/dev/null || true
    for i in $(seq 1 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.2; done
    kill -0 "$p" 2>/dev/null && { kill -KILL "$p" 2>/dev/null || true; sleep 0.2; }
  fi
  rm -f "$dir/taskd.pid"
  # API が消えたことも確認する（追跡外のプロセスが残っていれば警告）
  for i in $(seq 1 25); do curl -sf -o /dev/null "$(health_url)" || break; sleep 0.2; done
  if curl -sf -o /dev/null "$(health_url)"; then echo "warning: $(health_url) still answers after stopping '$name' (another process?)" >&2; fi
  echo "taskd '$name' stopped"
}

cmd_status() {
  need_name "$@"; local name="$1"
  if alive "$name"; then echo "taskd '$name': running (pid $(pid_of "$name"))"; else echo "taskd '$name': not running"; fi
  if curl -sf "$(health_url)"; then echo; else echo "health: no answer at $(health_url)"; fi
}

cmd_logs() { need_name "$@"; local f; f="$(run_dir "$1")/taskd.log"; [ -f "$f" ] || die "no log: $f"; tail -n "${2:-100}" "$f"; }

cmd_taskctl() {
  need_name "$@"; local name="$1"; shift
  [ -x "$TASKCTL_BIN" ] || die "taskctl binary not found ($TASKCTL_BIN); run 'scripts/taskd.sh build' first"
  "$TASKCTL_BIN" --db "$(run_dir "$name")/taskd.sqlite3" "$@"
}

# fixture <scenario>: .run/<scenario>/ を作り直し、taskctl と taskd --until-idle で既知の DB を作る。
# シナリオはフェーズごとに case を追加する（G1: basic、G4: multi-account / unroutable）。
cmd_fixture() {
  [ $# -ge 1 ] || die "scenario is required"
  local scenario="$1"
  case "$scenario" in
    *) die "unknown fixture scenario '$scenario' (none defined yet; scenarios are added from Phase G1)" ;;
  esac
}

[ $# -ge 1 ] || usage
cmd="$1"; shift
case "$cmd" in
  build) cmd_build "$@" ;;
  start) cmd_start "$@" ;;
  stop) cmd_stop "$@" ;;
  status) cmd_status "$@" ;;
  logs) cmd_logs "$@" ;;
  taskctl) cmd_taskctl "$@" ;;
  fixture) cmd_fixture "$@" ;;
  -h|--help|help) usage ;;
  *) die "unknown command '$cmd'" ;;
esac
