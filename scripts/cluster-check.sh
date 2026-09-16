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

echo "== git リポジトリか（ADR-0019: sync の選び方）"
if [ -n "$workdir" ]; then
  run "cd '$workdir' 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null" | {
    read -r top rest
    if [ -n "${top:-}" ]; then
      echo "   git 管理下です（root: $top）→ sync = \"worktree\" を使えます（追跡ファイルだけを写します）"
      run "cd '$workdir' && echo '   追跡ファイル数: '\$(git ls-files | wc -l); echo '   全体の大きさ  : '\$(du -sh . 2>/dev/null | cut -f1); echo '   .git の大きさ : '\$(du -sh .git 2>/dev/null | cut -f1)"
      echo "   （追跡ファイルにも巨大なものがある場合は worktree_paths で絞ります: 例 worktree_paths = [\"src\", \"Cargo.toml\"]）"
    else
      echo "   git 管理外です → sync = \"rsync\"（大きなディレクトリなら rsync_excludes で減らす）"
    fi
  }
else
  echo "   （remote_workdir を渡すと判定します）"
fi

echo "== 手元とクラスタでファイルが共有されているか"
# 手元で印を書き、リモートから同じ内容が見えるかで判定する（見えれば sync = "none" が使える）。
marker="taskd-shared-fs-probe-$$-$(date +%s)"
probe_dir="${workdir:-$HOME}"
if [ -n "$workdir" ]; then
  # リモートの作業ディレクトリと同じパスが手元にもあるか（共有 FS なら同じパスで見えるのが普通）
  if [ -d "$workdir" ] && [ -w "$workdir" ]; then
    echo "$marker" > "$workdir/.taskd-shared-probe"
    run "grep -q '$marker' '$workdir/.taskd-shared-probe' 2>/dev/null && echo '共有されています（sync = \"none\" を使えます）' || echo '共有されていません（sync = \"rsync\"）'"
    rm -f "$workdir/.taskd-shared-probe"
  else
    echo "   $workdir は手元に無い（または書けない）ので、共有されていません: sync = \"rsync\""
  fi
else
  echo "   remote_workdir を渡すと判定します（手元に同じパスがあるかで見ます）"
fi

echo "== ファイルシステム（手元とリモート）"
printf "   手元    : "; df -PT "$HOME" 2>/dev/null | tail -1
printf "   リモート: "; run "df -PT '${workdir:-\$HOME}' 2>/dev/null | tail -1"

echo "== まとめ: 上の結果を taskd.toml の [[clusters]] に書きます（host / remote_workdir / sync / concurrency）"
