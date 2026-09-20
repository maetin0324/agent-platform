#!/bin/sh
# scripts/celeris.sh fixture delegation 用の fake ワーカー（docs/DESIGN.md §10 Phase G7、ADR-0016）。
# stdin に RunRequest の JSON が来る。中身は read-run-request.mjs で解析する（grep で JSON を読まない。G7-U3）。
#
# Lead-Delegator の最初の run では `context.children` が空なので `delegate` を出して done する
# （dispatcher は delegate の後も呼び出し元に done を期待する。crates/task-dispatch/src/dispatcher.rs
# のテスト fixture アダプタと同じ流儀）。子が全て終端になった後の集約 run では `context.children` が
# 非空になるので、そこで artifacts/summary.md を書いて done する。委譲された子（role=implementer）は
# このスクリプトの既定分岐（touch + done）で終わる。
set -u
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RUN=$(mktemp)
cat >"$RUN"
eval "$(node "$HERE/read-run-request.mjs" <"$RUN")"
rm -f "$RUN"
mkdir -p artifacts

case "$TITLE" in
  Lead-Delegator)
    if [ "$CHILDREN" -gt 0 ]; then
      printf '# summary\n\nboth delegated children finished (%s).\n' "$CHILD_TITLES" > artifacts/summary.md
      echo '{"type":"done","summary":"aggregated the delegated children","evidence":[]}'
    else
      echo '{"type":"delegate","tasks":[{"title":"Delegated-Child-1","objective":"first delegated unit of work","acceptance":[{"text":"exits 0","check":{"type":"command","cmd":"true","expect_exit":0}}],"role":"implementer"},{"title":"Delegated-Child-2","objective":"second delegated unit of work","acceptance":[{"text":"exits 0","check":{"type":"command","cmd":"true","expect_exit":0}}],"role":"implementer"}]}'
      echo '{"type":"done","summary":"delegated 2 subtasks","evidence":[]}'
    fi
    ;;
  *)
    touch artifacts/out.txt
    echo '{"type":"done","summary":"fixture done","evidence":[]}'
    ;;
esac
