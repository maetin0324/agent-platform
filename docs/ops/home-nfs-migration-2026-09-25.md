# home-dev のホームディレクトリを TrueNAS の NFS 共有へ直接載せ替える（手順書）

作成: 2026-09-25（Fable）。実行者: 人（Proxmox ホストと TrueNAS の操作が要る）。Celeris と実装エージェントは移行が終わるまで起こさない。

## 現状（LXC 内から確認した事実）
- CT 100（home-dev、192.168.1.103、kernel 6.8.12-9-pve）は **非特権 CT**（`/proc/self/uid_map` = `0 100000 65536`）。CT の rmaeda は uid/gid 1001 = ホストでは 101001。
- `/home` は `/dev/loop0`（ext4、1007G、使用 438G）。実体はホスト側の `/mnt/pve/truenas/images/100/vm-100-disk-0.raw`（TrueNAS の NFS 上の raw イメージ）。fsync 69 ms。
- `/` はローカル LVM `pve-vm-100-disk-0`（ext4、236G 空き）。Celeris の DB は既に `/var/lib/celeris/celeris.sqlite3`（ローカル）。
- CT 内に NFS クライアントはある（`/usr/sbin/mount.nfs4`、カーネルに nfs/nfs4）。
- `/home` に依存するもの: `~/.config/celeris`（設定・token・secrets）、`~/.local/celeris`（workspaces 242G→整理後、build-cache、releases、tools、backups、memory の langmem SQLite）、`~/.local/share/celeris/knowledge`（git）、`~/.ssh`（鍵と **ControlPath の UNIX ソケット** `~/.ssh/mux-*`）、`~/.claude`・`~/.codex`（各 CLI の状態）、`~/.cargo`・`~/.rustup`、`~/workspace`（リポジトリと agent worktree）、systemd user unit（`%h` 参照）。

## 設計上の決め（提案）
1. **NFS はホストで mount し、CT には bind mount（`mp0`）で渡す**（Proxmox の標準。CT の再作成時も `mp0` を付け直すだけ）。CT 内で直接 `mount -t nfs` する案は `features: mount=nfs` が要り、CT 死亡後の再利用性は同じで、ホスト側のキャッシュも効かないので採らない。
2. **uid は 1001 のまま TrueNAS に置く**（再利用のため）。非特権 CT の 1001 はホストでは 101001 なので、`lxc.idmap` で uid/gid 1001 だけをホストの 1001 に写す（`/etc/subuid` / `/etc/subgid` に `root:1001:1` を足す）。これで TrueNAS 上のファイル所有者は 1001:1001 になる。
3. **I/O の重いもの・ソケット・SQLite はホーム（NFS）に置かない**: Celeris の workspaces / build-cache / releases / staging / tools と `~/.cargo` の target 相当、langmem の memory SQLite、ssh の ControlPath。NFS は metadata の往復が遅く（git / cargo が顕著）、UNIX ソケットは置けず、SQLite の WAL は NFS では壊れうる。
4. TrueNAS 側は専用 dataset（例 `tank/home/rmaeda`、snapshot 有効、NFS export は Proxmox ホストの IP だけ、`maproot` は使わず所有者 1001 で書く）。

## 手順
### 0. 準備（ダウンタイムなし）
- TrueNAS: dataset `tank/home/rmaeda`（owner 1001:1001、mode 700）、NFS export（許可ホスト = Proxmox ホスト、NFSv4、`no_root_squash` は不要）。
- Proxmox ホスト: `mkdir -p /mnt/truenas-home && echo 'truenas:/mnt/tank/home /mnt/truenas-home nfs4 _netdev,hard,noatime,nconnect=4 0 0' >> /etc/fstab && mount /mnt/truenas-home`。`touch /mnt/truenas-home/rmaeda/.probe` で書けることと所有者を確認。
- Proxmox ホスト: `/etc/subuid` と `/etc/subgid` に `root:1001:1` を追記。CT 設定（`/etc/pve/lxc/100.conf`）に idmap を追加:
  ```
  lxc.idmap: u 0 100000 1001
  lxc.idmap: g 0 100000 1001
  lxc.idmap: u 1001 1001 1
  lxc.idmap: g 1001 1001 1
  lxc.idmap: u 1002 101002 64534
  lxc.idmap: g 1002 101002 64534
  ```
  （idmap を変えると CT 内の既存ファイルの所有者もずれる: rootfs 上の rmaeda 所有物は 101001 のまま残るので、切替後に `chown -R 1001:1001 /var/lib/celeris /home/rmaeda` 相当の修正が要る。`/var/lib/celeris` はこれに該当する。）
- CT 内（rmaeda）: ホームを軽くする。`~/.local/celeris/workspaces` は終端タスクの `repos/*/target` 等を消す（`celerisctl workspace prune --older-than 1`、`repos/` 配下は手で）。`~/workspace/agent-platform/target` と `.claude/worktrees/*/target` を消す。`~/.cargo/registry` は再取得できるので除外可。目安: 438G → 100G 以下。
- CT 内: ssh の ControlPath を `/run/user/1001/ssh-mux-%r@%h:%p` に変える（`~/.ssh/config`。NFS にソケットは置けない）。Celeris の ssh master（`celeris-ssh-master-*`）も同じ設定を見る。

### 1. 初回コピー（サービスは動いたまま。差分は後で取る）
Proxmox ホストで（loop の実体を直接読むのではなく CT の中身を rsync）:
```
pct exec 100 -- bash -c 'true'   # 疎通
rsync -aHAX --numeric-ids --info=progress2 \
  --exclude '.local/celeris/workspaces/*/repos/*/target' --exclude '.local/celeris/build-cache' \
  --exclude '.cargo/registry' --exclude 'workspace/agent-platform/target' --exclude '.claude/worktrees/*/target' \
  --exclude '.ssh/mux-*' \
  /var/lib/lxc/100/rootfs/home/rmaeda/ /mnt/truenas-home/rmaeda/
```
（rootfs のパスは `pct mount 100` で `/var/lib/lxc/100/rootfs` に出す。所有者は idmap 前なので 101001 で写る → 最後に `chown -R --from=101001:101001 1001:1001 /mnt/truenas-home/rmaeda`。）

### 2. 停止
- CT 内: `GET /api/v1/tasks?status=running|reviewing` が 0 であることを確認。`systemctl --user stop 'celeris@*' 'celeris-gui@*' 'celeris-ssh-master-*'`。実装エージェントは動かしていないことを確認。
- Proxmox ホスト: `pct stop 100`。

### 3. 差分コピーと切替
- Proxmox ホスト: `pct mount 100` → 同じ rsync をもう一度（`--delete` 付き）→ `chown` → `pct unmount 100`。
- Proxmox ホスト: `pct set 100 -mp0 /mnt/truenas-home/rmaeda,mp=/home/rmaeda,backup=0`。旧 `/home` の loop ボリューム（`mp` か `rootfs` の別ディスク）は **まだ消さない**（`100.conf` の該当行をコメントアウトして保存）。
- `pct start 100` → CT 内で `findmnt /home /home/rmaeda`（nfs4 と表示されるはず）、`id`、`ls -ln ~ | head`（所有者 1001）、`sudo -n true`。
- `chown -R 1001:1001 /var/lib/celeris`（idmap の変更で必要）。`ls -ln /var/lib/celeris`。

### 4. 起動と確認
- CT 内: `systemctl --user daemon-reload && systemctl --user start celeris@<current> celeris-gui@<current>`（`readlink ~/.local/celeris/current`）。`GET /api/v1/health`、GUI 7700、`journalctl --user -u 'celeris@*' -n 50`。
- ssh: `ssh pegasus true` が ControlPath の新しい場所で動く。クラスタ再接続（TOTP）。
- 性能: `dd if=/dev/zero of=~/.fsync-test bs=4k count=200 oflag=dsync`（loop 時は 69 ms/回）。git status / cargo build の体感。
- 3 日ほど問題なければ旧 loop ボリュームを削除（`/mnt/pve/truenas/images/100/vm-100-disk-0.raw`）。

### 5. 移行後の Celeris 設定（人が `config.toml` を編集し、次の昇格で有効）
- `workspace_root = "/var/lib/celeris/workspaces"`（I/O の重い作業場所をローカルへ。既存の workspaces は終端タスク分を残す必要なし）。
- `[workspace] build_cache_dir = "/var/lib/celeris/build-cache"`。
- `[memory] dir = "/var/lib/celeris/memory"`（langmem の SQLite。移行時に `~/.local/celeris/memory` をコピー）。
- `[db] backup_dir` はホーム（NFS、snapshot あり）のままでよい。releases / staging / tools もローカル（`/var/lib/celeris`）へ寄せると昇格が速い（`CELERIS_STATE_DIR` を変えるか symlink）。
- 実装エージェント（Fable 配下）の worktree `~/workspace/agent-platform/.claude/worktrees` は NFS 上に残るが、`CARGO_TARGET_DIR=/var/lib/celeris/build-cache/cargo/agent-platform-dev` を `~/.cargo/config.toml` の `[build] target-dir` で指定してビルド生成物をローカルへ。

## 戻し方
`pct stop 100` → `100.conf` の `mp0` 行を消し、旧 `/home` ボリュームの行を戻す → idmap 行を消す（`/var/lib/celeris` の所有者を 101001 に戻す）→ `pct start 100`。TrueNAS 側のデータはそのまま残る。

## 既知の注意
- `~/.claude` / `~/.codex` の状態ファイル（JSON / SQLite）は NFS 上でも概ね動くが、同時アクセスがあると壊れうる。問題が出たら `/var/lib/celeris/cli-state/` に置いて symlink する。
- docker（groups に docker）を使うなら、コンテナのボリュームはホームに置かない。
- NFS が落ちると `hard` mount でプロセスが D state になる。TrueNAS のメンテ前は CT を止める。
