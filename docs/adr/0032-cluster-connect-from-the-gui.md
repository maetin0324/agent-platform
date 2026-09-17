# ADR-0032: クラスタへの接続を GUI から張る（認証方式を設定で分け、TOTP は中継する）

- 日付: 2026-09-17
- 状態: **Accepted**（人間の依頼「pegasus や sirius のクラスタ画面で、接続ボタンを押して TOTP を GUI から送信したら
  接続されるようにしてください。また fern03 などクラスタでないノードで実験する事もありますが、fern03 等は
  二要素認証が要らないのでクラスタの config で良い感じに設定できるようにしてください」）
- 関連: **ADR-0018 D2 / D7 / §3「採らない」を上書きする**、ADR-0019（sync）、ADR-0024 / 0025（ログイン中継の先例）、
  ADR-0017 M3（管理系はトークン必須）、ADR-0030（GUI から秘密を預かる先例）

## 1. 文脈

ADR-0018 は「**taskd から対話的な認証は絶対に行わない**」「接続を張るのは人力」「§3 採らない: taskd が ssh 接続を張る／
2 要素認証を自動化する試み」と決めていた。当時の判断は「パスワードや OTP を taskd が扱うことはできない」という前提に
基づく。運用してみると、この前提から次の 2 つの不便が出た。

1. **2 要素認証のクラスタ（pegasus / sirius）**: 接続が切れるたびに、人が手元の端末で `scripts/cluster-login.sh` を
   実行しに行く必要がある。GUI は「実行してください」と案内するだけで、画面からは何もできない。
2. **2 要素認証の要らないノード（fern03 など）**: 鍵だけで入れるのに、「人が接続を張るまで待つ」という
   同じ扱いを受ける。設定でこの違いを表現できない。

人間の依頼により、ADR-0018 のこの部分を上書きする。ただし **OTP を保存しない**という原則は維持する
（保存する案は §3 で明示的に採らない）。

### 実機で確かめた事実（2026-09-17）

- **認証の段数が host によって違う**。`ssh -v -o BatchMode=yes` で観測:
  - `sirius`: `Authentications that can continue: publickey` → `Authenticated using "publickey" with **partial success**` →
    `Authentications that can continue: keyboard-interactive` → `Permission denied (keyboard-interactive)`。
    **publickey の後に TOTP が来る 2 段構え**。
  - `fern03`: `Authenticated to fern03.omni.hpcc.jp ... using "publickey"`。**1 段で完了。TOTP は無い**。
  - `pegasus`: 既存の ControlMaster が生きているため観測できず（人間の申告どおり sirius と同じ扱いにする）。
- **`SSH_ASKPASS` + `SSH_ASKPASS_REQUIRE=force` で、pty 無しに TOTP を中継できる**。ssh はプロンプト文字列を
  askpass プログラムの **argv[1]** で渡し、その **標準出力**を答えとして読む。非 0 終了は「利用者がキャンセルした」扱い。
  実測したプロンプト: `(rmaeda@130.158.241.2) Verification code: `（末尾に改行なし）。
  → 既存のログイン中継（`claude_account.rs`）のような pty / 改行なしプロンプトの読み取りは**不要**。
- **`~/.ssh/config` は全 Host が `ControlPersist 10`（10 秒）**。`ssh -M -N -f` で張って離すと、
  最後のクライアントが去った 10 秒後に master が消える。**`-f` を付けずに master プロセスを保持し続ければ、
  `ControlPersist` の値に関係なく接続が生きる**ことを fern03 で確認した（`-O check` → `Master running (pid=…)`、
  別プロセスの `ssh -o BatchMode=yes fern03 -- hostname` が成功）。
- **FIFO（名前付きパイプ）で秘密をディスクに落とさずに受け渡せる**ことを確認した。偽 ssh を使った配管の実験で、
  プロンプトが出て行き、コードが戻り、FIFO のサイズは 0 のまま（`prw-------`、中身はカーネルのパイプバッファ）。

## 2. 決定

### D1. 認証方式はクラスタごとに設定する（`[[clusters]].auth`）

```toml
[[clusters]]
id = "fern03"
host = "fern03"
auth = "publickey"     # 鍵だけで入れる。taskd が自分で接続を張ってよい

[[clusters]]
id = "pegasus"
host = "pegasus"
auth = "totp"          # publickey の後に検証コードが要る。GUI から送る

[[clusters]]
id = "legacy"
host = "legacy"
auth = "manual"        # 既定。taskd は接続を張らない（ADR-0018 D2 の従来どおり）
```

- **既定は `"manual"`**。`auth` を書いていない既存の設定の挙動は 1 ミリも変わらない。新しい振る舞いは opt-in。
- 値は 3 つだけ。それ以外は設定エラー（`sync` と同じ規約）。
- `"publickey"` と `"totp"` の違いは**人の入力が要るかどうかだけ**で、ssh の呼び方は同じ
  （`auth` で `PreferredAuthentications` 等をいじったりはしない。ssh の設定は人の `~/.ssh/config` が正。ADR-0018 D7）。

### D2. 接続は taskd が `ssh -M -N` の子プロセスを**保持する**ことで張る

- `-f`（バックグラウンド化）は使わない。taskd がその子プロセスを持ち続け、それが master になる。
  したがって **`ControlPersist` の値に依存しない**（実機の `ControlPersist 10` でも切れない）。
- 子が死んだら接続も終わる。**taskd を止めれば接続も閉じる**（ADR-0018 の「人が張った接続を借りる」から、
  「taskd が張った接続を taskd が使う」に変わる。借りる側の仕組み＝`-O check` と `BatchMode=yes` は**そのまま**）。
- 人が `scripts/cluster-login.sh` で張った master があるなら、taskd はそれをこれまでどおり借りる。
  **接続の有無の判定は今までどおり `ssh -o BatchMode=yes -O check <host>` の 1 本**（`refresh_cluster_liveness`）。
  張り方が増えるだけで、見方は増やさない。
- 既に master がある状態で接続要求が来たら、新しく張らずに成功を返す（`cluster-login.sh` と同じ判断）。

### D3. `auth = "publickey"` のクラスタは、ディスパッチャが自動で張る

- dispatch の直前に `connected == false` を見つけたとき、**`auth = "publickey"` なら、cooldown にする前に
  1 回だけ接続を試みる**。成功したらそのまま dispatch に進む。
- 失敗したら従来どおり `mark_cluster_unavailable()`（cooldown + `Event::ClusterUnavailable`）。
  **理由（`reason`）に「自動接続を試みて失敗した」ことを書く**ので、人は受信箱で区別できる。
- 接続の試行は **クラスタごとに 1 回ずつ直列**で、cooldown 中は試みない（tick ごとに ssh が湧かないようにする）。
  接続に使う時間は `connect_timeout_secs`（既定 30）で切る。
- `auth = "totp"` と `"manual"` では**自動では張らない**（人の入力が要るので当然）。従来どおり cooldown に落ちる。

### D4. TOTP は「その場で使って捨てる」形で中継する（保存しない）

仕組み（実機で確認した `SSH_ASKPASS` を使う）:

1. taskd が 0700 の一時ディレクトリを作り、その中に FIFO を 2 本（`prompt` / `code`）と askpass の小さなスクリプトを置く。
   スクリプトは「argv[1] を `prompt` に書き、`code` から読んだものを標準出力に出す」だけ。
2. `SSH_ASKPASS=<そのスクリプト>`、`SSH_ASKPASS_REQUIRE=force`、`DISPLAY=`（念のため）を渡して
   `ssh -M -N <host>` を起動する。**`BatchMode=yes` は付けない**（付けると askpass が呼ばれない）。
3. taskd は `prompt` から**プロンプト文字列**を読み、API の応答として GUI に返す。
   プロンプトが所定の時間（既定 30 秒）来なければ、鍵だけで入れたか失敗したかなので、`-O check` で判定して終える。
4. GUI から届いたコードを taskd が `code` に流す。**コードはメモリ上だけを通り、ファイル・DB・ログ・API 応答の
   どこにも残らない**（FIFO の中身はカーネルのパイプバッファで、ディスクには落ちない）。書いた直後に FIFO を unlink する。
5. `-O check` が通れば成功。通らなければ失敗として子プロセスをプロセスグループごと落とす。

- **コードの検証**: `trim` して空、または制御文字を含むなら ssh に渡さずに 422
  （`claude_account.rs::submit_code` と同じ注入防止）。長さや文字種は**制限しない**（TOTP 以外のプロンプトもありうる）。
- **ログに出すもの**: `who = "admin"`, `op = "cluster_connect" | "cluster_connect_code" | "cluster_disconnect"`,
  `cluster`, `host` だけ。**プロンプト文字列も応答には返すがログには出さない**（ユーザ名やホスト名が入るため）。
- セッションは taskd のメモリ上のマップ（`id` ごとに高々 1 つ）で持ち、`tick_loop` が毎 tick 期限切れを掃除する。
  期限は既定 300 秒。**`accounts_admin.rs` の B1 の規約（掃除の関数はチャネルに送らず、id を返して呼び出し側が反映する）に従う**。

### D5. API（3 本。すべて管理系 = `token_file` 未設定でも 401。ADR-0017 M3）

| エンドポイント | 内容 |
|---|---|
| `POST /clusters/{id}/connect` | 接続を開始 → 200 `ClusterConnectStart{kind: "connected" \| "needs_code", prompt?, expires_at?}` |
| `POST /clusters/{id}/connect/code` `{code}` | コードを送る → 200 `ClusterConnectResult{ok, detail}` |
| `DELETE /clusters/{id}/connect` | 進行中の接続を取り消す / 張った接続を切る → 200 `{}` |

- `kind = "connected"` は「コード無しで張れた」（`auth = "publickey"`、または既に master があった）。
- 既知の id でなければ 404 `cluster_not_found`。`auth = "manual"` に `connect` したら 409 `cluster_connect_not_supported`。
  進行中のセッションが無いのに `connect/code` したら 409 `cluster_connect_not_started`。
- `GET /clusters` は**読み取り専用のまま**（従来どおり無認証）。ただし `ClusterView` に `auth` と
  `connect_pending: bool` を足す。**プロンプト文字列は `GET /clusters` には出さない**（`POST` の応答にだけ出す。
  ADR-0024/0025 の「URL とコードは action の戻り値にだけ置く」と同じ規律）。

### D6. GUI は「クラスタ」画面の既存の案内を置き換える

- `connected === false` のときに出している「手元で `scripts/cluster-login.sh <host>` を実行してください」の Alert を、
  **`auth` に応じて出し分ける**:
  - `publickey`: 「接続」ボタン（押すとその場で張る）。
  - `totp`: 「接続」ボタン → プロンプト文字列と入力欄（`type="password"`, `autocomplete="off"`, `inputmode="numeric"`）
    → 「送信」。取り消しボタンも置く。
  - `manual`: 従来どおりの案内文（`cluster-login.sh` を実行してください）。
- コードは action の戻り値にも**載せない**（結果の可否だけ）。値が画面に残らないようにする。
- **注意書き**: コードは平文 HTTP を通る（GUI は LAN で平文）。ADR-0024 D7 / ADR-0030 D4 と同じ注意を画面に出す。

### D7. `scripts/cluster-login.sh` は残す

GUI や taskd が使えないときの逃げ道として残す（`-f` で張った master は taskd が借りられる。D2）。
ただし README と画面の案内は「GUI の接続ボタン」を先に案内する。

## 3. 採らない

- **TOTP のシークレット（`otpauth://`）を預かって taskd がコードを生成する**。人間の判断。
  2 要素認証の意味が実質的に失われるため。ADR-0018 D7 の「taskd は鍵・パスワード・OTP を扱わない」のうち、
  **「保存しない」は維持する**（今回外すのは「対話的な認証を一切しない」の部分だけ）。
- **パスワード認証の中継**。今回のプロンプトは TOTP（`keyboard-interactive`）に限らないが、
  パスワードを GUI から送る運用は勧めない。仕組みとしては同じ経路を通るので技術的には可能だが、
  設定例にも書かないし、ドキュメントでも案内しない。
- **pty で ssh を駆動する**。`SSH_ASKPASS` で足りることを実機で確認したため（§1）。依存を増やさない。
- **接続の自動維持（切れたら勝手に張り直す）を `totp` にも広げる**。人の入力が要るので不可能。
  `publickey` の自動接続（D3）だけにする。
- **`auth` から ssh のオプションを組み立てる**。ssh の設定は人の `~/.ssh/config` が正（ADR-0018 D7）。

## 4. 受け入れ条件（Phase 22 / G12）

1. `[[clusters]].auth` が `"manual"`（既定）/ `"publickey"` / `"totp"` を取り、それ以外は設定エラー。
   `auth` を書かない既存の設定の挙動が変わらない（`ClusterSpec` と dispatch のテスト）。
2. `POST /clusters/{id}/connect` / `POST /clusters/{id}/connect/code` / `DELETE /clusters/{id}/connect` が
   **トークン無しで 401**（`token_file` 未設定でも）。未知の id は 404、`auth = "manual"` は 409、
   セッション無しの code は 409、空・制御文字入りの code は 422。
3. 偽の ssh（テスト用スクリプト）で、`needs_code` → コード送信 → `-O check` 成功、の一連が通る。
   コードが応答・ログ・`GET /clusters` のどこにも出ない。取り消しで子プロセスが落ちる。
4. `auth = "publickey"` のクラスタで接続が無いとき、ディスパッチャが 1 回だけ接続を試み、
   失敗したら従来どおり cooldown + `ClusterUnavailable`（`reason` に自動接続の失敗と分かる文字列が入る）。
5. GUI: `auth` ごとに 3 通りの見た目になり、`totp` でプロンプトと入力欄が出る。コードが `fetcher.data` にも残らない。
6. **実機**: fern03 を `auth = "publickey"` で登録し、GUI の接続ボタンだけで接続され、クラスタでタスクが動く。
   pegasus か sirius を `auth = "totp"` で登録し、**人が GUI に実際の TOTP を入れて接続できる**
   （コードは人間が用意するので、ここは人間と一緒に確認する）。
7. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の検査一式 /
   `scripts/sync-gui-docs.sh --check`。
