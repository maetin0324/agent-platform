#!/bin/sh
# scripts/taskd.sh fixture basic 用の fake ワーカー（docs/adr/0004 D5）。
# stdin に RunRequest の JSON が 1 行来る。task.kind / task.title で分岐する（tests/e2e/tests/plan_scenarios.rs と同じ流儀。
# 実 taskd の crate には依存しない。grep/cut だけで最初に現れる "kind" / "title" を取る）。
# @PLAN_JSON@ は scripts/taskd.sh が絶対パスに置換する。
set -u
RUN=$(mktemp)
cat >"$RUN"
KIND=$(grep -o '"kind":"[a-z]*"' "$RUN" | head -1 | cut -d'"' -f4)
TITLE=$(grep -o '"title":"[^"]*"' "$RUN" | head -1 | cut -d'"' -f4)
rm -f "$RUN"
mkdir -p artifacts

case "$KIND" in
  plan)
    cp "@PLAN_JSON@" artifacts/plan.json
    echo '{"type":"done","summary":"planned","evidence":[]}'
    ;;
  *)
    case "$TITLE" in
      Blocked-C)
        echo '{"type":"question","text":"which environment should this target?"}'
        ;;
      Failed-E)
        echo '{"type":"error","message":"deliberate non-retryable failure","retryable":false}'
        ;;
      *)
        touch artifacts/out.txt
        echo '{"type":"done","summary":"fixture done","evidence":[]}'
        ;;
    esac
    ;;
esac
