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

正本は **`[knowledge] root`**（既定 `~/.local/share/celeris/knowledge`）。**バージョン管理下**に置く（`celerisctl knowledge init` が
用意する。変更は 1 件ごとに 1 コミット）。

```
~/.local/share/celeris/knowledge/
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
celerisctl knowledge rerun  <task_id>                      # 管理系。**DB を開く**（ADR-0052 D3）
```

- **このサブコマンドだけは DB を開かない**。KB のファイルを直接読み書きするので、コンテナの中でも
  KB さえ同じパスにマウントされていれば動く。
- 根の決め方: `--root` > `CELERIS_KNOWLEDGE_ROOT` > `[knowledge] root` > `~/.local/share/celeris/knowledge`。
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
celerisctl knowledge init                  # ~/.local/share/celeris/knowledge を用意する（冪等）
$EDITOR ~/.config/celeris/config.toml      # [knowledge] を書く（既定でよければ省略できる）
$EDITOR ~/.local/share/celeris/knowledge/user/profile.md        # 雛形を埋める
$EDITOR ~/.local/share/celeris/knowledge/environment/clusters/pegasus.md
celerisctl knowledge reindex
```

`init` が置く雛形は**空欄と書き方だけ**（`confidence: low`）。`environment/clusters/{pegasus,sirius,fern03}.md` は
「接続 / 作業場所 / ジョブ / 環境」の見出しだけがあるので、`docs/workspace.md` と `config.toml` の `[[clusters]]` に
既に書いてあることを人が書き写し、`confidence: high` にする。**celeris は雛形を勝手に埋めない**（出典の無い
知識を作らないため）。

## 7. 自動メンテナンス（D4。Phase 62）

**既定は無効**（`[knowledge.langmem] enabled = false`）。有効にすると、タスクが終端になり報告
（ADR-0034）ができるたびに、決定的なトリガ（`crates/celeris/src/knowledge_maint.rs`。tick から
1 回・1 tick に最大 1 件）が**知識整理 run**（裏方 `support = "knowledge"`。人には見せない。
harness `knowledge`、adapter `langmem`）を起こす。**LLM が動くのはこの run の python プロセスの
中だけ**（`tools/langmem/`。CLAUDE.md「ディスパッチャやストアに LLM 呼び出しを入れない」を守る）。

### 何を渡すか

知識整理 run の依頼文（`objective`）には、終端になったタスクの id・題名・目的、その報告
（headline/body）、コメント、関連する既存の KB ページ（D3 の検索で「題名 + ラベル + 能力タグ」の
上位 `[knowledge.langmem] max_related_pages` 件。既定 10）、担当ノードの手帳（`memory/<node>/
notes.md`）の抜粋、既存の索引の題名（近い重複を作らないための手掛かり）を、決定的に組み立てて入れる
（`task_core::knowledge::maintenance_objective`。LLM は使わない）。python ランナー
（`langmem_run.py`）はこれをそのまま `langmem.create_memory_manager` に渡すだけで、他のファイルは
読まない。

### 何を保存するか・しないか

抽出の指示文（ADR-0047 D4）に明記してある:

- **保存する**: 将来も使える事実。出典（`sources`。少なくとも `task:<id>`）を必ず付ける
- **保存しない**: 一時的な情報、雑談、重複、信頼性の低い推測、秘密（API キー・パスワード・トークン・
  秘密鍵）。秘密は python 側の指示文でも避けるが、**適用前に決定的な検査**
  （`task_core::knowledge::secret_finding`。`sk-…`/`ghp_…`/`-----BEGIN` 等のパターン）で必ず弾く

### 候補の適用（`task_ops::knowledge::apply_candidates`。決定的）

run が書く `artifacts/knowledge-candidates.json` の各候補
（`{op, path, title, tags, scope, body, sources, confidence}`。`op` は `create`/`update`/`merge`/
`retire`）を、次の規則で機械的に振り分ける:

| 条件 | 結果 |
|---|---|
| path 境界違反・`.md` 以外・題名や出典が無い・本文が空（`retire` を除く）・64 KiB 超・秘密を含む | **落とす**（どこにも書かない） |
| `confidence: high` かつ `op: create`（対象がまだ無い）または `op: update`（対象があり、人の未コミット編集が無い） | **KB へ直接コミット**（author `Celeris (knowledge) <celeris@local>`、message `knowledge: <op> <path> (task <id>)`。`update` は既存の `sources` と和集合にする） |
| それ以外（`merge`/`retire`/`medium`/`low`/対象に人の未コミット編集がある/`create` なのに既にある/`update` なのに無い） | `_inbox/` へ（front matter に取り込み先 `path` と `op` を持たせる） |

適用のたびに `index.json` を作り直す（daemon の起動時にも、索引が無ければ作る）。

### `_inbox` での accept / reject（GUI・API は Phase 61 のまま拡張）

- `op` が無い候補（`record` が書いたもの）は Phase 61 のまま: accept が `path`（既定は `scope` と
  題名から決めたもの）へ書き、reject が捨てる
- **`op: merge`** の accept は、候補の本文（= 書き直した完全な版）で `target` を**必ず上書き**する
- **`op: retire`** の accept は、`target`（対象の既存ページ）を `_retired/<同じ相対パス>` へ動かす
  （P-61-k の答え: `DELETE /knowledge/page` は足さない。ページを消す経路は「retire → 候補を
  accept」の 1 本に統一する。`_retired/` は `_inbox/` と同じく索引にも検索にも出ない）
- reject はどちらも候補を捨てるだけ（対象のページには触らない）

### 見える場所

- Console の `knowledge` ブロック: 「この仕事から知識 N 件: 取り込み a / 候補 b / 破棄 c」
  （`GET /console`。適用が終わってから 1 件出る。`_inbox` への案内は `/knowledge/inbox`）
- タスクのタイムライン（`GET /tasks/{id}/timeline`）: `kind: "knowledge"` の 1 件
  （`state: scheduled | applied | failed`。`via` が `fallback:<adapter>` なら「（cheap のハーネスで抽出）」）
- `_inbox` の一覧（`GET /knowledge/inbox`）: 各候補の `op` と、取り込み元のタスクへのリンク（`sources`
  の `task:<id>`）

### 立ち上げ（人が 1 回だけやること）

```bash
scripts/knowledge/setup-langmem.sh          # $CELERIS_STATE_DIR/tools/langmem/.venv を作る
$EDITOR ~/.config/celeris/config.toml       # [adapters.langmem] と [knowledge.langmem] を書く
```

```toml
[adapters.langmem]
command = "~/.local/celeris/tools/langmem/.venv/bin/python"

[knowledge.langmem]
enabled = true
provider = "openai-compatible"
# ADR-0053 D2（Phase 65）: celeris の LLM source プロキシに向ける（`[llm_proxy]`。既定
# 127.0.0.1:18100）。Qwen が落ちていれば celeris/cheap は自動で Claude/GPT のアカウントプールに倒れる
# （ADR-0052 のフォールバックはプロキシの中に吸収される）。
base_url = "http://127.0.0.1:18100/v1"
model = "celeris/cheap"
# api_key_secret = "langmem-openai-key"   # 鍵を確認するエンドポイントのときだけ

[[providers]]
id = "langmem-main"
adapter = "langmem"
tiers = ["cheap"]
concurrency = 1
```

celeris を再起動（または `POST /reload` で読み直せない設定なので再起動）すれば、次の tick から
知識整理 run が起き始める。

## 7.1 Qwen が落ちているとき（ADR-0052。Phase 64）

知識整理 run の LLM は pegasus のトンネル越しの Qwen で、**トンネルは人が GUI で TOTP を通さないと
復帰しない**。Phase 63 までは落ちている間の run がそのまま `failed` になり、「1 タスクにつき 1 回」の
規則で二度と起こされなかったので、その間に終わった仕事の知識は永久に取り込まれなかった
（実機 2026-09-20〜21 に 4 件）。Phase 64 で次の 3 つが入った。

### (1) dispatch の直前に到達性を見る（D1）

知識整理タスクを起こす直前に、ディスパッチャが `[knowledge.langmem].base_url` へ
**`GET <base_url>/models` を 3 秒**で当てる（`crates/task-worker/src/probe.rs`。**LLM は呼ばない**）。
結果は `base_url` ごとに **60 秒キャッシュ**するので、tick ごとには叩かない。

- 2xx → 従来どおり `langmem` アダプタ（Qwen）で走る
- 接続不可・時間切れ・2xx 以外 → **(2) のフォールバック**へ。run の進行に
  `status`「langmem の接続先に届かない（<理由>）。cheap のハーネスに倒す（<adapter>）」が 1 行残る
- `base_url` が無い・`https://`・書き方が壊れている → **検査しない**（従来どおり `langmem`）

### (2) tier `cheap` の汎用ハーネスに倒す（D2）

`knowledge` ハーネスの `fallback`（組み込みの既定は `{ tier = "cheap" }`。`[[harnesses]]` で上書きでき、
`fallback = false` で無効）に従い、ADR-0049 の選び方で **tier `cheap` の汎用の供給元**（Claude Code /
Codex / ACP。枯渇・未ログインのプールは飛ばす）を決定的に 1 つ選んで、そのアダプタで同じ仕事をさせる。

- 前置き = `langmem_run.py` の `EXTRACTION_INSTRUCTIONS`（**同じ文面**を Rust 側が切り出して使う）
  ＋ 出力契約「`artifacts/knowledge-candidates.json` に `{"candidates": […]}` を書く。無ければ空配列。
  他のファイルは作らない。道具は使わない」
- 依頼文（`maintenance_objective`）は Qwen に渡すものと**同じ**
- 予算は `max_turns = 8` / `max_wall_secs = 600`
- tier `cheap` の汎用の供給元が 1 つも無ければ、従来どおり dispatch されずに `ready` のまま残る
  （供給が戻れば次の tick で拾われる）
- 適用（`apply_candidates`）は経路に関係なく同じ。`knowledge_runs.via` と `summary_json.via` に
  `"langmem"` か `"fallback:<adapter>"` が残り、Console のブロックとタスクのタイムラインに
  **「cheap のハーネスで抽出」**が出る

### (3) 失敗した run は一度だけやり直す（D3）

`knowledge_runs.state = failed` で `retried_at` がまだ無い行は、次の tick で **1 回だけ**作り直される
（`knowledge_maint::retry_failed`。1 tick に 1 件。`retried_at` を書く）。2 回目が Qwen で走るか
汎用ハーネスで走るかは (1) の検査が決める。2 回目も落ちたらそのまま `failed`（3 回目は無い）。

### 人が手で もう一度 やらせる

```bash
celerisctl knowledge rerun <元のタスクの id> [--db ~/.local/celeris/celeris.sqlite3]
```

`knowledge_runs` の 1 行を `state = failed` / `retried_at = NULL` に戻すだけ（`applied_at` /
`summary_json` / `via` は消える）。**新しい run を作るのはデーモンの次の tick**なので、デーモンを
止める必要も再起動も要らない。`<元のタスクの id>` は知識整理 run 自身の id ではなく、その run の
**元になった仕事**の id（Console の `knowledge` ブロックや `/tasks/<id>` のタイムラインから引く）。

> `knowledge` のサブコマンドで **DB を開くのはこの `rerun` だけ**（`org migrate-v2` と同じ管理系。
> celerisctl は HTTP の口を持たず、他のサブコマンドは DB を開かないままなのでコンテナの中でも動く）。

## 8. まだやらないこと

- Knowledge Graph、Vector DB、時系列 Knowledge、大規模 Semantic Search（ADR-0047 §2）
- すべての会話の保存（再利用価値があるものだけ）
- ノードの手帳（`memory/<node>/notes.md`）自体の自動要約・整理（知識整理 run は手帳を**読んで**
  KB 昇格の候補にするだけで、手帳そのものを書き換えない）

## Periodic Knowledge GC

`[knowledge.gc] enabled = true` で、タスク完了時の整理と独立した定期棚卸しを有効化する。
接続先は既存 `[knowledge.langmem]` と `langmem` adapter を共用するが、同節の
`enabled` はタスク完了時の整理だけを制御する。既定では GC は無効。

Rust scanner は Markdown の新鮮な metadata から low confidence、未確認／長期間未確認、
巨大ページ、同 scope の同一 title／共通 tags、共通 source を検出し、scope・tags・path の近傍を
既定 8 ページ、24,000 文字以内で選ぶ。`skills/`、`_inbox/`、`_retired/` と symlink は除外する。
全文が予算に収まらないページは切り詰めて置換提案を作らず skip する（必要なら人が分割する）。
文字予算には指示文も含む。外部サイト・クラスタ調査、新規事実、create は禁止。

既存 knowledge support task と adapter が候補を抽出する。GC の update/merge/retire は
confidence を問わず既存 validation を通して `_inbox/` へ保存し、GUI の「知識」で accept/reject
する。入力外の path や実行中に内容が変わった path には適用しない。タスク完了時の high-confidence
create/update の自動 commit と merge/retire の人間承認は従来どおり。

DB の隣の `*.knowledge-gc.json` に interval 予約、実行中 task ID、content hash と正常確認時刻を
保存する。再起動後も実行中は次を起こさず、失敗も interval を消費して retry storm を避ける。
予約後の起票失敗も次 interval まで待つ。起票と state 保存の間に落ちた場合、残った支援タスクの
実行中は追加起票しないが、対応する snapshot が無いためその結果を適用しない。
欠損／不正 JSON 出力・失敗 run は確認済みにしない。正常な空候補は確認済みにできる。
同 hash で `review_after_hours` 内のページを skip し、内容変更後は再対象にする。
確認だけでは canonical の `updated` や Git commit を変更しない。state は再生成可能で、
削除しても Markdown 正本は失われない（未処理 snapshot と確認履歴は失われる）。
エラーは警告に留め、通常 dispatch と既存 maintenance を継続する。
