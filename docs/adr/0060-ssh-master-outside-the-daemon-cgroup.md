# ADR-0060: クラスタの ssh ControlMaster を celeris の cgroup の外で起こす（昇格で切らない）

- 日付: 2026-09-22
- 状態: **Accepted**（Phase 103 として実装する。本番の観測への対応）
- 関連: ADR-0032（クラスタ接続。D2 の訂正、D4）、ADR-0053 D3（Qwen トンネル。master 経由の `-O forward`）、
  ADR-0040 D4（自己改善デプロイ、ライブ切替）、`docs/PROGRESS.md` P-100-1（本番の気づき）

## 1. 文脈

2026-09-22 の本番運用で、昇格（停止→起動でもライブ切替でも）のたびに pegasus の `connected` が
`false` に戻り、TOTP の再ログインが毎回必要になることが観測された（13:18 の停止→起動、13:44 と
15:18 のライブ切替）。

原因を実機で確かめたところ、**2 つ**あった。

1. **cgroup 経由の巻き添え**: celeris は `ssh -M -N <host>` を自分の子プロセスとして起こす
   （`crates/task-worker/src/cluster_login.rs`）。`~/.ssh/config` の `ControlPersist 10` により ssh は
   認証後に自分を切り離す（PPID 1 になる。ADR-0032 D2 の訂正）が、**切り離されても systemd unit
   `celeris@<sha12>` の cgroup の中には残る**（Linux では親プロセスが変わっても cgroup 所属は自動では
   移らない）。`celeris@.service`（`deploy/systemd/celeris@.service`）は `Type=simple`、
   `KillSignal=SIGTERM`、`TimeoutStopSec=3900` で、`KillMode` は未指定（既定の `control-group`）。unit
   が停止すると systemd は cgroup の中の**全プロセス**へ `SIGTERM` を送るため、切り離された master も
   一緒に死ぬ。ライブ切替でも旧 unit は draining 後に止まるので同じことが起きる。
2. **celeris 自身の Rust コードも殺していた（今回判明）**: `crates/celeris/src/main.rs` は
   `std::process::exit` を呼ばず、`tick_loop` が `Ok(Exit::..)` を返してから `main()` が普通に戻る。
   このため `SIGTERM` を受けて正常終了する経路でも、Rust の通常のスタック巻き戻しで
   `ClusterMasters`（`Arc<Mutex<HashMap<String, ClusterMaster>>>`）が drop される。旧い
   `impl Drop for ClusterMaster` は drop のたびにプロセスグループへ `SIGKILL` を送っていたため、
   **cgroup の問題を直しても、celeris の通常終了そのものが繋がっていた master を殺していた**。

`[[clusters.forwards]]`（ADR-0053 D3。Qwen トンネル）は master 経由の `-O forward` なので、master が
切れるとトンネルも切れる。

## 2. 決定

### D1. master は `systemd-run --user --scope` で celeris の cgroup の外に起こす（既定 `auto`）

```
systemd-run --user --scope --quiet \
  --unit celeris-ssh-master-<cluster id>-<短い乱数> \
  --description "celeris ssh master (<cluster id>)" \
  -- ssh <従来の引数> -M -N <host>
```

- `--scope` は指定したコマンドを **exec するだけ**なので、ssh は celeris の子プロセスのまま
  （stderr の汲み出し・`SSH_ASKPASS` の中継は従来どおり動く。環境変数は `Command::envs` で渡したものが
  そのまま届く。`--setenv` は使わない）。cgroup だけが新しい scope に移る。
- `celeris@<sha12>` unit が `KillMode=control-group`（既定）で止まっても、別 scope にいる master は
  巻き込まれない。ssh が `ControlPersist` で切り離した後も同じ scope に残る。
- `[[clusters]] master_launcher = "auto" | "systemd-run" | "inline"`（既定 `auto`）。`auto` は
  `systemd-run` が `PATH` にあり、かつ `XDG_RUNTIME_DIR` が設定されていれば `systemd-run`、
  どちらか欠けていれば従来どおり `inline`（celeris の直接の子。cgroup の中に留まる）。この判定は
  純関数 `resolve_master_launcher`（`crates/task-worker/src/cluster_login.rs`）にしてテストする。
  `celeris@.service` は `Environment=PATH=...:/usr/local/bin:/usr/bin:/bin` を明示しており、
  systemd の user unit には `XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS` が入るので、本番では設定を
  変えなくても `auto` が `systemd-run` を選ぶ。

### D2. `ClusterMaster` は、接続成立後は Drop で殺さない

- 旧い `impl Drop for ClusterMaster` を削除した。`ClusterMaster` を保持している `Arc<Mutex<HashMap<..>>>`
  が celeris の通常終了で drop されても、もう master は殺されない（D1 と合わせて、cgroup 経由でも
  Rust の drop 経由でも celeris の終了・再起動が master を道連れにしなくなった）。
- 明示的な切断（`DELETE /clusters/{id}/connect`）だけが `ClusterMaster::kill`（`self` を消費して
  `SIGKILL` + `wait`）を呼ぶ。`crates/celeris/src/cluster_admin.rs::drop_master` がこれを呼んでから、
  従来どおり `ssh -O exit <host>` も呼ぶ（人が張った master・ssh 側の状態も含めて閉じるため）。
- `spawn_master` の `Command` は `kill_on_drop(false)` に変えた（以前の `true` は、上記の明示的な
  `send_signal_to_group` と重複していた上、接続成立後に `Child` がうっかり drop されただけで master を
  巻き込む副作用があった）。`ClusterConnectSession`（TOTP のプロンプト待ち＝pending 状態）の
  `cancel`/`Drop` は従来どおり明示的に kill する（人の入力を待つだけの未成立の接続なので、取り消しで
  確実に消えてほしい。ここは変えていない）。

### D3. 切断は従来どおり `ssh -O exit`。listener の再確立は既存の「listener/target 分離」に乗る

- `DELETE /clusters/{id}/connect` は変えていない（`ClusterMaster::kill` → `ssh -O exit`）。
- `[[clusters.forwards]]`（ADR-0053 D3、Phase 85）の `refresh_one_forward`
  （`crates/task-dispatch/src/dispatcher.rs`）は、**listener（手元の TCP connect probe）が生きていれば
  `tunnel_forward_ensurer`（`-O forward`）を一切呼ばない**設計に既になっている。master が昇格をまたいで
  生き残れば forward の listener も生きたままなので、新リリースの celeris は起動直後にこの probe が
  真を返し、`-O forward` の再発行（＝二重の待ち受けを試みてエラーになりうる操作）は起きない。この
  フェーズでは `dispatcher.rs` は変更していない（Phase 85 の設計をそのまま使う）。
- 起動直後の celeris が `-O check` で生きている master を見つけて `connected = true` にする仕組み
  （ADR-0018 D2、`start_connect` の最初の分岐）はそのまま。

## 3. 採らない

- **`celeris@.service` の `KillMode` を `process` に変える**。ワーカーの子プロセスまで生き残ってしまう
  ため（人間の指示、および ADR-0040 D4 の drain 前提を壊す）。unit ファイルはこの Phase では触らない。
- **フォールバックの `ssh -N -L`（`tunnel_forward_ensurer` が bnode150 直結不可のときに張る別プロセス）
  を scope に移す**。滅多に使わない経路であり、このフェーズの本筋（master 本体）とは別問題なので触れて
  いない。必要になれば別 Phase で扱う。
- **`master_launcher` を `ClusterSpec`（`task-dispatch`）に持たせる**。`cluster_connector` は
  `cluster_id`/`host` しか受け取らない契約（ADR-0032 D3 のまま）なので、celeris 側
  （`crates/celeris/src/lib.rs::master_launchers`）で起動時に一度だけ id → launcher の表を作り、
  クロージャに渡す形にした。`task-dispatch` のクラスタ仕様は変えていない。

## 4. 受け入れ条件（Phase 103）

1. `MasterLauncher`（`Inline` / `SystemdRun { program }`）と `resolve_master_launcher`（純関数）、
   `launch_master_command`（純関数、argv の組み立て）を `crates/task-worker/src/cluster_login.rs` に持つ。
   `systemd-run` 指定で argv が期待どおりになる／`inline` は従来と同じ／`auto` の判定が両条件そろって
   初めて `systemd-run` を選ぶ、をユニットテストで確認する。
2. 偽の `systemd-run`（`--` の後を `exec` するだけのスクリプト）を経由しても、`SSH_ASKPASS` 等の環境変数
   が末端の ssh まで届く。
3. 接続成立後は `ClusterMaster` を drop しても偽 ssh のプロセスが生きている。`ClusterMaster::kill` を
   呼べば確実に落ちる。TOTP の pending（`ClusterConnectSession`）の `cancel` は従来どおり子を殺す。
4. `[[clusters]] master_launcher` が `"auto"`（既定）/`"systemd-run"`/`"inline"` を取り、それ以外は
   設定エラー（`Config::validate`）。
5. `cargo test --workspace --no-fail-fast` / `cargo clippy --workspace --all-targets -- -D warnings`。
6. 本番確認（親エージェントが実施）: 昇格 → 人が pegasus に一度接続 → **次の昇格の後も**
   `GET /clusters` の pegasus `connected=true` が維持され、`systemd-cgls --user` で ssh master が
   `celeris-ssh-master-pegasus-*` scope に居ることを確認する。

## Phase 103 追記（実装、2026-09-22）

`crates/task-worker/src/cluster_login.rs` に `MasterLauncher` / `resolve_master_launcher` /
`launch_master_command` / `path_has_executable` / `systemd_run_on_path` / `xdg_runtime_dir_is_set` を
追加し、`ClusterMaster` から `impl Drop` を削除して `kill(self)`（明示的切断専用）に置き換えた。
`crates/celeris/src/config.rs` の `ClusterConfig` に `master_launcher`（既定 `"auto"`）を足し、
`crates/celeris/src/cluster_admin.rs` と `crates/celeris/src/lib.rs::cluster_connector` の両方の
`start_connect` 呼び出し（GUI 手動接続、ディスパッチャの自動接続の 2 経路）を解決した `MasterLauncher`
で呼ぶように変えた。テストは実 ssh・実 systemd-run を使わず、既存の偽 ssh の流儀で偽 `systemd-run` も
作った。詳細は `docs/PROGRESS.md` の Phase 103 節を見よ。

## Phase 105 追記（2026-09-22）: `promote.sh` にも同じ問題があった

昇格そのもの（`promote.sh`）も celeris の子として `setsid` で起こしていたため、この ADR の master と
同じ理由（cgroup 経由の巻き添え）で、旧デーモンの drain に途中で殺されていた（本番 2026-09-22
21:55 UTC の観測）。判断（`resolve_master_launcher` 相当）と argv の組み立て
（`launch_master_command` 相当）を `crates/task-worker/src/detach.rs`（`DetachLauncher` /
`resolve_detach_launcher` / `wrap_command`）に共通化し、`cluster_login.rs` はそこへ委譲する薄い
型別名・関数に変えた（挙動・既存テストは変えていない）。`promote.sh` 側の決定と受け入れ条件は
ADR-0040 の「Phase 105 追記」、`docs/PROGRESS.md` の Phase 105 節を見よ。
