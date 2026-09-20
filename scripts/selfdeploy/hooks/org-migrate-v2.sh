#!/usr/bin/env bash
# promote.sh --pre-start 用: ADR-0046 への移行。デーモンが止まっている間に
#   1. config.toml / org.toml を用意済みの *.next に差し替える（元は *.pre-org-v2 に残す）
#   2. celerisctl org migrate-v2（DB と memory/ を新しい木へ）
# 失敗したら設定を元に戻し、migrate-v2 --rollback を試みて非 0 で抜ける（DB の復元と旧 unit の再起動は promote.sh がやる）。
# 引数: <sha12> <release dir> <db path> <config path>
set -euo pipefail
REL="$2"; DB="$3"; CONFIG="$4"; DIR="$(dirname "$CONFIG")"
[ -f "$CONFIG.next" ] && [ -f "$DIR/org.toml.next" ] || { echo "[org-migrate-v2] missing $CONFIG.next or $DIR/org.toml.next"; exit 1; }
restore() {
  echo "[org-migrate-v2] FAILED — restoring config and org.toml"
  "$REL/bin/celerisctl" --db "$DB" org migrate-v2 --config "$CONFIG" --rollback || true
  [ -f "$CONFIG.pre-org-v2" ] && cp -p "$CONFIG.pre-org-v2" "$CONFIG"
  [ -f "$DIR/org.toml.pre-org-v2" ] && cp -p "$DIR/org.toml.pre-org-v2" "$DIR/org.toml"
}
trap restore ERR
cp -p "$CONFIG" "$CONFIG.pre-org-v2"
cp -p "$DIR/org.toml" "$DIR/org.toml.pre-org-v2"
cp -p "$DIR/org.toml.next" "$DIR/org.toml"
sed "s#^org_include = .*#org_include = \"$DIR/org.toml\"#" "$CONFIG.next" > "$CONFIG.tmp" && chmod 600 "$CONFIG.tmp" && mv "$CONFIG.tmp" "$CONFIG"
echo "[org-migrate-v2] config and org.toml swapped; dry-run"
"$REL/bin/celerisctl" --db "$DB" org migrate-v2 --config "$CONFIG" --dry-run
echo "[org-migrate-v2] apply"
"$REL/bin/celerisctl" --db "$DB" org migrate-v2 --config "$CONFIG"
sqlite3 "file:$DB?mode=ro" "select id, coalesce(parent_id,'-'), kind from org_nodes order by position, id"
trap - ERR
rm -f "$CONFIG.next" "$DIR/org.toml.next"
echo "[org-migrate-v2] done"
