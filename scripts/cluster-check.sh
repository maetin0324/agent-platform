#!/usr/bin/env bash
# クラスタ側の前提を調べる（読み取りのみ。ADR-0018 D6）。多重接続が張られていることが前提。
# 使い方: scripts/cluster-check.sh <host> [remote_workdir]
set -uo pipefail

host="${1:-}"
workdir="${2:-}"
if [ -z "$host" ]; then
  echo "使い方: $0 <host> [remote_workdir]" >&2
  exit 2
fi

echo "== 多重接続"
if ssh -o BatchMode=yes -O check "$host" 2>&1; then
  echo "   ok（taskd はこの接続を借りられます）"
else
  echo "   ありません。先に scripts/cluster-login.sh $host を実行してください。" >&2
  exit 1
fi

run() { ssh -o BatchMode=yes "$host" -- "$@" 2>&1; }

echo "== ホスト"
run 'uname -sr; echo "shell: $SHELL"; echo "home: $HOME"'
echo "== 道具"
run 'for c in rsync python3 git bash; do printf "%s: " "$c"; command -v "$c" || echo "(無し)"; done'
echo "== 作業ディレクトリ"
if [ -n "$workdir" ]; then
  run "test -d '$workdir' && echo 'あり' || echo '無し（作成が必要）'; test -w '$workdir' 2>/dev/null && echo '書き込み可' || echo '書き込み不可または未作成'"
  run "df -PT '$workdir' 2>/dev/null | tail -1"
else
  echo "   （remote_workdir を渡すと、存在・書き込み可否・ファイルシステムを確認します）"
fi

echo "== 手元とクラスタでファイルが共有されているか"
probe="$(mktemp "${TMPDIR:-/tmp}/taskd-probe-XXXXXX")"
marker="taskd-shared-fs-probe-$$-$(date +%s)"
echo "$marker" > "$probe"
if [ -n "$workdir" ]; then
  shared_probe="$workdir/.taskd-shared-probe"
  cp "$probe" "$shared_probe" 2>/dev/null &&
    { run "grep -q '$marker' '$shared_probe' && echo '共有されています（sync = \"none\" を使えます）' || echo '共有されていません（sync = \"rsync\"）'";
      rm -f "$shared_probe"; } ||
    echo "   remote_workdir に手元から書けないので判定できません（別のファイルシステムの可能性が高い: sync = \"rsync\"）"
else
  home_probe="$HOME/.taskd-shared-probe"
  cp "$probe" "$home_probe"
  run "grep -q '$marker' '$home_probe' 2>/dev/null && echo 'ホームが共有されています（sync = \"none\" を検討できます）' || echo 'ホームは共有されていません（sync = \"rsync\"）'"
  rm -f "$home_probe"
fi
rm -f "$probe"

echo "== まとめ: 上の結果を taskd.toml の [[clusters]] に書きます（host / remote_workdir / sync / concurrency）"
