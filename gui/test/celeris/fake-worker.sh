#!/bin/sh
# 既定の fake ワーカー（celeris の [adapters.fake] の既定と同じ挙動）: 要求を読み捨て、progress 1 行と done を返す。
# フェーズごとの fixture はこのファイルを .run/<name>/fake-worker.sh に置き換えて使う（G1 以降）。
set -u
cat >/dev/null
echo '{"type":"progress","msg":"fake worker"}'
echo '{"type":"done","summary":"fake done","evidence":[]}'
