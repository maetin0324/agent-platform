#!/usr/bin/env bash
# taskd（$TASKD_REPO、既定は gui/ の親 = リポジトリの根。ADR-0020）を fake ワーカー + [api] の設定で扱う補助スクリプト（docs/DESIGN.md §10.0）。
#   scripts/taskd.sh build                 cargo build -p taskd -p taskctl
#   scripts/taskd.sh start <name>          .run/<name>/ に taskd.toml と DB・workspaces/ を作りバックグラウンド起動
#   scripts/taskd.sh stop <name>           停止（SIGTERM → 猶予後 SIGKILL）
#   scripts/taskd.sh status <name>         起動中か、/health が返るか
#   scripts/taskd.sh logs <name>           .run/<name>/taskd.log を表示
#   scripts/taskd.sh taskctl <name> ...    taskctl --db .run/<name>/taskd.sqlite3 ... を実行
#   scripts/taskd.sh fixture <scenario>    既知の DB を作る（basic / unroutable / auth / clusters / delegation。multi-account は設定のみで DB は作らない。docs/adr/0007 D1）
# 環境変数: TASKD_REPO、TASKD_API_LISTEN（既定 127.0.0.1:7710）、TASKD_RUN_ROOT（.run の実体。既定はローカルディスク、下記）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TASKD_REPO="$(cd "${TASKD_REPO:-$ROOT/..}" 2>/dev/null && pwd || echo "${TASKD_REPO:-$ROOT/..}")"
TASKD_BIN="$TASKD_REPO/target/debug/taskd"
TASKCTL_BIN="$TASKD_REPO/target/debug/taskctl"
# .run/ の実体（docs/adr/0008 D13）。SQLite の WAL はネットワーク FS（NFS 等）上では tick が数十秒止まる（taskd-requests.md R1 の回答、
# taskd の ADR-0013 D5）ので、既定をローカルディスク（$TMPDIR か /tmp）にし、$ROOT/.run はそこへのシンボリックリンクにする。
# 優先順: $TASKD_RUN_ROOT > 既存の $ROOT/.run のリンク先 > ${TMPDIR:-/tmp}/taskd-gui-run-$USER。e2e と GUI は常に $ROOT/.run 経由で参照する。
RUN_LINK="$ROOT/.run"
if [ -n "${TASKD_RUN_ROOT:-}" ]; then
  RUN_ROOT="$TASKD_RUN_ROOT"
elif [ -L "$RUN_LINK" ]; then
  RUN_ROOT="$(readlink -f "$RUN_LINK")"
else
  RUN_ROOT="${TMPDIR:-/tmp}/taskd-gui-run-$(id -un)"
fi
API_LISTEN="${TASKD_API_LISTEN:-127.0.0.1:7710}"
TMPL="$ROOT/test/taskd/taskd.toml.tmpl"
DEFAULT_WORKER="$ROOT/test/taskd/fake-worker.sh"

usage() { sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 2; }
die() { echo "taskd.sh: $*" >&2; exit 1; }
# $RUN_ROOT を作り、$ROOT/.run をそこへのリンクにする（既に実ディレクトリなら中身を保ったまま使い、ネットワーク FS なら警告）。
ensure_run_root() {
  mkdir -p "$RUN_ROOT"
  if [ -L "$RUN_LINK" ]; then
    [ "$(readlink -f "$RUN_LINK")" = "$(readlink -f "$RUN_ROOT")" ] || { rm -f "$RUN_LINK"; ln -s "$RUN_ROOT" "$RUN_LINK"; }
  elif [ -d "$RUN_LINK" ]; then
    RUN_ROOT="$RUN_LINK"
  elif [ ! -e "$RUN_LINK" ]; then
    ln -s "$RUN_ROOT" "$RUN_LINK"
  fi
  local fstype; fstype="$(stat -f -c %T "$RUN_ROOT" 2>/dev/null || echo unknown)"
  case "$fstype" in
    nfs*|cifs|smb*|fuse*) echo "taskd.sh: warning: $RUN_ROOT is on $fstype; SQLite WAL needs a local disk (set TASKD_RUN_ROOT). See docs/taskd-requests.md R1" >&2 ;;
  esac
}
ensure_run_root
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

# .run/<name>/ を用意する（既存の DB は残す）。fake-worker.sh / taskd.toml が既に無ければ既定のものを置く
# （fixture が独自の設定・ワーカーを用意済み（multi-account / unroutable）ならそれを残す。docs/adr/0007 D1）。
prepare() {
  local name="$1" worker_src="${2:-$DEFAULT_WORKER}" dir; dir="$(run_dir "$name")"
  mkdir -p "$dir/workspaces"
  # fixture のワーカーは RunRequest を read-run-request.mjs で解析する（G7-U3）。ワーカーは .run/<name>/ に
  # 写して使うので、読み取り役も同じディレクトリに置く（ワーカー側は自分の隣を見る）。
  cp "$ROOT/test/taskd/fixtures/read-run-request.mjs" "$dir/read-run-request.mjs"
  [ -f "$dir/fake-worker.sh" ] || cp "$worker_src" "$dir/fake-worker.sh"
  [ -f "$dir/taskd.toml" ] || sed -e "s#@RUN_DIR@#$dir#g" -e "s#@API_LISTEN@#$API_LISTEN#g" "$TMPL" > "$dir/taskd.toml"
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

# fixture <scenario>: .run/<scenario>/ を作り直し、taskctl と taskd --until-idle で既知の DB を作る
# （multi-account は例外で設定のみ。docs/adr/0007 D1）。シナリオはフェーズごとに case を追加する（G1: basic、G4: multi-account / unroutable）。
cmd_fixture() {
  [ $# -ge 1 ] || die "scenario is required"
  local scenario="$1"
  case "$scenario" in
    basic) fixture_basic ;;
    multi-account) fixture_multi_account ;;
    unroutable) fixture_unroutable ;;
    auth) fixture_auth ;;
    clusters) fixture_clusters ;;
    delegation) fixture_delegation ;;
    *) die "unknown fixture scenario '$scenario' (known: basic, multi-account, unroutable, auth, clusters, delegation)" ;;
  esac
}

# basic（docs/DESIGN.md §10 Phase G1、docs/adr/0004 D5）: 依存チェーン done×3、Human check の Approval 子 ready×1
# （親は reviewing）、question で blocked×1、plan.auto_accept=false の draft 子×2（plan 自体は done）、failed×1（retryable:false）。
fixture_basic() {
  local name="basic" dir; dir="$(run_dir "$name")"
  [ -x "$TASKD_BIN" ] && [ -x "$TASKCTL_BIN" ] || die "binaries not found; run 'scripts/taskd.sh build' first"
  alive "$name" && die "taskd '$name' is running; stop it first (scripts/taskd.sh stop $name)"
  rm -rf "$dir"
  mkdir -p "$dir/workspaces"
  cp "$ROOT/test/taskd/fixtures/basic-plan.json" "$dir/basic-plan.json"
  sed "s#@PLAN_JSON@#$dir/basic-plan.json#g" "$ROOT/test/taskd/fixtures/basic-worker.sh" > "$dir/fake-worker.sh"
  chmod +x "$dir/fake-worker.sh"
  prepare "$name" "$dir/fake-worker.sh"

  local db="$dir/taskd.sqlite3"
  tc() { "$TASKCTL_BIN" --db "$db" "$@"; }

  local t1 t2 t3 tb tq tp te
  t1=$(tc add --title "Chain-A1" --objective "chain step 1" --check-cmd "test -f artifacts/out.txt" --workspace ws-a1)
  tc approve "$t1" >/dev/null
  t2=$(tc add --title "Chain-A2" --objective "chain step 2" --check-cmd "test -f artifacts/out.txt" --depends-on "$t1" --workspace ws-a2)
  tc approve "$t2" >/dev/null
  t3=$(tc add --title "Chain-A3" --objective "chain step 3" --check-cmd "test -f artifacts/out.txt" --depends-on "$t2" --workspace ws-a3)
  tc approve "$t3" >/dev/null

  tb=$(tc add --title "Human-B" --objective "needs a human sign-off" --accept "someone signs off" --workspace ws-b)
  tc approve "$tb" >/dev/null

  tq=$(tc add --title "Blocked-C" --objective "needs clarification before it can finish" --check-cmd "test -f artifacts/answered.txt" --workspace ws-c)
  tc approve "$tq" >/dev/null

  tp=$(tc plan "fixture plan goal: build two small things" --workspace ws-plan)
  tc approve "$tp" >/dev/null

  te=$(tc add --title "Failed-E" --objective "always fails" --check-cmd "true" --workspace ws-e)
  tc approve "$te" >/dev/null

  # G3（成果物ビューア）: Markdown・JSON・PNG を出す done タスク（docs/adr/0006-g3-decisions.md D6）。
  local tg
  tg=$(tc add --title "Artifacts-G" --objective "produce a markdown, json and png artifact" --check-cmd "test -f artifacts/data.json" --workspace ws-g)
  tc approve "$tg" >/dev/null

  "$TASKD_BIN" --config "$dir/taskd.toml" --until-idle --log-format text >> "$dir/taskd.log" 2>&1

  echo "fixture 'basic' built at $dir"
  echo "  chain: $t1 $t2 $t3 (done x3)"
  echo "  human-check: $tb (reviewing; approval child ready)"
  echo "  blocked: $tq"
  echo "  plan: $tp (done; 2 draft children)"
  echo "  failed: $te"
  echo "  artifacts: $tg (done; note.md/data.json/image.png)"
}

# multi-account（docs/DESIGN.md §10 Phase G4、docs/adr/0007 D1/D4）: 設定と workspace を用意するだけで DB は作らない
# （他の fixture と違う: cooldown はプロセス内メモリのみで DB から再構築できないため、スロットルを起こす
# taskctl add/approve は e2e が `start multi-account` した後の生きたプロセスに対して直接行う）。
fixture_multi_account() {
  local name="multi-account" dir; dir="$(run_dir "$name")"
  alive "$name" && die "taskd '$name' is running; stop it first (scripts/taskd.sh stop $name)"
  rm -rf "$dir"
  mkdir -p "$dir/workspaces"
  cp "$ROOT/test/taskd/fixtures/multi-account-worker.sh" "$dir/fake-worker.sh"
  chmod +x "$dir/fake-worker.sh"
  sed -e "s#@RUN_DIR@#$dir#g" -e "s#@API_LISTEN@#$API_LISTEN#g" "$ROOT/test/taskd/multi-account.toml.tmpl" > "$dir/taskd.toml"
  echo "fixture 'multi-account' prepared at $dir (config only; DB is empty)"
  echo "  providers: acct-a (throttles), acct-b (falls back and succeeds)"
  echo "  start it (scripts/taskd.sh start multi-account) and taskctl add/approve against the running process"
}

# unroutable（docs/DESIGN.md §10 Phase G4、docs/adr/0007 D2）: cheap タスクに frontier だけのプロバイダを与え、
# --until-idle で ready のまま残す（unroutable 判定は毎 tick 現在の DB から計算するので、basic と同じく再起動しても保たれる）。
fixture_unroutable() {
  local name="unroutable" dir; dir="$(run_dir "$name")"
  [ -x "$TASKD_BIN" ] && [ -x "$TASKCTL_BIN" ] || die "binaries not found; run 'scripts/taskd.sh build' first"
  alive "$name" && die "taskd '$name' is running; stop it first (scripts/taskd.sh stop $name)"
  rm -rf "$dir"
  mkdir -p "$dir/workspaces/ws-u"
  cp "$DEFAULT_WORKER" "$dir/fake-worker.sh"
  sed -e "s#@RUN_DIR@#$dir#g" -e "s#@API_LISTEN@#$API_LISTEN#g" "$ROOT/test/taskd/unroutable.toml.tmpl" > "$dir/taskd.toml"

  local db="$dir/taskd.sqlite3"
  tc() { "$TASKCTL_BIN" --db "$db" "$@"; }

  local tu
  tu=$(tc add --title "Unroutable-U" --objective "cheap task with no matching provider" --tier cheap --check-cmd "true" --workspace "$dir/workspaces/ws-u")
  tc approve "$tu" >/dev/null

  "$TASKD_BIN" --config "$dir/taskd.toml" --until-idle --log-format text >> "$dir/taskd.log" 2>&1

  echo "fixture 'unroutable' built at $dir"
  echo "  unroutable: $tu (ready; no provider matches the cheap tier)"
}

# auth（docs/DESIGN.md §10 Phase G5 受け入れ条件 2、docs/adr/0008 D12）: `[api] token_file = "api.token"` の設定で起動する taskd。
# `.run/auth/api.token` に乱数トークンを書く（GUI 側は TASKD_API_TOKEN_FILE=.run/auth/api.token を渡す）。DB は done×1 の最小限。
fixture_auth() {
  local name="auth" dir; dir="$(run_dir "$name")"
  [ -x "$TASKD_BIN" ] && [ -x "$TASKCTL_BIN" ] || die "binaries not found; run 'scripts/taskd.sh build' first"
  alive "$name" && die "taskd '$name' is running; stop it first (scripts/taskd.sh stop $name)"
  rm -rf "$dir"
  mkdir -p "$dir/workspaces"
  cp "$DEFAULT_WORKER" "$dir/fake-worker.sh"
  chmod +x "$dir/fake-worker.sh"
  head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "$dir/api.token"
  chmod 600 "$dir/api.token"
  sed -e "s#@RUN_DIR@#$dir#g" -e "s#@API_LISTEN@#$API_LISTEN#g" "$ROOT/test/taskd/auth.toml.tmpl" > "$dir/taskd.toml"

  local db="$dir/taskd.sqlite3"
  tc() { "$TASKCTL_BIN" --db "$db" "$@"; }
  local ta
  ta=$(tc add --title "Auth-A" --objective "token-protected taskd" --check-cmd "true" --workspace ws-auth)
  tc approve "$ta" >/dev/null

  "$TASKD_BIN" --config "$dir/taskd.toml" --until-idle --log-format text >> "$dir/taskd.log" 2>&1

  echo "fixture 'auth' built at $dir"
  echo "  token file: $dir/api.token (pass it to the GUI as TASKD_API_TOKEN_FILE)"
  echo "  task: $ta (done)"
}

# clusters（docs/DESIGN.md §10 Phase G7、docs/adr/0018 [taskd]）: [[clusters]] を 2 つ持つ設定
# （`local` は `~/.ssh/config` の `taskd-localhost`（localhost への多重接続、connected: true）、
# `offline` は到達しない host で connected: false）。`local` 向けのタスクは実際に push → run → 判定 → pull を
# localhost 相手に行う（taskd 側の tests/e2e/tests/cluster_scenarios.rs と同じ流儀。外部ネットワークには出ない）。
# ssh の多重接続が無い環境では作れない（あらかじめ `ssh -MNf taskd-localhost` 等で張っておくこと）。
fixture_clusters() {
  local name="clusters" dir; dir="$(run_dir "$name")"
  [ -x "$TASKD_BIN" ] && [ -x "$TASKCTL_BIN" ] || die "binaries not found; run 'scripts/taskd.sh build' first"
  alive "$name" && die "taskd '$name' is running; stop it first (scripts/taskd.sh stop $name)"
  command -v ssh >/dev/null || die "ssh not found; the clusters fixture needs it (docs/adr/0018 [taskd])"
  command -v rsync >/dev/null || die "rsync not found; the clusters fixture needs it (docs/adr/0018 [taskd])"
  ssh -o BatchMode=yes -O check taskd-localhost >/dev/null 2>&1 ||
    die "no ssh control master for 'taskd-localhost'; run 'ssh -MNf taskd-localhost' first (see ~/.ssh/config)"
  rm -rf "$dir"
  mkdir -p "$dir/workspaces" "$dir/remote/cluster-local-project" "$dir/remote/cluster-offline-project"
  cp "$ROOT/test/taskd/fixtures/clusters-worker.sh" "$dir/fake-worker.sh"
  chmod +x "$dir/fake-worker.sh"
  printf 'cluster-only\n' > "$dir/remote/cluster-local-project/secret.txt"
  sed -e "s#@RUN_DIR@#$dir#g" -e "s#@API_LISTEN@#$API_LISTEN#g" "$ROOT/test/taskd/clusters.toml.tmpl" > "$dir/taskd.toml"

  local db="$dir/taskd.sqlite3"
  tc() { "$TASKCTL_BIN" --db "$db" "$@"; }

  local tl to
  tl=$(tc add --title "Cluster-Local" --objective "runs on a reachable cluster" \
    --check-cmd "grep -q cluster-only answer.txt" --cluster local --workspace "$dir/remote/cluster-local-project")
  tc approve "$tl" >/dev/null
  to=$(tc add --title "Cluster-Offline" --objective "cluster is unreachable" \
    --check-cmd "true" --cluster offline --workspace "$dir/remote/cluster-offline-project")
  tc approve "$to" >/dev/null

  "$TASKD_BIN" --config "$dir/taskd.toml" --until-idle --max-ticks 2000 --log-format text >> "$dir/taskd.log" 2>&1

  echo "fixture 'clusters' built at $dir"
  echo "  local (connected): $tl (done via ssh taskd-localhost)"
  echo "  offline (unreachable): $to (ready; ClusterUnavailable)"
}

# delegation（docs/DESIGN.md §10 Phase G7、docs/adr/0016 [taskd]）: role=lead, aggregate=true の親が
# `delegate` で 2 件の role=implementer の子を作り、子が終端になった後の集約 run が artifacts/summary.md を書く。
fixture_delegation() {
  local name="delegation" dir; dir="$(run_dir "$name")"
  [ -x "$TASKD_BIN" ] && [ -x "$TASKCTL_BIN" ] || die "binaries not found; run 'scripts/taskd.sh build' first"
  alive "$name" && die "taskd '$name' is running; stop it first (scripts/taskd.sh stop $name)"
  rm -rf "$dir"
  mkdir -p "$dir/workspaces"
  cp "$ROOT/test/taskd/fixtures/delegation-worker.sh" "$dir/fake-worker.sh"
  cp "$ROOT/test/taskd/fixtures/read-run-request.mjs" "$dir/read-run-request.mjs"
  chmod +x "$dir/fake-worker.sh"
  sed -e "s#@RUN_DIR@#$dir#g" -e "s#@API_LISTEN@#$API_LISTEN#g" "$ROOT/test/taskd/delegation.toml.tmpl" > "$dir/taskd.toml"

  local db="$dir/taskd.sqlite3"
  tc() { "$TASKCTL_BIN" --db "$db" "$@"; }

  local tl
  # 受け入れ条件は committer は初回の delegate run でも即 pass する必要がある（子が終端になるまで待つ・
  # 集約 run を予約するのは all_pass=true の場合だけ、crates/task-dispatch/src/dispatcher.rs の
  # `settle_awaiting_children` / `needs_aggregate_run`）。summary.md の存在確認はここでは行わない
  # （fixture 構築後に GUI/e2e 側が `artifacts` 一覧で確認する）。
  tl=$(tc add --title "Lead-Delegator" --objective "delegates work to two implementers" \
    --check-cmd "true" --role lead --aggregate --config "$dir/taskd.toml" --workspace ws-lead)
  tc approve "$tl" >/dev/null

  "$TASKD_BIN" --config "$dir/taskd.toml" --until-idle --log-format text >> "$dir/taskd.log" 2>&1

  echo "fixture 'delegation' built at $dir"
  echo "  lead: $tl (done; delegated 2 children, aggregate run wrote summary.md)"
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
