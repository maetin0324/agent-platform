#!/usr/bin/env bash
# scripts/selfdeploy/lib.sh — release / verify / promote / rollback / status / migrate で共有する道具
# （ADR-0040 D1/D2、ADR-0045 D2）。単体では何もしない。`source` して使う。
#
# 置き場（ADR-0045 D2。XDG 流に設定と状態を分ける）:
#   $CELERIS_CONFIG_DIR（既定 ~/.config/celeris） 設定と秘密: config.toml org.toml providers.d/ api.token
#                                                 gui.password gui.session-secret secrets/
#   $CELERIS_STATE_DIR （既定 ~/.local/celeris）  状態: celeris.sqlite3 releases/ backups/ staging/
#                                                 workspaces/ memory/ logs/ tools/ current previous
#
# 触ってよい場所（ADR-0040 D1、安全規則）:
#   $SD_RELEASES / $SD_STAGING / $SD_BACKUPS と、昇格のときだけ $SD_CURRENT / $SD_PREVIOUS の symlink。
# 触ってはいけない場所: $SD_CONFIG（編集しない。`migrate-to-celeris.sh` だけが移行のときに 1 度書く）、
#   $SD_DB（`sqlite3 .backup` と `mode=ro` で読むだけ）、本番のポート 127.0.0.1:7710 と 0.0.0.0:7700（bind しない）。

# shellcheck shell=bash

# ---- 場所 ------------------------------------------------------------------

CELERIS_CONFIG_DIR="${CELERIS_CONFIG_DIR:-$HOME/.config/celeris}"
CELERIS_STATE_DIR="${CELERIS_STATE_DIR:-$HOME/.local/celeris}"
# `SD_REPO` を要るのは `release.sh`（作業ツリーを生やす）だけ。`verify.sh` / `promote.sh` /
# `rollback.sh` / `status.sh` は上の 2 つの下だけを見るので、作業チェックアウトが無くても動く。
# ADR-0040 D6（Phase 48）: `release.sh` がこの一式を `<release>/scripts/` に写すので、
# `promote.sh` はリリースの中から（`POST /releases/{sha12}/promote` 経由で）起きることがある。
# そのときも `lib.sh` は `dirname "${BASH_SOURCE[0]}"` で自分の隣を読むだけなので、場所に依らない。
# Phase 83 / G36（ADR-0041 追記）: `verify.sh` の検査 4b（gui-e2e）だけ、`$SD_REPO/gui`（devDependencies
# 込みで `pnpm install` 済みの方。release の `gui/` は `pnpm install --prod` で Playwright が無い）を
# **あれば使う**。無くても検査 4b は「未インストール」で false になるだけでクラッシュしない
# （＝この段落の「作業チェックアウトが無くても動く」は変わらない）。
SD_REPO="${SD_REPO:-$HOME/workspace/agent-platform}"

SD_RELEASES="$CELERIS_STATE_DIR/releases"
SD_BUILD_ROOT="$SD_RELEASES/.build"
SD_CARGO_TARGET="$SD_RELEASES/.cargo-target"
SD_STAGING="$CELERIS_STATE_DIR/staging"
SD_BACKUPS="$CELERIS_STATE_DIR/backups"
SD_CURRENT="$CELERIS_STATE_DIR/current"
SD_PREVIOUS="$CELERIS_STATE_DIR/previous"
# 設定ファイル。`CELERIS_CONFIG` が立っていればそれが勝つ（移行のあいだ `verify.sh` に
# **旧い名前の**設定ファイルを読ませるため。ADR-0045 D3 の段取り 1）。
SD_CONFIG="${CELERIS_CONFIG:-$CELERIS_CONFIG_DIR/config.toml}"
SD_API_TOKEN_FILE="$CELERIS_CONFIG_DIR/api.token"

# DB は**設定ファイルの `db =` から読む**（ADR-0045 D2）。`CELERIS_DB` が立っていればそれが勝つ
# （移行のあいだ `verify.sh` に旧 DB を見せるため）。設定が無い／`db` を書いていないときは
# celeris 本体と同じ既定（$CELERIS_STATE_DIR/celeris.sqlite3）。
sd_db_from_config() {
  local v
  [ -f "$SD_CONFIG" ] || { printf '%s' "$CELERIS_STATE_DIR/celeris.sqlite3"; return 0; }
  # 最初の節（`[...]`）より前の、トップレベルの `db = "..."` だけを見る。
  v="$(sed -n '/^[[:space:]]*\[/q; s/^[[:space:]]*db[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' "$SD_CONFIG" | head -n 1)"
  # ADR-0064 D1（Phase 110a）: `[db]` テーブル形式（`path = "..."`）も見る。`[db]` 見出しから次の見出しまで。
  if [ -z "$v" ]; then
    v="$(sed -n '/^[[:space:]]*\[db\][[:space:]]*$/,/^[[:space:]]*\[/{ s/^[[:space:]]*path[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p; }' "$SD_CONFIG" | head -n 1)"
  fi
  if [ -z "$v" ]; then printf '%s' "$CELERIS_STATE_DIR/celeris.sqlite3"; return 0; fi
  case "$v" in
    "~/"*) printf '%s/%s' "$HOME" "${v#"~/"}" ;;
    /*) printf '%s' "$v" ;;
    *) printf '%s/%s' "$(dirname "$SD_CONFIG")" "$v" ;;   # 相対は設定ファイルのディレクトリ基準
  esac
}
SD_DB="${CELERIS_DB:-$(sd_db_from_config)}"

# 本番のポート。ここに bind してはいけない（読むだけ）。
SD_PROD_API="http://127.0.0.1:7710"
SD_PROD_GUI_PORT=7700
# staging のポート（D3）。
SD_STAGING_API_PORT=7711
SD_STAGING_GUI_PORT=7701
SD_STAGING_OLD_API_PORT=7712

# corepack の pnpm（このホストでは PATH に無い）。
SD_PNPM_SHIM_DIR="${SD_PNPM_SHIM_DIR:-/usr/lib/node_modules/corepack/shims}"

# ---- ログ ------------------------------------------------------------------

SD_LOG_FILE="${SD_LOG_FILE:-}"

sd_ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }
sd_stamp() { date +%Y%m%d-%H%M%S; }

sd_log() {
  local line
  line="$(sd_ts) [${SD_PROG:-selfdeploy}] $*"
  printf '%s\n' "$line" >&2
  if [ -n "$SD_LOG_FILE" ]; then printf '%s\n' "$line" >>"$SD_LOG_FILE"; fi
}

sd_die() {
  sd_log "ERROR: $*"
  exit 1
}

# ---- JSON（python3 を優先。無ければ jq） -----------------------------------

if command -v python3 >/dev/null 2>&1; then
  SD_JSON_TOOL=python3
elif command -v jq >/dev/null 2>&1; then
  SD_JSON_TOOL=jq
else
  SD_JSON_TOOL=none
fi

sd_require_json_tool() {
  [ "$SD_JSON_TOOL" = none ] && sd_die "need python3 or jq for JSON handling"
  return 0
}

# 文字列を JSON の文字列リテラルにする（前後の " 込み）。
sd_json_str() {
  case "$SD_JSON_TOOL" in
    python3) python3 -c 'import json,sys; sys.stdout.write(json.dumps(sys.argv[1], ensure_ascii=False))' "$1" ;;
    jq) jq -Rn --arg s "$1" '$s' ;;
    *) sd_die "need python3 or jq" ;;
  esac
}

# ファイルの JSON から dotted path の値を取り出す（`a.b.0.c`）。
# 見つかれば標準出力に（スカラーは素の値、配列/オブジェクトは JSON）、見つからなければ exit 1。
sd_json_get() {
  local file="$1" path="$2"
  [ -f "$file" ] || return 1
  case "$SD_JSON_TOOL" in
    python3)
      python3 - "$file" "$path" <<'PY'
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        cur = json.load(fh)
except Exception:
    sys.exit(1)
for part in sys.argv[2].split("."):
    if part == "":
        continue
    if isinstance(cur, list):
        try:
            cur = cur[int(part)]
        except Exception:
            sys.exit(1)
    elif isinstance(cur, dict) and part in cur:
        cur = cur[part]
    else:
        sys.exit(1)
if cur is None:
    sys.stdout.write("null")
elif isinstance(cur, bool):
    sys.stdout.write("true" if cur else "false")
elif isinstance(cur, (int, float, str)):
    sys.stdout.write(str(cur))
else:
    sys.stdout.write(json.dumps(cur, ensure_ascii=False))
PY
      ;;
    jq)
      local filter=".$path"
      [ "$path" = "" ] && filter="."
      jq -e -r "$filter" "$file" 2>/dev/null
      ;;
    *) sd_die "need python3 or jq" ;;
  esac
}

# JSON ファイルが妥当かどうか（本文は捨てる）。
sd_json_valid() {
  case "$SD_JSON_TOOL" in
    python3) python3 -c 'import json,sys; json.load(open(sys.argv[1],encoding="utf-8"))' "$1" >/dev/null 2>&1 ;;
    jq) jq -e . "$1" >/dev/null 2>&1 ;;
    *) return 1 ;;
  esac
}

# TSV（1 行目が `name:type name:type ...` のヘッダ。type は s/i/f/b）→ JSON の配列。
sd_tsv_to_json() {
  local file="$1"
  case "$SD_JSON_TOOL" in
    python3)
      python3 - "$file" <<'PY'
import json, sys
rows = []
with open(sys.argv[1], encoding="utf-8") as fh:
    lines = fh.read().split("\n")
spec = [f.split(":") for f in lines[0].split() if f]
for line in lines[1:]:
    if not line.strip():
        continue
    cells = line.split("\t")
    row = {}
    for i, (name, kind) in enumerate(spec):
        raw = cells[i] if i < len(cells) else ""
        if kind == "i":
            row[name] = int(raw or 0)
        elif kind == "f":
            row[name] = round(float(raw or 0), 3)
        elif kind == "b":
            row[name] = raw == "true"
        else:
            row[name] = raw
    rows.append(row)
sys.stdout.write(json.dumps(rows, ensure_ascii=False, indent=2))
PY
      ;;
    jq)
      local hdr spec
      hdr="$(head -n 1 "$file")"
      spec="$(printf '%s' "$hdr" | tr ' ' '\n' | jq -Rn '[inputs | select(length>0) | split(":") | {n: .[0], t: .[1]}]')"
      tail -n +2 "$file" | jq -Rn --argjson spec "$spec" '
        [ inputs | select(length > 0) | split("\t") as $r
          | reduce range(0; ($spec | length)) as $i ({};
              ($spec[$i].n) as $n | ($spec[$i].t) as $t | ($r[$i] // "") as $v
              | .[$n] = (if $t == "i" then ($v | tonumber | floor)
                         elif $t == "f" then ($v | tonumber)
                         elif $t == "b" then ($v == "true")
                         else $v end)) ]'
      ;;
    *) sd_die "need python3 or jq" ;;
  esac
}

# 1 行 1 要素のファイル → JSON の文字列配列（空行は落とす）。1 回の呼び出しで済ませる
# （`sd_json_str` をパスの数だけ呼ぶと python3 の起動が数百回になる）。
sd_lines_to_json_array() {
  local file="$1"
  [ -s "$file" ] || { printf '[]'; return 0; }
  case "$SD_JSON_TOOL" in
    python3)
      python3 - "$file" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
    items = [line for line in fh.read().split("\n") if line.strip()]
sys.stdout.write(json.dumps(items, ensure_ascii=False))
PY
      ;;
    jq) jq -R -s 'split("\n") | map(select(length > 0))' "$file" ;;
    *) sd_die "need python3 or jq" ;;
  esac
}

# ---- git -------------------------------------------------------------------

sd_sha12() {
  local ref="$1"
  git -C "$SD_REPO" rev-parse --short=12 "${ref}^{commit}" 2>/dev/null \
    || sd_die "cannot resolve git ref: $ref (repo $SD_REPO)"
}

sd_sha_full() {
  git -C "$SD_REPO" rev-parse "${1}^{commit}" 2>/dev/null || sd_die "cannot resolve git ref: $1"
}

# ---- HTTP ------------------------------------------------------------------

# `sd_http_get <url> [token-file]` — 本文を標準出力に。HTTP status を返す代わりに、
# 非 2xx なら exit 1（本文は捨てない）。
sd_http_get() {
  local url="$1" token_file="${2:-}" code body tmp
  tmp="$(mktemp)"
  if [ -n "$token_file" ] && [ -r "$token_file" ]; then
    code="$(curl -sS -o "$tmp" -w '%{http_code}' -m 30 -H "Authorization: Bearer $(tr -d '\r\n' <"$token_file")" "$url" || echo 000)"
  else
    code="$(curl -sS -o "$tmp" -w '%{http_code}' -m 30 "$url" || echo 000)"
  fi
  body="$(cat "$tmp")"
  rm -f "$tmp"
  printf '%s' "$body"
  case "$code" in
    2*) return 0 ;;
    *) return 1 ;;
  esac
}

# `sd_http_post <url> <token-file> <json-body>` — 本文を標準出力に。非 2xx なら exit 1
# （本文は捨てない。`problem+json` の説明をそのまま呼び出し側のログに出せる）。
# 本番には使わない（`verify.sh` の staging だけ。lib.sh の先頭の安全規則）。
sd_http_post() {
  local url="$1" token_file="${2:-}" body="${3:-}" code out tmp
  tmp="$(mktemp)"
  if [ -n "$token_file" ] && [ -r "$token_file" ]; then
    code="$(curl -sS -o "$tmp" -w '%{http_code}' -m 30 -X POST \
      -H "Authorization: Bearer $(tr -d '\r\n' <"$token_file")" \
      -H 'Content-Type: application/json' --data-binary "$body" "$url" || echo 000)"
  else
    code="$(curl -sS -o "$tmp" -w '%{http_code}' -m 30 -X POST \
      -H 'Content-Type: application/json' --data-binary "$body" "$url" || echo 000)"
  fi
  out="$(cat "$tmp")"
  rm -f "$tmp"
  printf '%s' "$out"
  case "$code" in
    2*) return 0 ;;
    *) return 1 ;;
  esac
}

# `sd_http_status <url> [token-file]` — status コードだけ。
sd_http_status() {
  local url="$1" token_file="${2:-}"
  if [ -n "$token_file" ] && [ -r "$token_file" ]; then
    curl -sS -o /dev/null -w '%{http_code}' -m 30 -H "Authorization: Bearer $(tr -d '\r\n' <"$token_file")" "$url" 2>/dev/null || echo 000
  else
    curl -sS -o /dev/null -w '%{http_code}' -m 30 "$url" 2>/dev/null || echo 000
  fi
}

# `sd_http_status_follow <url>` — リダイレクトを追って**最後の**status を返す。GUI の画面用
# （`/` は `/tasks` などへ 302 する。ADR-0040 D3 の「200」はリダイレクトの先のこと）。
sd_http_status_follow() {
  curl -sSL --max-redirs 5 -o /dev/null -w '%{http_code}' -m 30 "$1" 2>/dev/null || echo 000
}

# `sd_wait_http_200 <url> <timeout-secs> [token-file]`
sd_wait_http_200() {
  local url="$1" timeout="$2" token_file="${3:-}" waited=0 code
  while [ "$waited" -lt "$timeout" ]; do
    code="$(sd_http_status "$url" "$token_file")"
    [ "$code" = 200 ] && return 0
    sleep 1
    waited=$((waited + 1))
  done
  return 1
}

# ---- ポート ----------------------------------------------------------------

# 誰かが LISTEN していれば 1（塞がっている）。
sd_port_free() {
  local port="$1"
  if ss -ltn "sport = :$port" 2>/dev/null | tail -n +2 | grep -q .; then
    return 1
  fi
  return 0
}

sd_require_port_free() {
  sd_port_free "$1" || sd_die "port $1 is already in use (need it for $2)"
}

# ---- リリース --------------------------------------------------------------

sd_release_dir() { printf '%s/%s' "$SD_RELEASES" "$1"; }

# `current` / `previous` が指している sha12（無ければ空文字）。
sd_link_target() {
  local link="$1" dest
  [ -L "$link" ] || return 0
  dest="$(readlink -f "$link" 2>/dev/null)" || return 0
  [ -n "$dest" ] || return 0
  basename "$dest"
}

sd_current_sha() { sd_link_target "$SD_CURRENT"; }
sd_previous_sha() { sd_link_target "$SD_PREVIOUS"; }

# `sd_set_link <link> <sha12>` — symlink を張り替える（同一 dir 内で mv するので原子的）。
sd_set_link() {
  local link="$1" sha="$2" tmp
  tmp="$link.tmp.$$"
  ln -sfn "releases/$sha" "$tmp"
  mv -T "$tmp" "$link"
}

# `crates/task-core/src/store.rs` から `pub const SCHEMA_VERSION: u32 = N;` を読む。
sd_schema_version_of_tree() {
  # `local a="$1" b="$a"` は全部の語を先に展開してから代入するので `$a` はまだ無い（set -u で落ちる）。
  # 参照する変数は別の `local` に分ける。
  local tree="$1"
  local file n
  file="$tree/crates/task-core/src/store.rs"
  [ -f "$file" ] || { printf '0'; return 1; }
  n="$(sed -n 's/^[[:space:]]*pub const SCHEMA_VERSION: u32 = \([0-9][0-9]*\);.*$/\1/p' "$file" | head -n 1)"
  [ -n "$n" ] || { printf '0'; return 1; }
  printf '%s' "$n"
}

# 本番 DB のスキーマ版数（read-only。`schema_migrations` の最大 `version`）。
sd_db_schema_version() {
  local db="${1:-$SD_DB}" v
  v="$(sqlite3 "file:${db}?mode=ro" 'SELECT COALESCE(MAX(version), 0) FROM schema_migrations;' 2>/dev/null)" || return 1
  printf '%s' "$v"
}

sd_mkdirs() {
  mkdir -p "$SD_RELEASES" "$SD_BUILD_ROOT" "$SD_BACKUPS"
}

# pnpm（corepack の shim）を PATH に入れる。
sd_use_pnpm() {
  case ":$PATH:" in
    *":$SD_PNPM_SHIM_DIR:"*) ;;
    *) PATH="$SD_PNPM_SHIM_DIR:$PATH"; export PATH ;;
  esac
  command -v pnpm >/dev/null 2>&1 || sd_die "pnpm not found (looked in $SD_PNPM_SHIM_DIR)"
}

# ---- 直列化（flock。ADR-0041 D2） -----------------------------------------

# `verify.sh` は `$SD_STAGING` とポート 7711 / 7701 / 7712 を固定で使うので、2 本同時に走ると
# 必ず壊れる（スナップショットを作り直す側が、もう一方の DB を消す）。`release.sh` は同じ sha の
# worktree競合に加え、異なるshaでも共有Cargo成果物が競合するため全体を直列化する。
# どちらも `flock` の**ファイル記述子**で直列化する
# （プロセスが死ねばカーネルが外すので、残骸のロックファイルは無害）。
#
# EX_TEMPFAIL（sysexits.h の 75）= 「いまは無理。あとでもう一度」。呼び出し側（人・ワーカー・CI）が
# 「壊れた（1）」と「混んでいる（75）」を区別できるようにする。
SD_EX_TEMPFAIL=75

# `verify.sh` がロックを待つ上限（秒）。既定 1800（ADR-0041 D2）。
SD_VERIFY_LOCK_WAIT="${SD_VERIFY_LOCK_WAIT:-1800}"

# `sd_lock_or_tempfail <fd> <lockfile> <wait-secs> <what>`
#   `<wait-secs>` が 0 なら `flock -n`（待たずに諦める）。取れなければ exit 75。
#   取れたロックは**このシェルが終わるまで**（fd が閉じるまで）持ち続ける。
#
# 実機 2026-09-21 09:19: `cargo test` の中で起こされた `podman info`（container の runtime probe）が
# 応答せずに残り、release.sh から**継承した lock の fd** を握ったまま PID 1 の子になった。release.sh 自体は
# 終わっているのに次の release.sh が 1800 秒待って exit 75 になった。対策は 2 つ:
#   1. 子プロセスに lock の fd を継がせない（release.sh の run_step 等で `8>&- 9>&-`）。
#   2. それでも「持ち主が selfdeploy のスクリプトではない」ロックは**漏れた fd**なので、持ち主を記録して
#      ロックファイルを `<file>.leaked-<日時>` に退け、新しい inode で取り直す（`sd_lock_leaked_holders`）。
#      生きている release.sh / verify.sh が 1 つでも fd を持っていれば退けない（本物の直列化はそのまま）。

# `sd_lock_leaked_holders <lockfile>`: そのファイルを開いている全プロセスの `pid cmd` を 1 行ずつ出す。
# 1 つでも selfdeploy のスクリプト（bash …/selfdeploy/*.sh）が含まれていれば何も出さない（＝本物の持ち主がいる）。
sd_lock_leaked_holders() {
  local file="$1" abs pid target cmd holders="" script_alive=false ppid
  abs="$(readlink -f "$file" 2>/dev/null)" || return 0
  for pid in $(ls /proc 2>/dev/null | grep -E '^[0-9]+$'); do
    # 自分自身（スクリプト本体・この関数を動かすサブシェル）と、その直接の子（`flock` など）は持ち主として数えない。
    [ "$pid" = "$$" ] || [ "$pid" = "$BASHPID" ] && continue
    ppid="$(awk '/^PPid:/{print $2}' "/proc/$pid/status" 2>/dev/null)"
    [ "$ppid" = "$$" ] || [ "$ppid" = "$BASHPID" ] && continue
    for target in /proc/"$pid"/fd/*; do
      [ "$(readlink "$target" 2>/dev/null)" = "$abs" ] || continue
      cmd="$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | cut -c1-120)"
      case "$cmd" in *selfdeploy/*.sh*) script_alive=true ;; esac
      holders="${holders}${pid} ${cmd}
"
      break
    done
  done
  [ "$script_alive" = true ] && return 0
  printf '%s' "$holders"
}

sd_lock_or_tempfail() {
  local fd="$1" file="$2" wait="$3" what="$4" leaked
  command -v flock >/dev/null 2>&1 \
    || sd_die "flock not found (util-linux); refusing to run without serialization"
  mkdir -p "$(dirname "$file")"
  : >>"$file"
  eval "exec $fd>>\"\$file\""
  if ! flock -n "$fd"; then
    leaked="$(sd_lock_leaked_holders "$file")"
    if [ -n "$leaked" ]; then
      sd_log "lock $file is held only by processes that are not selfdeploy scripts (the script that took it has exited; the fd leaked to a child):"
      printf '%s' "$leaked" | while IFS= read -r line; do [ -n "$line" ] && sd_log "  leaked holder: $line"; done
      local aside="$file.leaked-$(date -u +%Y%m%dT%H%M%SZ)"
      mv "$file" "$aside" && sd_log "moved the leaked lock aside to $aside; taking a fresh lock"
      : >>"$file"
      eval "exec $fd>>\"\$file\""
    fi
  fi
  if [ "$wait" = 0 ]; then
    if flock -n "$fd"; then return 0; fi
    sd_log "another $what is already running (lock: $file); nothing was changed"
    exit "$SD_EX_TEMPFAIL"
  fi
  if flock -w "$wait" "$fd"; then return 0; fi
  sd_log "another $what is running and did not finish within ${wait}s (lock: $file)"
  sd_log "exit $SD_EX_TEMPFAIL (EX_TEMPFAIL) = busy, not broken; try again later or raise SD_VERIFY_LOCK_WAIT"
  exit "$SD_EX_TEMPFAIL"
}

# ---- 安全に関わる変更（ADR-0041 D4） ---------------------------------------

# 昇格の前に人へ**赤く**見せるパスの一覧。**ここが唯一の定義**（`release.sh` が `changes.json` の
# `sensitive` に書き、`GET /releases` と GUI はその結果を読むだけ。判断を 2 か所に置かない）。
# リポジトリ相対のパスに対する**前方一致**（ディレクトリは末尾 `/`、ファイルはそのまま）。
#
# 選んだ理由: これらを変えたリリースは「昇格の仕組みそのもの」「本番の設定」「エージェントへの
# 指示文」を変える。壊れると次の昇格で直せるとは限らないので、人が必ず中身を見てから押す。
SD_SENSITIVE_PATTERNS=(
  "scripts/selfdeploy/"
  "deploy/"
  "crates/celeris/src/instance.rs"
  "crates/celeris/src/releases.rs"
  "crates/task-api/src/releases.rs"
  "crates/task-core/migrations/"
  "CLAUDE.md"
  "gui/CLAUDE.md"
  ".claude/"
  "config/"
  "docs/adr/0040-"
  "docs/adr/0041-"
)

# `sd_is_sensitive <repo-relative-path>` — 上のどれかに前方一致すれば 0。
sd_is_sensitive() {
  local path="$1" pat
  for pat in "${SD_SENSITIVE_PATTERNS[@]}"; do
    case "$path" in "$pat"*) return 0 ;; esac
  done
  return 1
}

# ---- 旧 celeris の pid（Phase 119 D3） ------------------------------------
#
# 実機 2026-09-24: `promote.sh` の停止→起動が、drain したまま終了しない前回の昇格の残骸
# （`celeris@0e20e1b2a058` 等、複数の draining インスタンス）の中から**最も古い無関係な pid**に
# SIGTERM を送り、300 秒待って「old celeris is still serving after 300s」で失敗した。本来は
# `current` symlink が指す release の unit の MainPID を対象にすべき（ADR-0040 追記）。
#
# 以下 3 つは `promote.sh`/`status.sh` からだけでなく `scripts/selfdeploy/tests/` からも直接
# source して呼べるよう、`/proc` 相当のディレクトリと自分の pid を引数で受け取れるようにしてある
# （既定はどちらも本物。本物の `/proc`・本物の `systemctl` に触れるのは実引数を省略したときだけ）。

# `sd_find_old_celeris_pid [proc_dir] [self_pid]` — 本番の celeris の pid を**設定パス
# （`$SD_CONFIG`）まで含めた完全一致**で `/proc` を走査して探す。`pgrep -f` は自分のシェルの
# コマンドラインにも当たるので使わない（過去に踏んだ）。
#
# **これは systemd 管理化にない場合の最終手段**（初回の移行。`sd_resolve_old_daemon_pid` を使うこと）。
# 複数の draining インスタンスが同じ `--config` を持つ状態では、この関数は「最初に見つかった」
# プロセスを返すだけなので、どれが `current` かは分からない。
sd_find_old_celeris_pid() {
  local proc_dir="${1:-/proc}" self_pid="${2:-$$}"
  local pid_dir pid argv i matched skip
  for pid_dir in "$proc_dir"/[0-9]*; do
    pid="${pid_dir##*/}"
    if [ "$pid" = "$self_pid" ]; then continue; fi
    if [ ! -r "$pid_dir/cmdline" ]; then continue; fi
    argv=()
    mapfile -d '' -t argv <"$pid_dir/cmdline" 2>/dev/null || continue
    if [ "${#argv[@]}" -lt 3 ]; then continue; fi
    # argv[0] が `celeris` そのものでなければ対象外（`grep`、`bash -c`、`sh -c` はここで落ちる）。
    if [ "$(basename -- "${argv[0]}")" != celeris ]; then continue; fi
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

# `sd_resolve_old_daemon_pid <old_sha> [proc_dir] [self_pid]` — Phase 119 D3。
#
# `<old_sha>`（= `current` symlink が指す release）が systemd 管理下で生きているなら、**その unit の
# MainPID だけ**を対象にする（実機で確認した事故の再発防止: 複数の draining インスタンスが同じ
# `--config` を持つときに、`sd_find_old_celeris_pid` の /proc 走査は最初に見つかった無関係な pid
# （最も古い残骸であることが多い）を拾ってしまう）。systemd がまだこの release を知らない
# （初回の移行、または unit がまだ無い）ときだけ `sd_find_old_celeris_pid` にフォールバックする。
sd_resolve_old_daemon_pid() {
  local old_sha="$1" proc_dir="${2:-/proc}" self_pid="${3:-$$}" pid
  if [ -n "$old_sha" ] && systemctl --user is-active --quiet "celeris@$old_sha" 2>/dev/null; then
    pid="$(systemctl --user show -p MainPID --value "celeris@$old_sha" 2>/dev/null || echo 0)"
    if [ -n "$pid" ] && [ "$pid" != 0 ]; then
      printf '%s' "$pid"
      return 0
    fi
  fi
  sd_find_old_celeris_pid "$proc_dir" "$self_pid"
}

# `sd_list_stale_celeris_units <new_sha> <old_sha>` — Phase 119 D3。
#
# `<new_sha>`/`<old_sha>` 以外に**まだ active な** `celeris@*.service` の sha12 を 1 行ずつ返す
# （前回までの昇格で drain したまま終了しなかった残骸。ADR-0040 追記「drain 後にプロセスが終了しない」）。
# systemd が無い環境では何も返さない（クラッシュしない）。
sd_list_stale_celeris_units() {
  local new_sha="$1" old_sha="$2" sha
  command -v systemctl >/dev/null 2>&1 || return 0
  systemctl --user list-units --type=service --state=active --no-legend --plain 'celeris@*.service' \
    2>/dev/null \
    | awk '{print $1}' \
    | sed -n 's/^celeris@\(.*\)\.service$/\1/p' \
    | while IFS= read -r sha; do
        [ -n "$sha" ] || continue
        [ "$sha" = "$new_sha" ] && continue
        [ "$sha" = "$old_sha" ] && continue
        printf '%s\n' "$sha"
      done
}
