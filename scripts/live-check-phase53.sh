#!/usr/bin/env bash
# Phase 53（ADR-0044 B1）の実機確認: **走っている run に人がコメントすると run が止まり、
# 次の run の前置きの先頭にそのコメントが載る**。
#
#   bash scripts/live-check-phase53.sh
#
# 本番（~/taskd/、ポート 7710/7700、systemd）には一切触らない:
#   - DB・workspaces・設定は `mktemp -d` の下だけ
#   - API は 127.0.0.1 の**空きポート**（既定 7719。`PORT=... ` で変えられる）
#   - ワーカーは `fake` アダプタ（LLM を呼ばない。ADR-0041 D5 と同じ流儀）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${PORT:-7719}"
BASE="http://127.0.0.1:$PORT/api/v1"
RUN="$(mktemp -d "${TMPDIR:-/tmp}/celeris-phase53-XXXXXX")"
TOKEN="phase53-live-token"

cleanup() {
  if [ -n "${TASKD_PID:-}" ] && kill -0 "$TASKD_PID" 2>/dev/null; then
    kill "$TASKD_PID" 2>/dev/null || true
    sleep 1
    kill -9 "$TASKD_PID" 2>/dev/null || true
  fi
  echo "run dir: $RUN"
}
trap cleanup EXIT

[ "$PORT" != "7710" ] && [ "$PORT" != "7700" ] || { echo "本番のポートは使わない" >&2; exit 2; }

echo "== build =="
(cd "$ROOT" && cargo build -q -p taskd -p taskctl)

printf '%s' "$TOKEN" > "$RUN/token"
chmod 600 "$RUN/token"

# 1 回目は「止められるまで走り続ける」、2 回目以降はすぐ done。前置き（stdin の `run`）を残す。
cat > "$RUN/worker.sh" <<'WORKER'
#!/bin/sh
set -u
n=$(cat "$RUN_DIR/count" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$RUN_DIR/count"
cat > "$RUN_DIR/request-$n.json"
echo '{"type":"progress","msg":"working"}'
if [ "$n" = "1" ]; then
  # 人のコメントで止められるまで走り続ける（SIGTERM で死ぬ）。
  sleep 600
fi
echo '{"type":"comment","body":"2 回目の run: 割り込みを読んだ"}'
echo '{"type":"done","summary":"fake done","evidence":[]}'
WORKER
chmod +x "$RUN/worker.sh"

cat > "$RUN/taskd.toml" <<CONF
db = "taskd.sqlite3"
workspace_root = "workspaces"
tick_ms = 200
max_concurrency = 2
lease_grace_secs = 60
idle_timeout_secs = 900
kill_grace_secs = 1
review_timeout_secs = 30
error_cooldown_secs = 300
retry_backoff_base_secs = 0
retry_backoff_max_secs = 0
max_requeues = 5

[plan]
auto_accept = false

[reviewer]
tier = "standard"

[api]
listen = "127.0.0.1:$PORT"
token_file = "$RUN/token"

[adapters.fake]
command = ["sh", "$RUN/worker.sh"]
env = { RUN_DIR = "$RUN" }

[[providers]]
id = "fake-local"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = 2
model = "fake"
CONF

mkdir -p "$RUN/workspaces"

echo "== start taskd on 127.0.0.1:$PORT (本番ではない) =="
(cd "$RUN" && "$ROOT/target/debug/taskd" --config "$RUN/taskd.toml" --log-format text) \
  > "$RUN/taskd.log" 2>&1 &
TASKD_PID=$!

for _ in $(seq 1 60); do
  if curl -fsS -H "Authorization: Bearer $TOKEN" "$BASE/health" > /dev/null 2>&1; then break; fi
  sleep 0.5
done
curl -fsS -H "Authorization: Bearer $TOKEN" "$BASE/health" | head -c 300; echo

api() { curl -fsS -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' "$@"; }

echo "== 1. 人がタスクを作る（ADR-0044 D1: status は ready）=="
CREATED="$(api -X POST "$BASE/tasks" -d '{
  "title":"実機: 割り込みの確認","objective":"走り続けるワーカーを人のコメントで止める",
  "acceptance":[{"type":"command","cmd":"true"}],
  "labels":["live-check"],"category":"ops","priority":"P1"}')"
echo "$CREATED" | head -c 400; echo
ID="$(printf '%s' "$CREATED" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"
STATUS="$(printf '%s' "$CREATED" | sed -n 's/.*"status":"\([^"]*\)".*/\1/p')"
[ "$STATUS" = "ready" ] || { echo "FAIL: 人が作ったタスクは ready のはず（got $STATUS）" >&2; exit 1; }

echo "== 2. run が走り出すのを待つ =="
for _ in $(seq 1 60); do
  S="$(api "$BASE/tasks/$ID" | sed -n 's/.*"status":"\([^"]*\)".*/\1/p' | head -1)"
  [ "$S" = "running" ] && break
  sleep 0.5
done
[ -f "$RUN/request-1.json" ] || { echo "FAIL: 1 回目の run が始まっていない" >&2; exit 1; }
echo "running（1 回目の run が stdin を読んだ）"

echo "== 3. 人がコメントする（ADR-0044 D2: running → 割り込み）=="
RESULT="$(api -X POST "$BASE/tasks/$ID/comments" -d '{"body":"方針を変えたい。まず設計を書いて"}')"
echo "$RESULT" | head -c 400; echo
printf '%s' "$RESULT" | grep -q '"effect":"interrupted"' || { echo "FAIL: effect が interrupted でない" >&2; exit 1; }

echo "== 4. 走っていた run が止まり、次の run の前置きに割り込みが載る =="
for _ in $(seq 1 120); do
  [ -f "$RUN/request-2.json" ] && break
  sleep 0.5
done
[ -f "$RUN/request-2.json" ] || { echo "FAIL: 2 回目の run が始まらない" >&2; tail -40 "$RUN/taskd.log" >&2; exit 1; }
python3 - "$RUN/request-2.json" <<'PY'
import json, sys
req = json.load(open(sys.argv[1], encoding="utf-8"))
ctx = req["context"]
interrupt = ctx.get("interrupt")
comments = ctx.get("comments", [])
print("interrupt:", interrupt)
print("comments:", json.dumps(comments, ensure_ascii=False))
print("comments_enabled:", ctx.get("comments_enabled"))
assert interrupt == "方針を変えたい。まず設計を書いて", interrupt
assert any(c["author_kind"] == "human" for c in comments), comments
assert ctx.get("comments_enabled") is True
PY

echo "== 5. イベントとコメントとタイムライン =="
api "$BASE/tasks/$ID/events" | python3 -c '
import json,sys
rows=json.load(sys.stdin)["items"]
kinds=[(r["event"]["type"], r["event"].get("reason") or r["event"].get("outcome","")) for r in rows]
for k in kinds: print(k)
assert ("transitioned","comment") in kinds, kinds
assert any(t=="worker_finished" and o=="interrupted: comment" for t,o in kinds), kinds
'
api "$BASE/tasks/$ID/comments" | python3 -c '
import json,sys
items=json.load(sys.stdin)["items"]
print(json.dumps(items, ensure_ascii=False, indent=1)[:800])
assert items[0]["author_kind"]=="human"
assert any(i["author_kind"]=="node" for i in items), "ワーカーの comment 行が残っていない"
'
api "$BASE/tasks/$ID/timeline" | python3 -c '
import json,sys
items=json.load(sys.stdin)["items"]
print("timeline kinds:", [i["kind"] for i in items])
ats=[i["at"] for i in items]
assert ats==sorted(ats), ats
assert "comment" in {i["kind"] for i in items}
'

echo "== 6. 終端での 409、再開（reopen）、編集（PATCH）=="
for _ in $(seq 1 120); do
  S="$(api "$BASE/tasks/$ID" | sed -n 's/.*"status":"\([^"]*\)".*/\1/p' | head -1)"
  case "$S" in done|failed) break ;; esac
  sleep 0.5
done
echo "終端: $S"
# ADR-0044 D1: 終端のタスクは編集できない（409）。
CODE="$(curl -s -o "$RUN/patch-terminal.json" -w '%{http_code}' -X PATCH \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  "$BASE/tasks/$ID" -d '{"title":"x"}')"
echo "PATCH on $S -> $CODE ($(head -c 160 "$RUN/patch-terminal.json"))"
[ "$CODE" = "409" ] || { echo "FAIL: 終端の編集は 409 のはず" >&2; exit 1; }
# ADR-0044 D2: `done`/`failed` は再開できる（attempts は 0）。
api -X POST "$BASE/tasks/$ID/reopen" -d '{}' | python3 -c 'import json,sys; b=json.load(sys.stdin); print(b["from"],"->",b["to"],b["reason"]); assert b["to"]=="ready"'
# 再開したので編集できる（`running` でも受け付ける）。
api -X PATCH "$BASE/tasks/$ID" -d '{"title":"実機: 割り込みの確認（編集済み）","priority":"P0","labels":["live-check","urgent"]}' \
  | python3 -c 'import json,sys; b=json.load(sys.stdin); print(b["fields"]); assert b["task"]["priority"]==30 and b["task"]["labels"]==["live-check","urgent"]'

echo "== 7. フィルタ =="
api "$BASE/tasks?label=live-check&label=urgent&category=ops&priority=P0" \
  | python3 -c 'import json,sys; b=json.load(sys.stdin); print("total:",b["total"], b["items"][0]["priority_label"]); assert b["total"]==1'
api "$BASE/tasks?q=割り込みを読んだ" \
  | python3 -c 'import json,sys; b=json.load(sys.stdin); print("q(コメント本文) total:",b["total"]); assert b["total"]==1'

echo
echo "OK: Phase 53 の実機確認（fake アダプタ、127.0.0.1:$PORT、本番には触れていない）"
