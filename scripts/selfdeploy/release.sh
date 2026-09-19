#!/usr/bin/env bash
# scripts/selfdeploy/release.sh <git-ref> — ADR-0040 D1/D2 の「リリース」段。
#
#   作業チェックアウトとは別の detached worktree（~/taskd/releases/.build/<sha12>）で
#   cargo test → clippy → build --release → GUI pnpm install/typecheck/test/build を順に回し、
#   全部 exit 0 のときだけ ~/taskd/releases/<sha12>/ を作る。
#   1 つでも非 0 なら**リリースを作らず**、.build/<sha12>/gate.json だけ残す。
#
# 本番には一切触れない（プロセスも DB も config も）。人でもワーカーでも実行してよい（D5）。
set -euo pipefail

SD_PROG=release
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: release.sh <git-ref>

  <git-ref>  ビルドする commit（ブランチ名 / タグ / sha。`HEAD` も可）

env:
  TASKD_HOME  既定 ~/taskd
  SD_REPO     既定 ~/workspace/agent-platform（git worktree を生やす元のリポジトリ）
EOF
  exit 2
}

[ $# -eq 1 ] || usage
REF="$1"

sd_require_json_tool
command -v cargo >/dev/null 2>&1 || sd_die "cargo not found"
sd_use_pnpm

SHA12="$(sd_sha12 "$REF")"
SHA_FULL="$(sd_sha_full "$REF")"
sd_mkdirs

BUILD="$SD_BUILD_ROOT/$SHA12"
REL="$(sd_release_dir "$SHA12")"
GATE_TSV="$(mktemp)"
trap 'rm -f "$GATE_TSV"' EXIT

CUR="$(sd_current_sha)"
PREV="$(sd_previous_sha)"
if [ -d "$REL" ]; then
  if [ "$SHA12" = "$CUR" ] || [ "$SHA12" = "$PREV" ]; then
    sd_die "release $SHA12 is currently \`current\`/\`previous\`; refusing to rebuild over it"
  fi
  sd_log "release $SHA12 already exists; rebuilding it (old directory will be replaced)"
fi

sd_log "ref=$REF sha=$SHA_FULL sha12=$SHA12"
sd_log "build worktree: $BUILD"
sd_log "CARGO_TARGET_DIR: $SD_CARGO_TARGET"

# ---- detached worktree -----------------------------------------------------

git -C "$SD_REPO" worktree remove --force "$BUILD" >/dev/null 2>&1 || true
rm -rf "$BUILD"
git -C "$SD_REPO" worktree prune
git -C "$SD_REPO" worktree add --detach "$BUILD" "$SHA_FULL" >&2
mkdir -p "$SD_CARGO_TARGET"

export CARGO_TARGET_DIR="$SD_CARGO_TARGET"
# gate はネットワークに出ない前提（Cargo.lock / pnpm-lock.yaml は固定）。
export CARGO_TERM_COLOR=never

printf 'step:s exit:i secs:f log:s\n' >"$GATE_TSV"
GATE_OK=true
GATE_FAILED_STEP=""

# `run_step <name> <workdir> -- <cmd...>`
run_step() {
  local name="$1" workdir="$2"
  shift 2
  [ "$1" = "--" ] && shift
  local log="$BUILD/.gate-$name.log" start end secs rc
  if [ "$GATE_OK" != true ]; then return 0; fi
  sd_log "step $name: $* (cwd $workdir)"
  start="$(date +%s.%N)"
  rc=0
  ( cd "$workdir" && "$@" ) >"$log" 2>&1 || rc=$?
  end="$(date +%s.%N)"
  secs="$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.3f", b - a }')"
  printf '%s\t%s\t%s\t%s\n' "$name" "$rc" "$secs" ".gate-$name.log" >>"$GATE_TSV"
  if [ "$rc" -eq 0 ]; then
    sd_log "step $name: exit 0 in ${secs}s"
  else
    sd_log "step $name: exit $rc in ${secs}s — see $log"
    tail -n 30 "$log" >&2 || true
    GATE_OK=false
    GATE_FAILED_STEP="$name"
  fi
}

# gate.json を書く（成功でも失敗でも同じ形）。
write_gate_json() {
  local dest="$1" steps
  steps="$(sd_tsv_to_json "$GATE_TSV")"
  {
    printf '{\n'
    printf '  "sha": %s,\n' "$(sd_json_str "$SHA_FULL")"
    printf '  "sha12": %s,\n' "$(sd_json_str "$SHA12")"
    printf '  "ref": %s,\n' "$(sd_json_str "$REF")"
    printf '  "at": %s,\n' "$(sd_json_str "$(sd_ts)")"
    printf '  "ok": %s,\n' "$GATE_OK"
    printf '  "failed_step": %s,\n' "$(sd_json_str "$GATE_FAILED_STEP")"
    printf '  "steps": %s\n' "$steps"
    printf '}\n'
  } >"$dest"
}

# ---- gate（D2 の順番どおり） ----------------------------------------------

run_step cargo-test "$BUILD" -- cargo test --workspace
run_step cargo-clippy "$BUILD" -- cargo clippy --workspace -- -D warnings
run_step cargo-build "$BUILD" -- cargo build --release -p taskd -p taskctl
run_step pnpm-install "$BUILD/gui" -- pnpm install --frozen-lockfile
run_step pnpm-typecheck "$BUILD/gui" -- pnpm typecheck
run_step pnpm-test "$BUILD/gui" -- pnpm test
run_step pnpm-build "$BUILD/gui" -- pnpm build

if [ "$GATE_OK" != true ]; then
  write_gate_json "$BUILD/gate.json"
  sd_log "gate failed at step '$GATE_FAILED_STEP'; no release directory created"
  sd_log "gate.json: $BUILD/gate.json (build worktree kept so the logs survive)"
  exit 1
fi

# ---- リリースの組み立て ----------------------------------------------------

STAGE="$REL.partial"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/gui"

for b in taskd taskctl; do
  [ -x "$SD_CARGO_TARGET/release/$b" ] || sd_die "built binary missing: $SD_CARGO_TARGET/release/$b"
  cp -p "$SD_CARGO_TARGET/release/$b" "$STAGE/bin/$b"
done

[ -d "$BUILD/gui/build" ] || sd_die "gui build output missing: $BUILD/gui/build"
cp -r "$BUILD/gui/build" "$STAGE/gui/build"
for f in server.js package.json pnpm-lock.yaml pnpm-workspace.yaml; do
  [ -f "$BUILD/gui/$f" ] || sd_die "gui file missing: $BUILD/gui/$f"
  cp -p "$BUILD/gui/$f" "$STAGE/gui/$f"
done

sd_log "gui: pnpm install --prod --frozen-lockfile in $STAGE/gui"
( cd "$STAGE/gui" && pnpm install --prod --frozen-lockfile ) >&2 \
  || sd_die "pnpm install --prod failed in the release gui directory"

SCHEMA_VERSION="$(sd_schema_version_of_tree "$BUILD")" \
  || sd_die "cannot parse SCHEMA_VERSION from crates/task-core/src/store.rs at $SHA12"
# `taskd` の版は Cargo.toml から読む（バイナリを起こさない。`--version` は無い）。
TASKD_VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "$BUILD/crates/taskd/Cargo.toml" | head -n 1)"
if [ -z "$TASKD_VERSION" ]; then
  TASKD_VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "$BUILD/Cargo.toml" | head -n 1)"
fi
GUI_VERSION="$(sd_json_get "$STAGE/gui/package.json" version || true)"

{
  printf '{\n'
  printf '  "sha": %s,\n' "$(sd_json_str "$SHA_FULL")"
  printf '  "sha12": %s,\n' "$(sd_json_str "$SHA12")"
  printf '  "ref": %s,\n' "$(sd_json_str "$REF")"
  printf '  "built_at": %s,\n' "$(sd_json_str "$(sd_ts)")"
  printf '  "built_by": %s,\n' "$(sd_json_str "${USER:-unknown}@$(hostname)")"
  printf '  "profile": "release",\n'
  printf '  "schema_version": %s,\n' "$SCHEMA_VERSION"
  printf '  "taskd_version": %s,\n' "$(sd_json_str "$TASKD_VERSION")"
  printf '  "gui_version": %s,\n' "$(sd_json_str "$GUI_VERSION")"
  printf '  "gate_ok": true\n'
  printf '}\n'
} >"$STAGE/manifest.json"

write_gate_json "$STAGE/gate.json"
# gate のログも残す（失敗の再現に要る）。
mkdir -p "$STAGE/gate-logs"
for log in "$BUILD"/.gate-*.log; do
  [ -f "$log" ] || continue
  cp -p "$log" "$STAGE/gate-logs/$(basename "$log" | sed 's/^\.gate-//')"
done

rm -rf "$REL"
mv -T "$STAGE" "$REL"
sd_log "release ready: $REL"

# ---- ビルド用 worktree を消す ----------------------------------------------

git -C "$SD_REPO" worktree remove --force "$BUILD" >/dev/null 2>&1 || rm -rf "$BUILD"
git -C "$SD_REPO" worktree prune

# ---- 掃除（current / previous / 検証済みの新しい 3 件を残す） --------------

prune_releases() {
  local keep_file dir sha
  keep_file="$(mktemp)"
  [ -n "$CUR" ] && printf '%s\n' "$CUR" >>"$keep_file"
  [ -n "$PREV" ] && printf '%s\n' "$PREV" >>"$keep_file"
  printf '%s\n' "$SHA12" >>"$keep_file"
  # 検証済み（verify.json.ok == true）の新しい 3 件
  for dir in "$SD_RELEASES"/*/; do
    [ -d "$dir" ] || continue
    sha="$(basename "$dir")"
    case "$sha" in .*) continue ;; esac
    if [ "$(sd_json_get "$dir/verify.json" ok 2>/dev/null || echo false)" = true ]; then
      printf '%s\t%s\n' "$(sd_json_get "$dir/manifest.json" built_at 2>/dev/null || echo 0000)" "$sha"
    fi
  done | sort -r | head -n 3 | cut -f2 >>"$keep_file"

  for dir in "$SD_RELEASES"/*/; do
    [ -d "$dir" ] || continue
    sha="$(basename "$dir")"
    case "$sha" in .*) continue ;; esac
    if grep -qxF "$sha" "$keep_file"; then continue; fi
    sd_log "pruning release $sha"
    rm -rf "$dir"
  done
  rm -f "$keep_file"
}
prune_releases

sd_log "done: sha12=$SHA12 schema_version=$SCHEMA_VERSION"
printf '%s\n' "$SHA12"
