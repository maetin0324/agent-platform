#!/usr/bin/env bash
# promote.sh --pre-start 用の汎用フック: `<config>.next` を `<config>` に入れ替える。
# 用途: 新バイナリだけが知る設定節（例: ADR-0053 の `[llm_proxy]`。`deny_unknown_fields` なので旧バイナリは読めない）を、
# 旧が止まった後・新が起きる前に 1 度だけ反映する。元の設定は `<config>.pre-<sha12>` に残す。
# 引数: <sha12> <release dir> <db path> <config path>（promote.sh の契約）。
# 失敗時（.next が無い・新バイナリが設定を読めない）は元の設定に戻して非 0 で終わる（promote.sh が旧 unit を起こし直す）。
set -euo pipefail
SHA12="$1"; REL="$2"; CONFIG="$4"
[ -f "$CONFIG.next" ] || { echo "[swap-config-next] missing $CONFIG.next"; exit 1; }
restore() { echo "[swap-config-next] FAILED — restoring $CONFIG"; [ -f "$CONFIG.pre-$SHA12" ] && cp -p "$CONFIG.pre-$SHA12" "$CONFIG"; }
trap restore ERR
cp -p "$CONFIG" "$CONFIG.pre-$SHA12"
cp -p "$CONFIG.next" "$CONFIG.tmp" && chmod 600 "$CONFIG.tmp" && mv "$CONFIG.tmp" "$CONFIG"
# 新バイナリで設定が読めることだけ確かめる（DB は触らない。--help は設定を読まないので `--mode verify` で起こして即止める）。
if "$REL/bin/celeris" --help 2>/dev/null | grep -q -- '--check-config'; then
  "$REL/bin/celeris" --config "$CONFIG" --check-config
fi
trap - ERR
rm -f "$CONFIG.next"
echo "[swap-config-next] swapped $CONFIG (previous: $CONFIG.pre-$SHA12)"
