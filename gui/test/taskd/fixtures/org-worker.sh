#!/bin/sh
# scripts/taskd.sh fixture org 用の fake ワーカー（Phase G13f-1 の e2e、gui/e2e/g13.spec.ts）。
# LLM は呼ばない。stdin の RunRequest を read-run-request.mjs で解析し、task.title で分岐して
# 「秘書の返事」「成果物つきの調査」「人に聞く（認可の要求）」を決定的に作る。
set -u
HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RUN=$(mktemp)
cat >"$RUN"
eval "$(node "$HERE/read-run-request.mjs" <"$RUN")"
rm -f "$RUN"
mkdir -p artifacts

# 分解（`POST /projects/{id}/plan` が作る `kind = "plan"` のタスク）。偽のプランナーとして、担当（`assignee`）
# 付きの子を 3 件返す（SPEC §3.3「案件は組織の上から入り、分解されて下へ流れる」）。LLM は呼ばない。
if [ "$KIND" = "plan" ]; then
  cat > artifacts/plan.json <<'PLAN'
{
  "tasks": [
    {
      "title": "関連研究を洗い出す",
      "objective": "Pluvio 周辺と隣接分野の関連研究を洗い、候補テーマを挙げる",
      "assignee": "research-survey",
      "acceptance": [{ "text": "候補テーマが 3 つ挙がっている", "check": { "type": "artifact_exists", "name": "report.md" } }]
    },
    {
      "title": "候補テーマの小さな検証を回す",
      "objective": "選んだ候補テーマについて小さな検証コードを書いて回す",
      "assignee": "coding-poc",
      "depends_on": [0],
      "acceptance": [{ "text": "検証が動いた", "check": { "type": "command", "cmd": "true", "expect_exit": 0 } }]
    },
    {
      "title": "結果をまとめて framing する",
      "objective": "検証結果を、論文の framing まで含めてまとめる",
      "assignee": "research-writing",
      "depends_on": [1],
      "acceptance": [{ "text": "まとめが書けている", "check": { "type": "artifact_exists", "name": "report.md" } }]
    }
  ]
}
PLAN
  echo '{"type":"done","summary":"3 件に分解しました","evidence":[]}'
  exit 0
fi

case "$TITLE" in
  対話:*)
    # 対話用タスク（秘書・各担当への話しかけ）。done の summary がそのまま返事になる（ADR-0033 D4）。
    printf '%s\n' '{"type":"done","summary":"## 理解の確認\n\n「Pluvio を基盤に用いた新たな研究テーマの模索、検証」として受け取りました。\n\n## 大まかな方針\n\n1. 関連研究の洗い出し\n2. 候補テーマの比較\n3. 小さな検証\n\n## 最初の途中目標\n\n関連研究を 20 件ほど洗い、候補テーマを 3 つに絞る。","evidence":[]}'
    ;;
  *聞く*)
    # 人に聞く（Question 終端 → /approvals の「認可待ち」に 1 件）。
    echo '{"type":"question","text":"クラスタ pegasus に実験を投入してよいですか？"}'
    ;;
  *)
    # 普通の仕事: 調査結果の文書とリンク集を成果物として登録し、報告のもとになる done を返す。
    printf '# 関連研究の調査結果\n\n- Pluvio の非同期ランタイムに近い先行研究は 3 系統ある。\n- 差分は I/O サーバ側のスケジューリング。\n' > artifacts/report.md
    printf '[{"url":"https://example.invalid/paper-1","title":"Ad-hoc FS の I/O サーバ","cited":true}]' > artifacts/sources.json
    echo '{"type":"artifact","name":"report.md","path":"artifacts/report.md","kind":"markdown"}'
    echo '{"type":"artifact","name":"sources.json","path":"artifacts/sources.json","kind":"json"}'
    echo '{"type":"done","summary":"関連研究を洗い、候補テーマを 3 つに絞りました。この framing なら論文が書けそうです。","evidence":[]}'
    ;;
esac
