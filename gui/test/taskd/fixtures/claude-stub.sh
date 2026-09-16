#!/bin/sh
# `claude` CLI の代わりに `[adapters.claude_code] command` へ置くスタブ（e2e/g8、scripts/taskd.sh fixture accounts）。
# taskd 側 crates/task-worker/src/claude_account.rs の実装・テスト（login_stub / check_account_ok_path_records_observation）
# と同じ形の出力をする:
#   (a) `claude auth login`: OSC 8 で包んだ認可 URL の行を出し、標準入力から 1 行コードを読む。
#       `good-code` なら $CLAUDE_SECURESTORAGE_CONFIG_DIR/.credentials.json を書いて "Login successful." で exit 0、
#       それ以外は "Login failed: Request failed with status code 400" で exit 1。
#   (b) それ以外（`-p ...`。疎通確認・アカウント確認の実行）: `rate_limit_event`（five_hour 0.42 / seven_day 0.18、
#       resetsAt は起動時刻 + 3600 / + 86400 秒）を 1 行、続けて成功の `result` を 1 行出す。
if [ "$1" = "auth" ] && [ "$2" = "login" ]; then
  printf 'Opening browser to sign in\xe2\x80\xa6\n'
  printf "If the browser didn't open, visit: \033]8;;https://claude.example.invalid/cai/oauth/authorize?code=g8&client_id=x&state=g8\007https://claude.example.invalid/cai/oauth/authorize?code=g8&client_id=x&state=g8\033]8;;\007\n"
  printf 'Paste code here if prompted > '
  read -r code
  if [ "$code" = "good-code" ]; then
    printf '%s' '{}' > "$CLAUDE_SECURESTORAGE_CONFIG_DIR/.credentials.json"
    echo 'Login successful.'
    exit 0
  else
    echo 'Login failed: Request failed with status code 400'
    exit 1
  fi
fi

now=$(date +%s)
five_hour_reset=$((now + 3600))
seven_day_reset=$((now + 86400))
echo "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\",\"resetsAt\":$five_hour_reset,\"rateLimitType\":\"five_hour\",\"overageStatus\":\"rejected\",\"isUsingOverage\":false,\"unifiedWindows\":{\"five_hour\":{\"utilization\":0.42,\"resetsAt\":$five_hour_reset},\"seven_day\":{\"utilization\":0.18,\"resetsAt\":$seven_day_reset}}}}"
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok"}'
