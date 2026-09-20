# 知識ベース（Knowledge Base / Memory Layer）

- 出所: ADR-0047（D1〜D3・D5 が Phase 61、D4 = LangMem は Phase 62）
- 関連: ADR-0046 D1（profile の `knowledge` マウント）、ADR-0033 D3（ノードの手帳）、ADR-0044 D7（文書は git が正本）、
  ADR-0043 D3（コンテナのマウント）、ADR-0048（Console）

## 1. 原則

| 原則 | 意味 |
|---|---|
| Local-first | 正本は**手元の Markdown**。DB も外部サービスも正本ではない |
| Human-readable | 人がエディタで直接読み書きできる。front matter は最小の YAML もどき |
| Agent-shared | 組織のどのノードからも**同じ 1 本の道具**（`celerisctl knowledge`）で読める |
| Self-maintaining | 抽出・整理は決定的なトリガで起きる（Phase 62） |
| User-owned | 人の物。git なのでいつでも戻せる。削除も人が決める |
| Implementation-independent | 検索の実装（今は grep）・LangMem・将来の埋め込み索引は**差し替えられる**。ファイルの形は変えない |

## 2. 置き場と形（D1）

正本は **`[knowledge] root`**（既定 `~/knowledge`）。**バージョン管理下**に置く（`celerisctl knowledge init` が
用意する。変更は 1 件ごとに 1 コミット）。

```
~/knowledge/
  user/                 profile.md / expertise.md / preferences.md / goals.md
  environment/          clusters/<name>.md, servers/<name>.md, tools/<name>.md
  projects/<slug>/      design.md / decisions.md / status.md …（slug は ADR-0044 D7 の案件 slug）
  experience/           YYYY/MM/<slug>.md（問題・解法・結果・採らなかった案と理由）
  _inbox/               抽出された候補。**索引にも検索にも出ない**（人が accept / reject する）
  index.json            派生物。再生成できる（バージョン管理には入れない）
  README.md             この構成の説明（init が書く）
```

**1 ファイル = 1 トピック**。先頭に front matter を付ける:

```markdown
---
title: pegasus の使い方
tags: [hpc, cluster, pegasus]
scope: environment
sources: [human, "task:01J…"]
created: 2026-09-20
updated: 2026-09-20
confidence: high
---

# pegasus の使い方

…
```

| 鍵 | 値 |
|---|---|
| `title` | ページの題名。無ければ本文の最初の `# `、それも無ければファイル名 |
| `tags` | 検索の第一の手掛かり（`[a, b]` か `- a` の並び） |
| `scope` | `user` / `environment` / `project:<slug>` / `experience`。無ければ置き場から決まる |
| `sources` | `task:<id>` / `message:<id>` / `human` / `url:<…>`。**`record` では必須** |
| `created` / `updated` | `YYYY-MM-DD` か RFC 3339 |
| `confidence` | `high` / `medium` / `low` |
| `path` | **`_inbox/` の候補にだけ意味がある**: 取り込む先の KB 相対パス |

`index.json` は `{generated_at, items: [{path, title, tags, scope, sources, updated, confidence}]}`。
**派生物**なので、壊れても `celerisctl knowledge reindex` で作り直せる。`_inbox/` は入らない。

パスは常に **KB の根からの相対**。`..`・絶対パス・`.md` 以外は、CLI でも API でも通らない。

## 3. 道具（D3）— エージェントはここだけを使う

```bash
celerisctl knowledge init    [--root …]                    # 1 回だけ。冪等
celerisctl knowledge search <語> [--scope …] [--limit N] [--json]
celerisctl knowledge get    <path> [--json]
celerisctl knowledge record --title … --scope … [--tags a,b] --source task:<id> \
                            [--confidence high|medium|low] [--path <取り込み先>] < body.md
celerisctl knowledge reindex
```

- **このサブコマンドだけは DB を開かない**。KB のファイルを直接読み書きするので、コンテナの中でも
  KB さえ同じパスにマウントされていれば動く。
- 根の決め方: `--root` > `CELERIS_KNOWLEDGE_ROOT` > `[knowledge] root` > `~/knowledge`。
  設定ファイルが読めない環境（コンテナの中）では黙って次の候補に落ちる。
- `search` の順位（ADR-0047 D3）: **`tags` の一致語数 → `title`（とパス）の一致語数 → 本文の全文一致 →
  `updated` の新しさ → パスの辞書順**。語は空白で切り、大文字小文字は区別しない部分一致。
  本文は `git grep -i -l -F`（なければ `grep -ril`）。`_inbox/` は出ない。
- `record` は**候補**を `_inbox/<ts>-<slug>.md` に書くだけ。**正本には直接書かない**。
  - `--source` が 1 件も無ければエラー（出典の無い知識は入れない）
  - 秘密（`sk-…` / `ghp_…` / `-----BEGIN` など）が本文・題名・出典に含まれていれば**拒否**（D4 の検査を先に入れてある）
  - 人が GUI の「知識」画面で accept / reject する

## 4. 誰が何を読むか（D2 のマウント）

実効マウント = **組織の実効 profile の `knowledge`（根→葉の和。ADR-0046 D1）＋ 案件の `projects/<slug>`（自動）
＋ タスクの明示**。Phase 61 の時点では組織側がまだ `knowledge` を持たないので、`[knowledge] default_mounts`
（既定 `["kb:user", "kb:environment"]`）が全ノードの既定として効く。

| kind | 意味 | 書き方 |
|---|---|---|
| `kb` | KB の一部 | `{ kind = "kb", scope = "environment/clusters" }` / `"kb:environment/clusters"` |
| `repo` | 案件のリポジトリの `docs/` | `{ kind = "repo", name = "pluvio", docs = "docs" }` / `"repo:pluvio"` |
| `dir` | 任意のローカルディレクトリ（読み取り） | `{ kind = "dir", path = "/opt/share/notes" }` / `"dir:/opt/share/notes"` |
| `memory` | そのノードの手帳（`memory/<node>/notes.md`） | `{ kind = "memory" }` / `"memory"` |

**前置きには索引だけが出る**（`path` / `title` / `tags`。全体で最大 200 件）。本文は入らない。

```
## 知識 (knowledge base — 索引だけ。本文は道具で読む)
あなたが読める知識: `kb:user`、`kb:environment`。
知識は `celerisctl knowledge search <語> [--scope …]` で探し、`celerisctl knowledge get <path>` で読む。
将来も使える事実を得たら `celerisctl knowledge record --title … --scope … --source task:<このタスクの id>`
で候補に入れる（一時的な情報・雑談・推測は入れない。出典を付ける）。候補は人が確認してから正本に入る。
### kb:user
- `user/profile.md` — 人のプロフィール（user）
### kb:environment
- `environment/clusters/pegasus.md` — pegasus の使い方（environment、cluster、pegasus）
```

マウントが無い run・KB がまだ無い環境では、**この節ごと出ない**（前置きは Phase 60 までと 1 バイトも変わらない）。

コンテナで走る run（ADR-0043 D3）には、KB の根を**同じパスに読み取り専用**で、`_inbox` だけ**書き込み可**で
マウントする。だから `search` / `get` はそのまま動き、`record` も候補を書ける。正本はコンテナからは書けない。

## 5. 人が使う経路（D5）

- GUI の「知識」画面: scope 別のツリー、検索、ページの描画・編集・履歴、`_inbox` の一覧（accept / reject、出典へのリンク）
- API: `GET /knowledge/tree`、`GET /knowledge/page`、`PUT /knowledge/page`（管理系）、`GET /knowledge/inbox`、
  `POST /knowledge/inbox/{id}/{accept,reject}`（管理系）。仕様は `docs/gui/api.md` §3.98〜3.103
- エディタで直接書いてもよい（**正本は作業ツリーのファイル**なので、未コミットの編集もそのまま GUI に見える）。
  そのときは `celerisctl knowledge reindex` を 1 回呼ぶか、GUI をもう一度開けば索引が作り直される

書き込みは **1 件 1 コミット**。author / committer は `Celeris (human) <celeris@local>`
（`record` が作る候補だけ `Celeris (knowledge) <celeris@local>`）。衝突は `etag`（中身の sha256）で見る。

## 6. 立ち上げ（人が 1 回だけやること）

```bash
celerisctl knowledge init                  # ~/knowledge を用意する（冪等）
$EDITOR ~/.config/celeris/config.toml      # [knowledge] を書く（既定でよければ省略できる）
$EDITOR ~/knowledge/user/profile.md        # 雛形を埋める
$EDITOR ~/knowledge/environment/clusters/pegasus.md
celerisctl knowledge reindex
```

`init` が置く雛形は**空欄と書き方だけ**（`confidence: low`）。`environment/clusters/{pegasus,sirius,fern03}.md` は
「接続 / 作業場所 / ジョブ / 環境」の見出しだけがあるので、`docs/workspace.md` と `config.toml` の `[[clusters]]` に
既に書いてあることを人が書き写し、`confidence: high` にする。**celeris は雛形を勝手に埋めない**（出典の無い
知識を作らないため）。

## 7. まだやらないこと

- Knowledge Graph、Vector DB、時系列 Knowledge、大規模 Semantic Search（ADR-0047 §2）
- すべての会話の保存（再利用価値があるものだけ）
- LangMem による自動抽出・整理（D4 = **Phase 62**）。Phase 61 の `_inbox` は `record` で人・ワーカーが
  明示的に入れたものだけが入る
- タスクのタイムラインへの「この仕事から知識 N 件」の表示（D5 の後半。Phase 62）
