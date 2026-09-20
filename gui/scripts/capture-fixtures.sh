#!/usr/bin/env bash
# 実 celeris（`scripts/celeris.sh fixture basic && scripts/celeris.sh start basic`）から
# `test/fixtures/api/*.json` を採取する（docs/DESIGN.md §10 Phase G1）。
# 型の検証は `test/fixtures/api-types.check.ts` を `pnpm typecheck` に含めることで行う（celeris の crate には依存しない）。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/test/fixtures/api"
BASE="${CELERIS_API_URL:-http://127.0.0.1:7710}/api/v1"
mkdir -p "$OUT"

fetch() { curl -sf "$BASE$1" | python3 -m json.tool; }

fetch "/inbox" > "$OUT/inbox.json"
fetch "/tasks?order=dispatch&limit=500" > "$OUT/tasks.json"

HUMAN_ID=$(fetch "/tasks?q=Human-B" | python3 -c 'import json,sys; print(json.load(sys.stdin)["items"][0]["id"])')
fetch "/tasks/$HUMAN_ID" > "$OUT/task-detail.json"
fetch "/tasks/$HUMAN_ID/events" > "$OUT/task-events.json"

echo "captured fixtures to $OUT"
