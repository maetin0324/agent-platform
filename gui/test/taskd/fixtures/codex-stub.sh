#!/bin/sh
# `codex` CLI の代わりに `[adapters.codex] command` へ置くスタブ（e2e/g9、scripts/taskd.sh fixture accounts）。
# taskd 側 crates/task-worker/src/codex_account.rs の実装・テスト（login_stub / check_account_codex_ok_path_records_observation）
# と同じ形の出力をする:
#   (a) `codex login --device-auth`: ANSI で色付けした認可 URL の行と `ABCD-EFGHI` 形式の一回限りのコードを出し、
#       標準入力は使わず、約 1 秒後に $CODEX_HOME/auth.json を書いて exit 0（ADR-0025 D5）。
#   (b) それ以外（`exec --json ...`。疎通確認・アカウント確認の実行）: `token_count`（primary 30% / 300 分、
#       secondary 12% / 10080 分）を 1 行、続けて `turn.completed` を 1 行出す。
if [ "$1" = "login" ] && [ "$2" = "--device-auth" ]; then
  printf '\033[1mVisit\033[0m \033[36mhttps://auth.openai.com/codex/device\033[0m and enter the code below:\n'
  printf '\033[32mABCD-EFGHI\033[0m\n'
  sleep 1
  printf '%s' '{}' > "$CODEX_HOME/auth.json"
  exit 0
fi

echo '{"type":"token_count","rate_limits":{"primary":{"used_percent":30.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":12.0,"window_minutes":10080,"resets_in_seconds":432000}}}'
echo '{"type":"turn.completed"}'
