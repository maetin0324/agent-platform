#!/usr/bin/env bash
# 知識の自動メンテナンス（ADR-0047 D4、Phase 62）の venv を用意する。
#
#   scripts/knowledge/setup-langmem.sh
#
# `$CELERIS_STATE_DIR/tools/langmem/.venv`（既定 `~/.local/celeris/tools/langmem/.venv`）に
# venv を作り、`tools/langmem/requirements.txt` を入れる。できた python のパスを
# `[adapters.langmem] command` に書く。
#
# **`cargo test` の一部ではない**（ネットワークに出る。人が 1 回だけ手で叩く。ADR-0009 P-34）。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
requirements="$here/tools/langmem/requirements.txt"
state_dir="${CELERIS_STATE_DIR:-$HOME/.local/celeris}"
venv_dir="$state_dir/tools/langmem/.venv"

if [ ! -f "$requirements" ]; then
  echo "requirements ファイルが見つかりません: $requirements" >&2
  exit 1
fi

python_bin="${CELERIS_LANGMEM_PYTHON:-python3}"
if ! command -v "$python_bin" >/dev/null 2>&1; then
  echo "$python_bin が見つかりません（CELERIS_LANGMEM_PYTHON で指定できます）" >&2
  exit 1
fi

echo "venv:         $venv_dir"
echo "requirements: $requirements"

mkdir -p "$(dirname "$venv_dir")"
"$python_bin" -m venv "$venv_dir"
"$venv_dir/bin/pip" install --upgrade pip
"$venv_dir/bin/pip" install -r "$requirements"

echo
echo "できました。設定に次を書いてください:"
echo
echo "  [adapters.langmem]"
echo "  command = \"$venv_dir/bin/python\""
echo
echo "  [knowledge.langmem]"
echo "  enabled = true"
echo "  provider = \"openai-compatible\"   # または \"anthropic\""
echo "  base_url = \"...\"                 # openai-compatible のとき"
echo "  model = \"...\""
echo
"$venv_dir/bin/python" -c "import langmem; print('langmem', langmem.__version__ if hasattr(langmem, \"__version__\") else 'ok')"
