# Repository Documentation Maintenance

---
tasks: [01M38FXZZVDNY2VQS2R2ZDWYX0]
---

Repository docs の監査・整理・継続管理。設計は [ADR-0068](adr/0068-knowledge-gc-and-repository-docs-maintenance.md)。
Knowledge GC と別機能であり、組織の知識正本や inbox を変更しません。

## 操作

案件の「文書」から「文書メンテナンス」を開きます。
初回表示は登録 repository の default branch の Git snapshot を読むだけです。
未commit・未追跡ファイルは監査に含めず、そのまま保持します。
classification は title/path/明示した metadata に基づく候補です。
README や docs/ 配下という理由だけで canonical とは判定しません。
「監査結果を保存」で人間向け report を Celeris state 配下の artifact に保存できます。

1. audit の分類・根拠・findings と既存 conventions を確認します。
2. proposal の `actions` に具体的な変更を記入します。空の proposal は監査結果であり実行可能な整理ではありません。
3. 内容を確認して exact plan を承認します。承認は plan 全体の hash に対応します。
4. apply は新しい隔離 worktree にコミットし、検証用 draft task を作ります。そのタスクの変更画面で差分を確認し、既存のレビュー・取り込み手順で進めます。

apply は未承認、dirty な登録 repository、承認後の default branch 更新、snapshot hash 不一致を拒否します。
main を直接変更しません。CLI の apply は worktree と SHA を返す低水準操作で、自動 merge は行いません。
`choose_canonical` は proposal で表現できますが、authority の決定は policy overlay に記録します。
その後の rewrite/delete 等は別途具体的 plan にして承認します。

操作例（CLI は JSON を標準出力へ返します）:

```sh
celerisctl docs-maintenance audit /path/to/repo --reference main
celerisctl docs-maintenance adopt '<project-id>:<repo-name>' policy.json
celerisctl docs-maintenance approve '<project-id>:<repo-name>' plan.json
celerisctl docs-maintenance apply /path/to/repo '<project-id>:<repo-name>' plan.json /path/to/new-worktree
```

GUI/API と CLI で overlay を共有する場合は同じ `CELERIS_STATE_DIR` と上記 repository key を使います。
API は `GET/POST /api/v1/projects/{id}/docs/maintenance`。POST の `op` は
`audit`、`adopt`、`approve`、`apply`。書き込みには既存の管理認証を要求します。

## Policy と Doc Gardener

policy は Celeris state の `repository-docs/` 配下に保存し、repository に commit しません。
`mode` は observe（既定）、conservative、managed。
`categories` は path → category、`authority` は path → 根拠、`generated_sources` は生成物 → 生成元の対応です。
分類には canonical/architecture/reference/decision/active_plan/historical/generated/residue/duplicate/unknown を使います。
agent instructions は AGENTS.md 等の path と authority で記録できます。

```json
{
  "mode": "managed",
  "interval_hours": 168,
  "categories": {"docs/architecture.md": "architecture", "docs/api.md": "generated"},
  "authority": {"README.md": "maintainer-confirmed entry point"},
  "generated_sources": {"docs/api.md": "scripts/generate-api.py"}
}
```

`[docs_maintenance] enabled = true` と managed policy の両方が必要です。
周期は全体設定と repo policy の大きい方、既定168時間。
local Git repository の broken link、巨大文書、duplicate title、stale active plan を検出します。
生成元が宣言されていれば Git 履歴から生成元変更を drift 候補として報告します。
宣言が無い生成物は未確認とし、正しい/古いと断定しません。

候補だけを最大10ページ、設定文字数以内で support task に渡します。
この semantic review は repository を持たない scratch workspace で実行し、成果を artifacts に返します。
具体的変更は上記の承認・apply 経路を通します。通常タスクの adapter/harness と reviewer を再利用します。
実行中タスクと周期を再起動後も確認し、失敗は通常 dispatch を止めません。
候補なしの監査も周期内は繰り返しません。失敗した review は再確認成功とは扱いません。

## Publish と初期版の制約

worker の report/debug/experiment、判断材料、agent context は原則 artifacts に残します。
人間 contributor に継続して価値のある current/canonical 情報のみ、分類と明示的な人間の判断を経て
既存 artifact promotion で docs に公開します。managed は自動公開や削除の包括許可ではありません。

初期版は committed Markdown の heuristic audit です。semantic な正確性は保証せず、リンク fragment の
完全検証や生成コマンドの実行は行いません。外部/remote repository の定期監査は対象外です。
既存 layout の強制変更、AGENTS.md/map の自動 publish、applies_to による change-driven freshness は行いません。
