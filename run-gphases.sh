#!/usr/bin/env bash
# taskd-gui 自動進行ランナー（G フェーズ）。run-phases.sh と同じく、フェーズごとに新しい claude -p セッションで /goal を回す。
# GUI 本体は同じリポジトリの gui/（ADR-0020。既定 $TASKD_REPO/gui）。無ければ docs/gui/ から立ち上げる（docs/gui/bootstrap/README.md）。
# 使い方: tmux 内で  ./run-gphases.sh                  （G0〜G7）
#                   PHASES="G1 G2" ./run-gphases.sh     （一部だけ）
#                   BOOTSTRAP_ONLY=1 ./run-gphases.sh   （前提確認と gui/ の立ち上げだけ）
set -uo pipefail

TASKD_REPO="$(cd "$(dirname "$0")" && pwd)"
export TASKD_REPO
GUI_REPO="${GUI_REPO:-$TASKD_REPO/gui}"   # ADR-0020: taskd と同じリポジトリの gui/

PHASES="${PHASES:-G0 G1 G2 G3 G4 G5 G6 G7}"
MAX_RETRIES="${MAX_RETRIES:-3}"
RETRY_SLEEP="${RETRY_SLEEP:-900}"        # 失敗後の待ち（秒）。レート制限回復待ちを兼ねる
LIGHT_MODEL="${LIGHT_MODEL:-sonnet}"      # G1, G3, G4, G6 のメインセッション
STRONG_MODEL="${STRONG_MODEL:-opus}"      # G0, G2, G5 のメインセッション。Fable が使えるなら fable
BOOTSTRAP_ONLY="${BOOTSTRAP_ONLY:-0}"
SKIP_PREFLIGHT="${SKIP_PREFLIGHT:-0}"     # 前提ツールの確認を飛ばす（立ち上げの試験用）
LOGDIR="$TASKD_REPO/logs/gphases"; mkdir -p "$LOGDIR"   # logs/ は .gitignore 済み

# implementer は frontmatter で sonnet 固定。auditor は opus 固定。
# それ以外（Explore など model 未指定のもの）の上限をここで決める。
export CLAUDE_CODE_SUBAGENT_MODEL="${CLAUDE_CODE_SUBAGENT_MODEL:-sonnet}"
export TASKD_API_URL="${TASKD_API_URL:-http://127.0.0.1:7710}"
export TASKD_GUI_BIND="${TASKD_GUI_BIND:-127.0.0.1:7700}"

# docs/gui/bootstrap/README.md §2 のフェーズ表。
model_for()    { case "$1" in G0|G2|G5) echo "$STRONG_MODEL";; *) echo "$LIGHT_MODEL";; esac; }
maxturns_for() { case "$1" in G0|G2|G5) echo 60;; G4|G6) echo 40;; *) echo 50;; esac; }  # G1/G3/G7 は 50
phase_done()    { grep -q  "^## Phase $1 — DONE" docs/PROGRESS.md 2>/dev/null; }
phase_stopped() { grep -qE "^## Phase $1 — (BLOCKED|PARTIAL)" docs/PROGRESS.md 2>/dev/null; }

log() { printf '[%s] %s\n' "$(date '+%F %T')" "$*" | tee -a "$LOGDIR/runner.log"; }
die() { log "ERROR: $*"; exit "${2:-3}"; }

# $1 >= $2（sort -V による版の比較）
version_ge() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1)" = "$2" ]; }

# docs/gui/bootstrap/README.md §3 の前提。満たさなければ止める（G0 の中で詰まるより先に分かる方がよい）。
preflight() {
  local missing=0 cmd
  for cmd in claude git cargo node pnpm curl; do
    command -v "$cmd" >/dev/null 2>&1 || { log "preflight: '$cmd' not found"; missing=1; }
  done
  if command -v node >/dev/null 2>&1; then
    local nodev; nodev="$(node --version | sed 's/^v//')"
    if ! version_ge "$nodev" "22.22.0"; then
      log "preflight: node $nodev < 22.22.0 (React Router 8 の engines)。Node 24 LTS に更新する（例: nvm install 24）"; missing=1
    elif ! version_ge "$nodev" "24.0.0"; then
      log "preflight: warning: node $nodev は最低版を満たすが、推奨は Node 24 LTS"
    fi
  fi
  if command -v pnpm >/dev/null 2>&1; then
    local pnpmv; pnpmv="$(pnpm --version)"
    case "$pnpmv" in 11.*) ;; *) log "preflight: pnpm $pnpmv は 11.x ではない（npm install -g pnpm@11）"; missing=1;; esac
  fi
  if ! grep -q "^## Phase 9 — DONE" "$TASKD_REPO/docs/PROGRESS.md" 2>/dev/null; then
    log "preflight: taskd の Phase 9（API 層）が DONE になっていない"; missing=1
  fi
  if [ ! -f "$TASKD_REPO/docs/api/v1/api-v1.schema.json" ]; then
    log "preflight: $TASKD_REPO/docs/api/v1/api-v1.schema.json が無い（G0 が BLOCKED になる）"; missing=1
  fi
  (( missing == 0 )) || die "preflight failed (SKIP_PREFLIGHT=1 で飛ばせる)"
  log "preflight: ok (node $(node --version), pnpm $(pnpm --version))"
}

# docs/gui/bootstrap/README.md §1 の対応表でコピーし、初期コミットを作る。既に立ち上がっていれば何もしない。
bootstrap() {
  if [ -f "$GUI_REPO/package.json" ] || [ -f "$GUI_REPO/docs/PROGRESS.md" ]; then
    log "bootstrap: $GUI_REPO is already set up, skip"
    return
  fi
  if [ -d "$GUI_REPO" ] && [ -n "$(ls -A "$GUI_REPO" 2>/dev/null)" ]; then
    die "bootstrap: $GUI_REPO exists, is not empty and has no package.json; refusing to overwrite"
  fi
  local src="$TASKD_REPO/docs/gui"
  log "bootstrap: creating $GUI_REPO from $src"
  mkdir -p "$GUI_REPO/docs/adr" "$GUI_REPO/.claude/agents" || die "bootstrap: mkdir failed"
  cp "$src/DESIGN-GUI.md"               "$GUI_REPO/docs/DESIGN.md" &&
  cp "$src/api.md"                      "$GUI_REPO/docs/taskd-api-v1.md" &&
  cp "$src"/adr/*.md                    "$GUI_REPO/docs/adr/" &&
  cp "$src/taskd-proposals.md"          "$GUI_REPO/docs/taskd-proposals.md" &&
  cp "$src/bootstrap/CLAUDE.md"         "$GUI_REPO/CLAUDE.md" &&
  cp "$src/bootstrap/GOAL_TEMPLATE.md"  "$GUI_REPO/docs/GOAL_TEMPLATE.md" &&
  cp "$src/bootstrap/PROGRESS.md"       "$GUI_REPO/docs/PROGRESS.md" &&
  cp "$src"/bootstrap/agents/*.md       "$GUI_REPO/.claude/agents/" || die "bootstrap: copy failed"

  # docs/DESIGN.md のリンクをコピー先に合わせる: api.md → taskd-api-v1.md、bootstrap/README.md は taskd リポジトリ側を指す文に。
  sed -i -E \
    -e 's#\[`bootstrap/README\.md`\]\(bootstrap/README\.md\)#taskd リポジトリの `docs/gui/bootstrap/README.md`#g' \
    -e 's#\[([^]]*)\]\(bootstrap/README\.md\)#\1（taskd リポジトリの docs/gui/bootstrap/README.md）#g' \
    -e 's#(^|[^/])bootstrap/README\.md#\1taskd リポジトリの docs/gui/bootstrap/README.md#g' \
    -e 's#\bapi\.md\b#taskd-api-v1.md#g' \
    "$GUI_REPO/docs/DESIGN.md" || die "bootstrap: link rewrite failed"

  cat > "$GUI_REPO/docs/taskd-requests.md" <<'EOF'
# taskd への依頼

GUI 側で回避せず、taskd の API に足りない・仕様（`docs/taskd-api-v1.md`）と違う点を書く。書いたら `docs/PROGRESS.md` に `## Phase G<N> — BLOCKED` を書いて止まる。

## 未対応

## 対応済み
EOF
  printf '%s\n' node_modules/ build/ .run/ dist/ test-results/ playwright-report/ .react-router/ > "$GUI_REPO/.gitignore"

  # ADR-0020: gui/ が taskd と同じリポジトリの中なら、そこにコミットする（git init はしない）。外を指すなら従来どおり独立したリポジトリにする。
  case "$GUI_REPO/" in
    "$TASKD_REPO"/*) ( cd "$TASKD_REPO" && git add -A "$GUI_REPO" && git commit -q -m "bootstrap: gui design, api spec, rules" ) \
                       || die "bootstrap: initial commit failed"
                     log "bootstrap: initial commit $(git -C "$TASKD_REPO" rev-parse --short HEAD)" ;;
    *)               ( cd "$GUI_REPO" && git init -q -b main && git add -A && git commit -q -m "bootstrap: design, api spec, rules" ) \
                       || die "bootstrap: initial commit failed"
                     log "bootstrap: initial commit $(git -C "$GUI_REPO" rev-parse --short HEAD)" ;;
  esac
}

if [ "$SKIP_PREFLIGHT" = "1" ]; then log "preflight: skipped"; else preflight; fi
bootstrap
if [ "$BOOTSTRAP_ONLY" = "1" ]; then log "BOOTSTRAP_ONLY=1; not running phases"; exit 0; fi

cd "$GUI_REPO" || die "cannot cd to $GUI_REPO"
[ -z "$(git status --porcelain .)" ] || log "warning: $GUI_REPO has uncommitted changes"

for N in $PHASES; do
  if phase_done "$N"; then log "phase $N: already DONE, skip"; continue; fi
  # ADR-0020 D4: taskd が正の API 仕様を GUI 側の写しに反映してからフェーズに入る。
  "$TASKD_REPO/scripts/sync-gui-docs.sh" >> "$LOGDIR/runner.log" 2>&1 || log "warning: sync-gui-docs.sh failed"
  GOAL="$(sed -e "s/__N__/${N#G}/g" -e "s/__MAXTURNS__/$(maxturns_for "$N")/g" docs/GOAL_TEMPLATE.md)"
  MODEL="$(model_for "$N")"
  attempt=0
  while (( attempt < MAX_RETRIES )); do
    attempt=$((attempt + 1))
    # 前回の taskd（fake ワーカー + [api]）が残っていたら止める。G0 の前は scripts/taskd.sh が無いので何もしない。
    if [ -x scripts/taskd.sh ]; then scripts/taskd.sh stop dev >/dev/null 2>&1 || true; fi
    out="$LOGDIR/phase-$N-attempt-$attempt.jsonl"
    log "phase $N: attempt $attempt, model=$MODEL"
    if (( attempt == 1 )); then
      claude -p "/goal $GOAL" --model "$MODEL" --permission-mode auto \
        --output-format stream-json --verbose > "$out" 2>&1
    else
      # 前回セッションを継続。未達の goal は復元されるが、クリアされていた場合に備えて同じ条件を再設定する
      claude -p "/goal $GOAL" --continue --model "$MODEL" --permission-mode auto \
        --output-format stream-json --verbose > "$out" 2>&1
    fi
    rc=$?
    if phase_done "$N";    then log "phase $N: DONE (rc=$rc)"; break; fi
    if phase_stopped "$N"; then log "phase $N: BLOCKED/PARTIAL — stopping runner (docs/taskd-requests.md を確認)"; exit 2; fi
    log "phase $N: exited rc=$rc without DONE marker; sleeping ${RETRY_SLEEP}s"
    sleep "$RETRY_SLEEP"
  done
  phase_done "$N" || { log "phase $N: not DONE after $MAX_RETRIES attempts — stopping"; exit 1; }
  sleep 60
done
log "all requested phases DONE"
