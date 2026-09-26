#!/usr/bin/env bash
# Exercise the delivery wrapper without running a real release or touching state.
set -euo pipefail

root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/scripts" "$root/bin"
cp "$(dirname "$0")/../prepare.sh" "$root/scripts/prepare.sh"
cat >"$root/bin/timeout" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$3" >>"$TIMEOUT_LOG"
shift 3
"$@"
EOF
cat >"$root/scripts/release.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == 0123456789abcdef0123456789abcdef01234567 ]]
EOF
cat >"$root/scripts/verify.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == 0123456789ab ]]
EOF
chmod +x "$root/bin/timeout"

export TIMEOUT_LOG="$root/timeouts"
PATH="$root/bin:$PATH" bash "$root/scripts/prepare.sh" \
  0123456789abcdef0123456789abcdef01234567 "$root/ok"
[[ "$(cat "$root/timeouts")" == "$(printf '7200\n900')" ]]
[[ "$(cat "$root/ok/result.json")" == '{"ok":true}' ]]

printf '#!/usr/bin/env bash\nexit 1\n' >"$root/scripts/release.sh"
if PATH="$root/bin:$PATH" bash "$root/scripts/prepare.sh" \
  0123456789abcdef0123456789abcdef01234567 "$root/failed"; then
  echo "prepare succeeded despite a failed release" >&2
  exit 1
fi
[[ "$(cat "$root/failed/result.json")" == '{"ok":false}' ]]
