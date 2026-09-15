# ADR-0018: 複数クラスタでのコマンド実行（ssh + ControlMaster）

- 日付: 2026-09-15（改訂: 人間の判断「LLM は手元、コマンドだけリモート」「pegasus / sirius は 2 要素認証で、ssh を貼るのは人力。ControlMaster が貼られている前提で動くように」）
- 状態: **Accepted**（Phase 12 として実装する）
- 関連: DESIGN §5.8（Workspace）、§6 Phase 12、ADR-0003（ワーカープロトコル）、ADR-0010 D5（供給側失敗）、ADR-0012（プロバイダ）、ADR-0013 D5（DB はローカルディスク）

## 文脈

`WorkspaceSpec::Remote { cluster, path }` は型だけがあり、ディスパッチャは remote のタスクを警告して `ready` のまま放置していた。
人間の狙いは pegasus / sirius のようなクラスタに仕事を投げることだが、**走らせたいのは LLM ではなく計算**（ビルド・実験・テスト）である。

前提（人間の説明と実測）:

- pegasus / sirius への ssh は **2 要素認証**で、接続を張るのは人力。パスワードや OTP を taskd が扱うことはできない。
- したがって taskd は **ssh の多重化（`ControlMaster`）で人が張った接続を借りる**。接続が無ければ「人がログインするまで待つ」以外にできることは無い。
- 手元（fern03）の `/home` は NFS。クラスタ側と共有かどうかは未確認（2 要素認証のため、確認には人の操作が要る）。

## 決定

### D1. LLM はローカル、リモートで実行するのは「コマンド」だけ。**プロジェクトはクラスタ側が正**

- ワーカー（claude-code / codex）は**これまでどおり taskd のホストで動く**。クラスタ側に CLI も認証情報も置かない。
- クラスタで実行するのは次の 2 つ:
  1. **受け入れ条件の `Check::Command`**（taskd が判定のために自分で実行するもの）
  2. **ワーカーが実行を頼むコマンド**（D3 のラッパ経由）
- 場所の表し方は**既存の `WorkspaceSpec::Remote { cluster, path }` をそのまま使う**（新しいフィールドを足さない）。
  - `path` は**クラスタ側の作業ディレクトリ**で、タスクごとに人が指定する。**すでにあるプロジェクト**（例
    `/work/NBB/rmaeda/workspace/rust/benchfs`）を指してよい。
  - taskd はそれを `workspace_root/<task_id>` に**写し**として持つ。ワーカー（LLM）はこの写しを読み書きし、
    run のログ（`runs/`）と成果物の照合はこれまでどおり手元で行う。
  - **真実はクラスタ側**。写しは run のたびに作り直される前提で扱う。

### D2. 接続は `ControlMaster` 前提。無ければ「人待ち」

- 設定 `[[clusters]]` の各行は、ssh の宛先（`~/.ssh/config` の `Host` 名）と作業ディレクトリ、並列度を持つ。
- taskd は必ず `ssh -o BatchMode=yes -O check <host>` で**多重化された接続の有無を先に調べる**。
  - 接続がある → そのまま `ssh -o BatchMode=yes <host> -- <command>` で実行する（2 要素認証は走らない）。
  - 接続が無い → **供給側失敗として扱い**、そのクラスタを cooldown にして requeue する（`attempts` を消費しない）。
    `Event::ProviderThrottled` と同じ形で `Event::ClusterUnavailable{cluster, reason}` を残し、GUI の「注意」区画に
    「pegasus へのログインが切れています。`scripts/cluster-login.sh pegasus` を実行してください」と出す。
- **taskd から対話的な認証は絶対に行わない**（`BatchMode=yes` を常に付ける）。パスワード・OTP をログや DB に残さない。
- 接続の維持は人の操作（`ControlPersist` の期限が切れたら張り直す）。taskd は接続を張らない。

### D3. ワーカーがクラスタでコマンドを実行する手段（ラッパ）

- `WorkspaceSpec::Remote` のタスクでは、run の開始時に写しの直下へ **`.taskd/remote-exec`**（実行可能なスクリプト）を置く。
  中身は `ssh -o BatchMode=yes <host> -- 'cd <remote_workdir> && <setup> && "$@"'` 相当で、引数のコマンドをクラスタで実行して標準出力・終了コードをそのまま返す。
- `RunRequest.task` にその存在と使い方を書いた指示文を足す（「重い処理・クラスタ上のデータを使う処理は `.taskd/remote-exec <cmd>` で実行すること」）。
  ワーカーが従うかは保証しないが、**受け入れ条件はクラスタ側で判定される**ので、ローカルだけで済ませたタスクは条件で落ちる。
- `.taskd/remote-exec` は run ごとに作り直し、`rsync` の同期対象から外す。

### D4. 同期は「pull してから作業し、push してから判定する」。既定では消さない

`[[clusters]] sync = "rsync" | "none"`（既定 `"rsync"`）、`delete_on_push`（既定 **false**）。

run 1 回の順序（`sync = "rsync"` のとき）:

1. **pull**: `rsync -a <host>:<path>/ <mirror>/`（クラスタ → 手元の写し。手元側は `--delete` してよい＝写しなので）
2. ワーカー（LLM）が**手元の写し**で作業する
3. **push**: `rsync -a <mirror>/ <host>:<path>/`（手元 → クラスタ）。
   **既定では `--delete` を付けない**（既存プロジェクトのファイルを消さないため）。taskd 専用の作業ディレクトリなら
   `delete_on_push = true` にしてよい
4. **判定**: `Check::Command` をクラスタで実行する（2 で編集した内容が反映済み）
5. **pull**: 判定で生まれた成果物を取り込み、`Check::ArtifactExists` と sha256 は手元で判定する

- `.taskd/`（ラッパ置き場）は両方向で同期から外す。`rsync_excludes` で `.git/` 等を足せる。
- `sync = "none"`: 共有ファイルシステムのとき。`path` が手元からも同じパスで見えることが前提。
- 競合について: pull から push までの間にクラスタ側で第三者が変更すると、push で上書きしうる（`--delete` 無しなので消しはしない）。
  タスクの作業ディレクトリは 1 つのタスクが占有する前提とし、重なる場合は人が `depends_on` で直列化する。

### D5. 並列度・失敗・時間

- 並列度は「プロバイダ（アカウント）」と「クラスタ」の二次元。どちらかが上限なら、その組は飛ばして次を試す（ADR-0012 の選択手順の拡張）。
  ローカル実行のタスクはクラスタの上限を消費しない。
- ssh の失敗の分類:
  - 多重接続が無い / 認証を求められた / ホストに届かない → **供給側失敗**（cooldown + requeue、attempts は消費しない）
  - コマンドが 0 以外で終了した → **判定の失敗**（受け入れ条件の不合格。attempts を消費する通常の失敗）
  - この区別は `ssh` の終了コード 255（ssh 自身の失敗）と、それ以外（リモートコマンドの終了コード）で行う。
- `Check::Command` のタイムアウト（`review_timeout_secs`）はリモートでも同じ。ssh 側にも `timeout` を掛ける。

### D6. 運用の道具（この ADR で一緒に入れる）

- `config/ssh-config.example`: `ControlMaster auto` / `ControlPath ~/.ssh/cm-%r@%h:%p` / `ControlPersist 8h` の雛形。
- `scripts/cluster-login.sh <host>`: 人が 1 回だけ実行して多重接続を張る（2 要素認証はここで通す）。張れたか `ssh -O check` で確認する。
- `scripts/cluster-check.sh <host>`: 多重接続の有無、`uname`、`rsync` / `python3` の有無、作業ディレクトリの書き込み可否、
  **共有ファイルシステムかどうか**（ローカルで作った印のファイルがリモートから見えるか）を調べて出す。読み取りだけ。

### D7. 秘密と安全

- taskd は鍵・パスワード・OTP を扱わない。`~/.ssh/config` と多重接続は人の管理下。
- API は `[[clusters]]` の `id` / `host` / `remote_workdir` / `concurrency` / `sync` を返してよい。`env` の値は返さない（ADR-0013 D11 と同じ）。
- `known_hosts` の検証設定は変更しない（`StrictHostKeyChecking` に触らない）。

## 採らない

- ジョブスケジューラ（Slurm 等）への投入。まずログインノードでの直接実行にする。必要になれば `[[clusters]] kind = "slurm"` で D2 の実行部だけ差し替える。
- クラスタ側に taskd やワーカー（LLM）を置く構成。
- taskd が ssh 接続を張る／2 要素認証を自動化する試み。

## 結果

- 新しい設定 `[[clusters]]`、`Event::ClusterUnavailable`、`.taskd/remote-exec`。タスク側は既存の `WorkspaceSpec::Remote` のまま。
- `Check::Command` の実行場所が、`WorkspaceSpec::Remote` のタスクではクラスタになる（`Local` のタスクは従来どおり手元）。
- テストは **ssh 先を `localhost` にして行う**（外部ネットワークに出ない。CLAUDE.md の規則）。実クラスタでの確認は人の操作を伴う手順として記録する。
