#!/bin/sh
# scripts/celeris.sh fixture multi-account 用の fake ワーカー（docs/adr/0007 D4）。
# celeris 本体の e2e `throttled_account_falls_back_to_the_next_account`
# （tests/e2e/tests/multi_account_scenarios.rs）と同じ挙動を移植する。
# $ACCOUNT は [[providers]].env で acct-a = "a" / acct-b = "b"。
set -u
cat >/dev/null
if [ "$ACCOUNT" = "a" ]; then
  echo '{"type":"error","message":"429 rate limited","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":300}}'
else
  printf '%s' "$ACCOUNT" > account.txt
  echo '{"type":"done","summary":"fallback ok","evidence":[],"usage":{"input_tokens":120,"output_tokens":40}}'
fi
