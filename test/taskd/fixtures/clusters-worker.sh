#!/bin/sh
# scripts/taskd.sh fixture clusters 用の fake ワーカー（docs/DESIGN.md §10 Phase G7、ADR-0018）。
# クラスタのタスクは pull 済みの手元の写し（cwd）で走る。secret.txt はクラスタ側にしか無いファイルの写し
# （agent-platform の tests/e2e/tests/cluster_scenarios.rs と同じ流儀）。判定はクラスタ側で行われるため
# ここでは push される answer.txt を作るだけでよい。
set -u
cat >/dev/null
mkdir -p artifacts
if [ -f secret.txt ]; then
  cp secret.txt answer.txt
  echo '{"type":"done","summary":"used the cluster file","evidence":[]}'
else
  touch artifacts/out.txt
  echo '{"type":"done","summary":"fixture done","evidence":[]}'
fi
