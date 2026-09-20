#!/usr/bin/env bash
# scripts/selfdeploy/migrate-to-celeris.sh <new sha12> [--dry-run] | --rollback
#
#   ADR-0045 D3 の**一度だけ**の移行。旧 `~/taskd/`（設定・DB・リリース・道具が全部 1 か所）を
#   `~/.config/celeris/`（設定と秘密）と `~/.local/celeris/`（状態）に分け、unit を
#   `taskd@` / `taskd-gui@` から `celeris@` / `celeris-gui@` に替える。停止 → 起動（数十秒止まる）。
#
#   **人だけが実行する**（ADR-0040 D5）。先に人が済ませておくこと:
#     CELERIS_STATE_DIR=~/.local/celeris scripts/selfdeploy/release.sh main
#     CELERIS_DB=~/taskd/taskd.sqlite3 CELERIS_CONFIG_DIR=~/taskd scripts/selfdeploy/verify.sh <new sha12>
#
#   このファイルは**旧い名前とパスを知っている唯一の場所**（ADR-0045 D4: 互換の読み替えは残さない）。
#
#   --dry-run   何も触らずに、`mv` の計画と**書き換えた後の設定ファイル全文**を標準出力に出す。
#               知らないパスが残っていたら exit 1（そこで止まるのが仕事）。
#   --rollback  新 unit を止め、ディレクトリを逆に移し、`backups/pre-celeris/` の設定と unit を戻して
#               旧 `taskd@<old>` を起こす（DB は schema が変わっていないのでそのまま使える）。
set -euo pipefail

SD_PROG=migrate
# shellcheck source=lib.sh
SD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SD_DIR/lib.sh"

# ---- 旧い世界（ここにしか書かない） ----------------------------------------

OLD_HOME="${CELERIS_OLD_HOME:-$HOME/taskd}"
OLD_CONFIG="$OLD_HOME/taskd.toml"
OLD_DB_NAME="taskd.sqlite3"
OLD_UNIT=taskd
OLD_GUI_UNIT=taskd-gui
export SD_OLD_UNITS="$OLD_UNIT@.service $OLD_GUI_UNIT@.service"

CONFIG="$CELERIS_CONFIG_DIR"
STATE="$CELERIS_STATE_DIR"
PRE="$STATE/backups/pre-celeris"
UNIT_DIR="${SD_UNIT_DIR:-$HOME/.config/systemd/user}"

usage() {
  cat >&2 <<'EOF'
usage: migrate-to-celeris.sh <new sha12> [--dry-run]
       migrate-to-celeris.sh --rollback

env:
  CELERIS_CONFIG_DIR  既定 ~/.config/celeris
  CELERIS_STATE_DIR   既定 ~/.local/celeris
  CELERIS_OLD_HOME    既定 ~/taskd（移行元）
  SD_STOP_WAIT        旧デーモンの停止を待つ上限（秒。既定 300）
EOF
  exit 2
}

MODE=migrate
DRY_RUN=false
SHA12=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    --rollback) MODE=rollback; shift ;;
    -h | --help) usage ;;
    -*) usage ;;
    *) SHA12="$1"; shift ;;
  esac
done
[ "$MODE" = rollback ] || [ -n "$SHA12" ] || usage

sd_require_json_tool
[ "$SD_JSON_TOOL" = python3 ] || sd_die "migrate-to-celeris.sh needs python3 (the config rewriter is written in it)"

# ---- 設定の書き換え（決定的な対応表。ADR-0045 D3）---------------------------
#
# 旧 `~/taskd/<x>` は**絶対**（`/home/rmaeda/taskd/<x>`）でも `~/taskd/<x>` でも書ける。
# どちらも下の表で新しい絶対パスにする。表に無い `<x>` が残っていたら**何も動かさずに止まる**。
# さらに、今日 `~/taskd` の中に解決している**裸の相対パス**（`db = "taskd.sqlite3"` など）も
# 同じ表で絶対パスにする（設定ファイルが `~/.config/celeris/` に移るので、相対のままだと行き先が変わる）。
rewrite_config() {
  local src="$1"
  CELERIS_MIG_OLD_HOME="$OLD_HOME" CELERIS_MIG_CONFIG="$CONFIG" CELERIS_MIG_STATE="$STATE" \
  CELERIS_MIG_OLD_DB="$OLD_DB_NAME" python3 - "$src" <<'PY'
import os, re, sys

home = os.path.expanduser("~")
old_home = os.environ["CELERIS_MIG_OLD_HOME"].rstrip("/")
config = os.environ["CELERIS_MIG_CONFIG"].rstrip("/")
state = os.environ["CELERIS_MIG_STATE"].rstrip("/")
old_db = os.environ["CELERIS_MIG_OLD_DB"]

# `<old_home>/<key>` -> 新しい絶対パス（ADR-0045 D3 の対応表）。表に無い `<key>` は「知らないパス」。
TABLE = {
    "secrets": f"{config}/secrets",
    "api.token": f"{config}/api.token",
    "gui.password": f"{config}/gui.password",
    "gui.session-secret": f"{config}/gui.session-secret",
    "org.toml": f"{config}/org.toml",
    "providers.d": f"{config}/providers.d",
    "taskd.toml": f"{config}/config.toml",
    "paperqa": f"{state}/tools/paperqa",
    "ldr": f"{state}/tools/ldr",
    "opencode": f"{state}/tools/opencode",
    old_db: f"{state}/celeris.sqlite3",
    "*.sqlite3": f"{state}/*.sqlite3",
    "logs": f"{state}/logs",
    "releases": f"{state}/releases",
    "backups": f"{state}/backups",
    "staging": f"{state}/staging",
    "workspaces": f"{state}/workspaces",
    "memory": f"{state}/memory",
    "claude-accounts": f"{state}/claude-accounts",
    "codex-accounts": f"{state}/codex-accounts",
    "current": f"{state}/current",
    "previous": f"{state}/previous",
}

# 今日 `<old_home>` 基準で解決している相対値を持つキー（ADR-0045 D3 が名指しするもの）。
RELATIVE_KEYS = {
    "db", "workspace_root", "token_file", "org_include", "providers_include",
    "dir", "claude_dir", "codex_dir", "releases_dir", "repo", "build_dir",
}
# パスを持ちうる残りのキー。相対で `/` を含むなら「知らないパス」として止める
# （`command = "pqa"` のような裸のコマンド名は触らない）。
OTHER_PATH_KEYS = {"command", "settings", "paper_directory", "index_directory", "worktree_root"}

# パスとして続けて読む文字（日本語の文やバッククォートで止まるように狭く取る）。
TAIL = r"[A-Za-z0-9._/*<>=+-]*"

text = open(sys.argv[1], encoding="utf-8").read()
unknown = []


def map_tail(tail):
    """`<old_home>` からの残り（`secrets/x.txt` など）を新しい絶対パスに。知らなければ None。"""
    head = tail.split("/", 1)[0]
    rest = tail[len(head):]
    if head not in TABLE:
        unknown.append(tail)
        return None
    return TABLE[head] + rest


# 1) 絶対 `<old_home>/<x>` と `~/<old_home の $HOME からの相対>/<x>`
forms = [re.escape(old_home)]
if old_home.startswith(home + "/"):
    forms.append(re.escape("~" + old_home[len(home):]))
pattern = re.compile(r"(?:" + "|".join(forms) + r")(?:/(?P<tail>" + TAIL + r"))?")


def abs_sub(m):
    tail = m.group("tail")
    if not tail:
        # `<old_home>` そのもの（`~/taskd` / `~/taskd/`）は状態側に寄せる。
        return state + ("/" if tail == "" else "")
    new = map_tail(tail)
    return m.group(0) if new is None else new


text = pattern.sub(abs_sub, text)


# 2) 裸の相対値（`db = "taskd.sqlite3"`、`workspace_root = "workspaces"` …）
def rel_sub(m):
    key, val = m.group("key"), m.group("val")
    if val.startswith("/") or val.startswith("~") or val == "":
        return m.group(0)
    if key in RELATIVE_KEYS or (key in OTHER_PATH_KEYS and "/" in val):
        new = map_tail(val)
        return m.group(0) if new is None else f'{m.group("pre")}"{new}"'
    return m.group(0)


text = re.sub(
    r'(?P<pre>(?P<key>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*)"(?P<val>[^"]*)"',
    rel_sub,
    text,
)

if unknown:
    sys.stderr.write("unknown path(s) under the old home; refusing to migrate:\n")
    for u in sorted(set(unknown)):
        sys.stderr.write(f"  {u}\n")
    sys.exit(3)

# 3) 残った旧い名前（コメント・役割の指示文・環境変数名）を改める。ここまでで**パスは全部**
#    新しい絶対パスになっているので、ここに残るのは散文と `TASKD_*` の env 名だけ
#    （branch 接頭辞 `taskd/<task-id>` の指示も `celeris/<task-id>` になる。ADR-0045 D1）。
text = text.replace("TASKD_", "CELERIS_").replace("taskctl", "celerisctl").replace("taskd", "celeris")

# 念のため: 旧い置き場の跡が 1 つでも残っていたら止める。
leftover = sorted(set(re.findall(r"(?:" + "|".join(forms) + r")\S*", text)))
if leftover:
    sys.stderr.write("the rewritten config still mentions the old home:\n")
    for u in leftover:
        sys.stderr.write(f"  {u}\n")
    sys.exit(3)

sys.stdout.write(text)
PY
}

# ---- mv の計画（順序固定。同一ファイルシステムなので rename で足りる）-------
#
# 「<src>\t<dst>」を 1 行ずつ。`<src>` が無い行は飛ばす。
plan_moves() {
  local f
  # 1. DB（-wal / -shm も一緒に。先に動かす）
  for f in "$OLD_DB_NAME" "$OLD_DB_NAME-wal" "$OLD_DB_NAME-shm"; do
    printf '%s\t%s\n' "$OLD_HOME/$f" "$STATE/celeris.sqlite3${f#"$OLD_DB_NAME"}"
  done
  # 2. リリース（新しいものと同居させる。`.build` / `.cargo-target` も）
  if [ -d "$OLD_HOME/releases" ]; then
    for f in "$OLD_HOME/releases/"* "$OLD_HOME/releases/".*; do
      case "$(basename -- "$f")" in . | .. | '*' | '.*') continue ;; esac
      [ -e "$f" ] || continue
      # release.sh が先に新しい置き場で走っているので `.cargo-target` / `.build` は既にある。衝突する
      # ディレクトリは `<name>-pre-celeris` に逃がす（新しい方は改名後の crate 名で温まっている。旧は人が消す）。
      if [ -e "$STATE/releases/$(basename -- "$f")" ]; then
        printf '%s\t%s\n' "$f" "$STATE/releases/$(basename -- "$f")-pre-celeris"
      else
        printf '%s\t%s\n' "$f" "$STATE/releases/$(basename -- "$f")"
      fi
    done
  fi
  # 3. 状態のディレクトリ
  for f in backups staging workspaces memory claude-accounts codex-accounts; do
    printf '%s\t%s\n' "$OLD_HOME/$f" "$STATE/$f"
  done
  # 4. 道具（venv とキャッシュ）
  for f in ldr paperqa opencode; do
    printf '%s\t%s\n' "$OLD_HOME/$f" "$STATE/tools/$f"
  done
  # 5. 設定と秘密
  for f in api.token gui.password gui.session-secret org.toml providers.d secrets; do
    printf '%s\t%s\n' "$OLD_HOME/$f" "$CONFIG/$f"
  done
  # 6. ログ
  for f in "$OLD_HOME"/*.log; do
    [ -e "$f" ] || continue
    printf '%s\t%s\n' "$f" "$STATE/logs/$(basename -- "$f")"
  done
  # 7. 昔のバックアップ（設定と DB の `.bak-*`）は pre-celeris へ
  for f in "$OLD_HOME"/taskd.toml.bak-* "$OLD_HOME"/$OLD_DB_NAME.bak-*; do
    [ -e "$f" ] || continue
    printf '%s\t%s\n' "$f" "$PRE/$(basename -- "$f")"
  done
  # 8. アカウントの観測値（ADR-0024 D4）。ファイル名にも旧い名前が入っているので改める。
  #    **3 でディレクトリごと動いた後**なので、元も先も状態側で見る（消えても空から始まるだけだが、
  #    cooldown と残量の観測を失わない）。`--rollback` は逆順なのでここが先に戻る。
  for f in claude-accounts codex-accounts; do
    printf '%s\t%s\n' "$STATE/$f/.taskd-usage.json" "$STATE/$f/.celeris-usage.json"
  done
}

# ---- dry run ---------------------------------------------------------------

if [ "$DRY_RUN" = true ]; then
  [ -d "$OLD_HOME" ] || sd_die "no such directory: $OLD_HOME"
  [ -f "$OLD_CONFIG" ] || sd_die "no such file: $OLD_CONFIG"
  printf '=== migrate-to-celeris --dry-run ===\n'
  printf 'old home   : %s\n' "$OLD_HOME"
  printf 'config dir : %s\n' "$CONFIG"
  printf 'state dir  : %s\n' "$STATE"
  printf 'new release: %s\n' "${SHA12:-<none given>}"
  printf 'units      : %s@<old> %s@<old>  ->  celeris@%s celeris-gui@%s\n' \
    "$OLD_UNIT" "$OLD_GUI_UNIT" "${SHA12:-<sha12>}" "${SHA12:-<sha12>}"
  printf '\n--- mv plan (in this order; same filesystem) ---\n'
  n=0
  while IFS=$'\t' read -r src dst; do
    [ -n "$src" ] || continue
    if [ -e "$src" ] || [ -L "$src" ]; then
      printf '  mv %s -> %s\n' "$src" "$dst"
      n=$((n + 1))
    elif [ -e "${src/#$STATE\//$OLD_HOME/}" ]; then
      # 前の段でディレクトリごと動いた後に効く行（`.taskd-usage.json` の改名）。
      printf '  mv %s -> %s   (after the directory move above)\n' "$src" "$dst"
      n=$((n + 1))
    else
      printf '  (skip, missing) %s\n' "$src"
    fi
  done < <(plan_moves)
  printf '  total: %d move(s)\n' "$n"
  printf '\n  keep in %s: taskd.toml (original config), units/ (pre-rename systemd templates)\n' "$PRE"
  printf '  then: rmdir %s if empty\n' "$OLD_HOME"
  printf '\n--- rewritten config (%s -> %s/config.toml) ---\n' "$OLD_CONFIG" "$CONFIG"
  rewrite_config "$OLD_CONFIG" || sd_die "the config still mentions paths this script does not know (see above); nothing was touched"
  printf '\n--- dry run ok: nothing was changed ---\n'
  exit 0
fi

# ---- 戻し（--rollback。ADR-0045 D3 項 3）-----------------------------------

if [ "$MODE" = rollback ]; then
  TS="$(sd_stamp)"
  mkdir -p "$STATE/backups"
  SD_LOG_FILE="$STATE/backups/migrate-$TS.log"
  sd_log "rollback of the celeris migration (log: $SD_LOG_FILE)"
  [ -f "$PRE/taskd.toml" ] || sd_die "no $PRE/taskd.toml — this host was not migrated by this script"

  NEW="$(sd_current_sha)"
  OLD="$(sd_previous_sha)"
  sd_log "current=${NEW:-<none>} previous=${OLD:-<none>}"
  if [ -n "$NEW" ]; then
    for u in "celeris-gui@$NEW" "celeris@$NEW"; do
      if systemctl --user is-active --quiet "$u"; then
        sd_log "systemctl --user stop $u"
        systemctl --user stop "$u" || sd_die "failed to stop $u"
      fi
      systemctl --user disable "$u" 2>/dev/null || true
    done
  fi

  mkdir -p "$OLD_HOME"
  sd_log "moving everything back under $OLD_HOME"
  while IFS=$'\t' read -r src dst; do
    [ -n "$dst" ] || continue
    { [ -e "$dst" ] || [ -L "$dst" ]; } || continue
    mkdir -p "$(dirname "$src")"
    mv -T "$dst" "$src"
    sd_log "mv $dst -> $src"
  done < <(plan_moves | tac)

  cp -p "$PRE/taskd.toml" "$OLD_CONFIG"
  sd_log "restored $OLD_CONFIG"
  rm -f "$CONFIG/config.toml"

  if [ -d "$PRE/units" ]; then
    mkdir -p "$UNIT_DIR"
    for u in "$PRE/units/"*.service; do
      [ -e "$u" ] || continue
      install -m 0644 "$u" "$UNIT_DIR/$(basename -- "$u")"
      sd_log "restored $UNIT_DIR/$(basename -- "$u")"
    done
    systemctl --user daemon-reload
  fi

  [ -n "$OLD" ] || sd_die "no \`previous\` release to start; start the old daemon by hand"
  ln -sfn "releases/$OLD" "$OLD_HOME/current.tmp.$$" && mv -T "$OLD_HOME/current.tmp.$$" "$OLD_HOME/current"
  sd_log "systemctl --user start $OLD_UNIT@$OLD"
  systemctl --user start "$OLD_UNIT@$OLD" || sd_die "failed to start $OLD_UNIT@$OLD"
  sd_wait_http_200 "$SD_PROD_API/api/v1/health" 60 || sd_die "$OLD_UNIT@$OLD did not become healthy within 60s"
  systemctl --user enable "$OLD_UNIT@$OLD" || sd_log "warning: enable $OLD_UNIT@$OLD failed"
  systemctl --user start "$OLD_GUI_UNIT@$OLD" || sd_die "the daemon is back but $OLD_GUI_UNIT@$OLD did not start"
  sd_wait_http_200 "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz" 60 \
    || sd_log "warning: the GUI did not answer /healthz within 60s"
  systemctl --user enable "$OLD_GUI_UNIT@$OLD" || sd_log "warning: enable $OLD_GUI_UNIT@$OLD failed"
  sd_log "rollback complete: back on $OLD under $OLD_HOME"
  exit 0
fi

# ---- 移行本体 --------------------------------------------------------------

TS="$(sd_stamp)"
mkdir -p "$OLD_HOME/backups"
SD_LOG_FILE="$OLD_HOME/backups/migrate-$TS.log"   # 移動の前は旧側に書く（後で $STATE/backups へ一緒に動く）
sd_log "migrate to celeris: new release $SHA12 (log: $SD_LOG_FILE)"

# 前提
[ -d "$OLD_HOME" ] || sd_die "no such directory: $OLD_HOME"
[ -f "$OLD_CONFIG" ] || sd_die "no such file: $OLD_CONFIG"
REL="$STATE/releases/$SHA12"
[ -d "$REL" ] || sd_die "no such release: $REL (run: CELERIS_STATE_DIR=$STATE $SD_DIR/release.sh main)"
[ -x "$REL/bin/celeris" ] || sd_die "missing $REL/bin/celeris"
[ -f "$REL/gui/server.js" ] || sd_die "missing $REL/gui/server.js"
[ "$(sd_json_get "$REL/verify.json" ok 2>/dev/null || echo false)" = true ] \
  || sd_die "verify.json of $SHA12 is not ok — run verify.sh first (there is no --force)"
WANT_SCHEMA="$(sd_json_get "$REL/manifest.json" schema_version)" || sd_die "manifest.json has no schema_version"
[ -d "$STATE" ] || sd_die "$STATE does not exist (release.sh should have created it)"
command -v sqlite3 >/dev/null 2>&1 || sd_die "sqlite3 not found"

# 旧の DB のスキーマ版数（移行の前後で変わらないことを後で確かめる）
BEFORE_SCHEMA="$(sqlite3 "file:$OLD_HOME/$OLD_DB_NAME?mode=ro" 'SELECT COALESCE(MAX(version), 0) FROM schema_migrations;')" \
  || sd_die "cannot read $OLD_HOME/$OLD_DB_NAME"
sd_log "old DB schema_version=$BEFORE_SCHEMA (release wants $WANT_SCHEMA)"

OLD_SHA="$(basename "$(readlink -f "$OLD_HOME/current" 2>/dev/null || echo '')" 2>/dev/null || true)"
case "$OLD_SHA" in "" | . | /) OLD_SHA="" ;; esac
sd_log "old current=${OLD_SHA:-<none>}"

# **先に**設定を書き換えてみる（知らないパスがあればここで止まる。まだ何も動かしていない）。
NEW_CONFIG_TEXT="$(mktemp)"
trap 'rm -f "$NEW_CONFIG_TEXT"' EXIT
rewrite_config "$OLD_CONFIG" >"$NEW_CONFIG_TEXT" \
  || sd_die "the config still mentions paths this script does not know (see above); nothing was moved"
sd_log "config rewrite ok ($(wc -l <"$NEW_CONFIG_TEXT") lines)"

# ---- 0. 行き先が空いていることを、何かを止める前に確かめる ------------------
while IFS=$'\t' read -r src dst; do
  [ -n "$src" ] || continue
  { [ -e "$src" ] || [ -L "$src" ]; } || continue
  case "$src" in "$STATE"/*) continue ;; esac   # 8. の usage.json の改名は動いた後の場所なのでここでは見ない
  if [ -e "$dst" ] || [ -L "$dst" ]; then
    # verify.sh / release.sh が新しい置き場で先に走っていると `backups/`（空）と `staging/`（使い捨て。
    # 検証のたびに作り直す）が残っている。空のディレクトリは消す。staging は中身ごと消す。それ以外は止まる。
    if [ "$dst" = "$STATE/staging" ] && [ -d "$dst" ]; then
      rm -rf -- "$dst"
      sd_log "removed the disposable $dst left by verify.sh"
    elif [ -d "$dst" ] && [ -z "$(ls -A -- "$dst")" ]; then
      rmdir -- "$dst"
      sd_log "removed the empty $dst"
    else
      sd_die "destination already exists: $dst (for $src); nothing was stopped or moved"
    fi
  fi
done < <(plan_moves)
sd_log "all planned destinations are free"

# ---- 1. 旧 unit を止める（SD_STOP_WAIT、既定 300 秒）-----------------------

stop_wait="${SD_STOP_WAIT:-300}"
for u in "$OLD_GUI_UNIT@$OLD_SHA" "$OLD_UNIT@$OLD_SHA"; do
  [ -n "$OLD_SHA" ] || break
  systemctl --user is-active --quiet "$u" || continue
  sd_log "systemctl --user stop $u (waiting up to ${stop_wait}s)"
  systemctl --user stop "$u" &
  stop_pid=$!
  waited=0
  while kill -0 "$stop_pid" 2>/dev/null && [ "$waited" -lt "$stop_wait" ]; do
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$stop_pid" 2>/dev/null; then
    # 実機 2026-09-19: SIGTERM の後 ext4 のジャーナル待ちで 2 分半かかった（ADR-0045 / promote.sh と同じ規則）。
    # API が閉じていれば先へ進む（DB は SQLite のロックが守る）。
    if [ "$(sd_http_status "$SD_PROD_API/api/v1/health")" = 200 ]; then
      sd_die "$u is still serving after ${stop_wait}s; nothing was moved"
    fi
    sd_log "warning: $u is still exiting after ${stop_wait}s but its API is closed; continuing"
  else
    wait "$stop_pid" || true
    sd_log "$u stopped after ${waited}s"
  fi
  systemctl --user disable "$u" 2>/dev/null || true
done
if [ "$(sd_http_status "$SD_PROD_API/api/v1/health")" = 200 ]; then
  sd_die "something is still serving $SD_PROD_API; stop it before migrating (nothing was moved)"
fi

# ---- 2. 移す（同一ファイルシステムなので mv。順序固定）--------------------

mkdir -p "$CONFIG" "$STATE" "$STATE/releases" "$STATE/tools" "$STATE/logs" "$PRE"
chmod 700 "$CONFIG"

MOVED=0
while IFS=$'\t' read -r src dst; do
  [ -n "$src" ] || continue
  { [ -e "$src" ] || [ -L "$src" ]; } || continue
  mkdir -p "$(dirname "$dst")"
  mv -T "$src" "$dst"
  sd_log "mv $src -> $dst"
  MOVED=$((MOVED + 1))
done < <(plan_moves)
sd_log "moved $MOVED entr(ies)"

# 旧 unit のテンプレートを控えてから消す（`--rollback` が戻す）。
mkdir -p "$PRE/units"
for u in $SD_OLD_UNITS; do
  [ -f "$UNIT_DIR/$u" ] || continue
  cp -p "$UNIT_DIR/$u" "$PRE/units/$u"
  sd_log "kept $UNIT_DIR/$u as $PRE/units/$u"
done

# 元の設定を残し、新しい設定を書く。
cp -p "$OLD_CONFIG" "$PRE/taskd.toml"
install -m 0600 "$NEW_CONFIG_TEXT" "$CONFIG/config.toml"
rm -f "$OLD_CONFIG"
sd_log "wrote $CONFIG/config.toml (original kept at $PRE/taskd.toml)"

# このログは旧側に書き始めたので、動いた backups/ の中に追いつかせる。
if [ -f "$OLD_HOME/backups/migrate-$TS.log" ]; then
  mkdir -p "$STATE/backups"
  mv -T "$OLD_HOME/backups/migrate-$TS.log" "$STATE/backups/migrate-$TS.log"
fi
SD_LOG_FILE="$STATE/backups/migrate-$TS.log"

# ---- 3. unit を入れ替える --------------------------------------------------

sd_log "install-units.sh --remove-old"
"$SD_DIR/install-units.sh" --remove-old || sd_die "install-units.sh failed"

# ---- 4. 起こす -------------------------------------------------------------

sd_log "systemctl --user start celeris@$SHA12"
systemctl --user start "celeris@$SHA12" || sd_die "failed to start celeris@$SHA12 (see journalctl --user -u celeris@$SHA12)"
sd_wait_http_200 "$SD_PROD_API/api/v1/health" 90 \
  || sd_die "celeris@$SHA12 did not become healthy within 90s"
HEALTH="$STATE/backups/migrate-$TS.health.json"
sd_http_get "$SD_PROD_API/api/v1/health" >"$HEALTH" || true
GOT_RELEASE="$(sd_json_get "$HEALTH" release || echo '?')"
GOT_SCHEMA="$(sd_json_get "$HEALTH" schema_version || echo '?')"
[ "$GOT_RELEASE" = "$SHA12" ] || sd_die "health says release=$GOT_RELEASE (want $SHA12)"
[ "$GOT_SCHEMA" = "$WANT_SCHEMA" ] || sd_die "health says schema_version=$GOT_SCHEMA (want $WANT_SCHEMA)"
[ "$GOT_SCHEMA" = "$BEFORE_SCHEMA" ] \
  || sd_log "note: schema_version moved $BEFORE_SCHEMA -> $GOT_SCHEMA (the new release migrated the DB)"
sd_log "celeris@$SHA12 healthy: release=$GOT_RELEASE schema_version=$GOT_SCHEMA"
systemctl --user enable "celeris@$SHA12" || sd_log "warning: enable celeris@$SHA12 failed"

sd_log "systemctl --user start celeris-gui@$SHA12"
systemctl --user start "celeris-gui@$SHA12" || sd_die "celeris is the new one, but celeris-gui@$SHA12 did not start"
sd_wait_http_200 "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz" 60 \
  || sd_die "celeris is the new one, but the GUI did not answer /healthz within 60s"
GUI_HEALTH="$STATE/backups/migrate-$TS.gui-healthz.json"
sd_http_get "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz" >"$GUI_HEALTH" || true
GUI_NAME="$(sd_json_get "$GUI_HEALTH" name || echo '?')"
[ "$GUI_NAME" = "celeris-gui" ] || sd_die "/healthz says name=$GUI_NAME (want celeris-gui)"
sd_log "celeris-gui@$SHA12 healthy: name=$GUI_NAME release=$(sd_json_get "$GUI_HEALTH" release || echo '?')"
systemctl --user enable "celeris-gui@$SHA12" || sd_log "warning: enable celeris-gui@$SHA12 failed"

# ---- 5. symlink / promoted.json / 後片付け ---------------------------------

if [ -n "$OLD_SHA" ] && [ -d "$STATE/releases/$OLD_SHA" ]; then
  sd_set_link "$SD_PREVIOUS" "$OLD_SHA"
  sd_log "previous -> releases/$OLD_SHA (pre-rename release; its binary keeps the old name)"
fi
sd_set_link "$SD_CURRENT" "$SHA12"
sd_log "current  -> releases/$SHA12"

{
  printf '{\n'
  printf '  "promoted_at": %s,\n' "$(sd_json_str "$(sd_ts)")"
  printf '  "mode": "migrate",\n'
  if [ -n "$OLD_SHA" ]; then
    printf '  "from": %s\n' "$(sd_json_str "$OLD_SHA")"
  else
    printf '  "from": null\n'
  fi
  printf '}\n'
} >"$REL/promoted.json"
sd_log "promoted.json: $REL/promoted.json (mode=migrate from=${OLD_SHA:-<none>})"

# 旧の家が空になったら消す（ADR-0045 D2: 互換の symlink は作らない）。
if rmdir "$OLD_HOME/backups" 2>/dev/null; then sd_log "rmdir $OLD_HOME/backups"; fi
if rmdir "$OLD_HOME" 2>/dev/null; then
  sd_log "removed $OLD_HOME (it was empty)"
else
  sd_log "note: $OLD_HOME is not empty; left as is:"
  ls -la "$OLD_HOME" >&2 || true
fi

sd_log "migration complete. config=$CONFIG/config.toml state=$STATE current=$SHA12"
sd_log "next promotions use the usual scripts/selfdeploy/promote.sh <sha12>"
