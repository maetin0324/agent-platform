#!/usr/bin/env bash
# taskd 自動進行ランナー。Phase ごとに新しい claude -p セッションで /goal を回す。
# 使い方: tmux 内で  ./run-phases.sh            （Phase 1〜6）
#                   PHASES="12 10 11" ./run-phases.sh （Phase 12 の残り → 10 → 11）
#                   PHASES="3 4" ./run-phases.sh （一部だけ）
set -uo pipefail
cd "$(dirname "$0")"

PHASES="${PHASES:-1 2 3 4 5 6}"
MAX_RETRIES="${MAX_RETRIES:-3}"
RETRY_SLEEP="${RETRY_SLEEP:-900}"        # 失敗後の待ち（秒）。レート制限回復待ちを兼ねる
LIGHT_MODEL="${LIGHT_MODEL:-sonnet}"      # Phase 1,2,4,6,11 のメインセッション
STRONG_MODEL="${STRONG_MODEL:-opus}"      # Phase 3,5,10,12 のメインセッション。Fable が使えるなら fable
LOGDIR="logs/autorun"; mkdir -p "$LOGDIR"

# implementer は frontmatter で sonnet 固定。auditor は opus 固定。
# それ以外（Explore など model 未指定のもの）の上限をここで決める。
export CLAUDE_CODE_SUBAGENT_MODEL="${CLAUDE_CODE_SUBAGENT_MODEL:-sonnet}"

model_for()    { case "$1" in 3|5|10|12) echo "$STRONG_MODEL";; *) echo "$LIGHT_MODEL";; esac; }
maxturns_for() { case "$1" in 3|5|10|12) echo 60;; 4|11) echo 50;; *) echo 40;; esac; }
phase_done()    { grep -q  "^## Phase $1 — DONE" docs/PROGRESS.md 2>/dev/null; }
phase_stopped() { grep -qE "^## Phase $1 — (BLOCKED|PARTIAL)" docs/PROGRESS.md 2>/dev/null; }

log() { printf '[%s] %s\n' "$(date '+%F %T')" "$*" | tee -a "$LOGDIR/runner.log"; }

for N in $PHASES; do
  if phase_done "$N"; then log "phase $N: already DONE, skip"; continue; fi
  GOAL="$(sed -e "s/__N__/$N/g" -e "s/__MAXTURNS__/$(maxturns_for "$N")/g" docs/GOAL_TEMPLATE.md)"
  MODEL="$(model_for "$N")"
  attempt=0
  while (( attempt < MAX_RETRIES )); do
    attempt=$((attempt + 1))
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
    if phase_stopped "$N"; then log "phase $N: BLOCKED/PARTIAL — stopping runner"; exit 2; fi
    log "phase $N: exited rc=$rc without DONE marker; sleeping ${RETRY_SLEEP}s"
    sleep "$RETRY_SLEEP"
  done
  phase_done "$N" || { log "phase $N: not DONE after $MAX_RETRIES attempts — stopping"; exit 1; }
  sleep 60
done
log "all requested phases DONE"