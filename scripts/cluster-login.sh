#!/usr/bin/env bash
# クラスタへの多重接続（ControlMaster）を張る。2 要素認証はここで人が通す（ADR-0018 D2 / D6）。
# 使い方: scripts/cluster-login.sh <host>   （<host> は ~/.ssh/config の Host 名）
set -uo pipefail

host="${1:-}"
if [ -z "$host" ]; then
  echo "使い方: $0 <host>   （~/.ssh/config の Host 名。雛形は config/ssh-config.example）" >&2
  exit 2
fi

mkdir -p ~/.ssh/cm && chmod 700 ~/.ssh/cm

if ssh -o BatchMode=yes -O check "$host" 2>/dev/null; then
  echo "既に接続があります（$host）。張り直す場合は: ssh -O exit $host"
  exit 0
fi

echo "== $host に接続します。2 要素認証の入力を求められたら応答してください。"
# -M -N: master のみ（コマンドは実行しない）。-f: 認証の後にバックグラウンドへ。
if ! ssh -M -N -f "$host"; then
  echo "接続できませんでした。~/.ssh/config の Host 設定（config/ssh-config.example）を確認してください。" >&2
  exit 1
fi

if ssh -o BatchMode=yes -O check "$host" 2>/dev/null; then
  echo "== 接続を張りました（$host）。ControlPersist の間は taskd がこの接続を借ります。"
  echo "   確認: scripts/cluster-check.sh $host"
else
  echo "接続は張れましたが -O check が失敗しました。ControlPath の設定を確認してください。" >&2
  exit 1
fi
