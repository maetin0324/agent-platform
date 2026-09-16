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
      Slow-F)
        # G3 受け入れ条件 7（追尾）: 2 秒おきに progress を 5 回出す（合計 10 秒）。taskd 独自プロトコルの行
        # なので stdout.jsonl 上は claude-code/codex のどちらでもなく raw 表示になる。
        i=1
        while [ "$i" -le 5 ]; do
          echo "{\"type\":\"progress\",\"msg\":\"tick $i\"}"
          sleep 2
          i=$((i + 1))
        done
        touch artifacts/out.txt
        echo '{"type":"done","summary":"slow done","evidence":[]}'
        ;;
      Slow-H)
        # G4 受け入れ条件 4（in_flight 表示）: 20 秒 sleep してから done。fixture 本体（--until-idle）には含めない
        # （含めると fixture 構築が 20 秒延び、かつ in_flight を観測できないまま終わる。docs/adr/0007 D6）。
        sleep 20
        touch artifacts/out.txt
        echo '{"type":"done","summary":"slow-h done","evidence":[]}'
        ;;
      Artifacts-G)
        # G3 受け入れ条件 3/4/5（成果物ビューア）: Markdown（信用できないスクリプトを含む）・JSON・PNG。
        # `ArtifactProduced` は taskd が自動検出せず、ワーカーが `{"type":"artifact",...}` を明示的に送る必要がある
        # （docs/taskd-api-v1.md、crates/task-worker/src/protocol.rs の `WorkerMessage::Artifact`）。
        printf '# note\n\nsome text.\n\n<script>alert(1)</script>\n' > artifacts/note.md
        printf '{"hello":"world"}' > artifacts/data.json
        printf 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=' | base64 -d > artifacts/image.png
        echo '{"type":"artifact","name":"note.md","path":"artifacts/note.md","kind":"markdown"}'
        echo '{"type":"artifact","name":"data.json","path":"artifacts/data.json","kind":"json"}'
        echo '{"type":"artifact","name":"image.png","path":"artifacts/image.png","kind":"image"}'
        echo '{"type":"done","summary":"artifacts done","evidence":[]}'
        ;;
      *)
        touch artifacts/out.txt
        echo '{"type":"done","summary":"fixture done","evidence":[]}'
        ;;
    esac
    ;;
esac
