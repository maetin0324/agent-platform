#!/usr/bin/env python3
"""config.toml の [[genres]] / [[roles]] / [conversation] を消し、to-harnesses の出力と data-analysis / writing を足す（ADR-0046 D3/D7）。"""
import sys, re, subprocess, tomllib
src, dst, celerisctl = sys.argv[1], sys.argv[2], sys.argv[3]
lines = open(src).read().split('\n')
out, skip, in_ml = [], False, False
for l in lines:
    if not in_ml and re.match(r'^\s*\[', l):
        skip = bool(re.match(r'^\s*\[\[(genres|roles)\]\]', l) or re.match(r'^\s*\[conversation\]', l))
    if l.count('"""') % 2 == 1: in_ml = not in_ml
    if not skip: out.append(l)
conv = subprocess.run([celerisctl, 'config', 'to-harnesses', '--config', src], capture_output=True, text=True, check=True).stdout
extra = '''
[[harnesses]]
id = "data-analysis"
description = "計測・実験データの整理、統計、図表の作成"
adapter = "claude-code"
tier = "standard"
capabilities = ["ログや CSV の集計", "統計と検定", "図表の作成（再現できるスクリプト付き）"]
input_artifacts = ["raw data", "repository"]
output_artifacts = ["figures", "tables", "analysis script"]
budget = { max_turns = 60, max_wall_secs = 3600 }
instructions = "あなたは実験データの分析担当。数値は再現できるスクリプトと一緒に出す。図表には軸・単位・条件を必ず書く。元データは書き換えない。"

[[harnesses]]
id = "writing"
description = "論文・報告・文書の執筆と推敲"
adapter = "claude-code"
tier = "standard"
capabilities = ["論文の節の執筆", "推敲と構成の見直し", "引用の整合の確認"]
input_artifacts = ["outline", "sources", "repository"]
output_artifacts = ["draft (Markdown / LaTeX)"]
budget = { max_turns = 60, max_wall_secs = 3600 }
instructions = "あなたは科学技術文書の執筆担当。主張には出典か計測を付ける。既存の原稿の文体と構成に合わせ、変更は節単位で小さくする。"
'''
text = '\n'.join(out).rstrip('\n') + '\n\n# ---- ADR-0046 D3: ハーネス（旧 [[genres]] + [[roles]] から変換。2026-09-20）----\n' + conv + extra
tomllib.loads(text)
open(dst, 'w').write(text)
d = tomllib.loads(text)
print('harnesses:', [h['id'] for h in d.get('harnesses', [])], '| genres left:', len(d.get('genres', [])), '| roles left:', len(d.get('roles', [])))
