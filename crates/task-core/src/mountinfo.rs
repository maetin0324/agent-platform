//! `/proc/self/mountinfo` を読んでマウント点のファイルシステム種別・ソースを引くユーティリティ
//! （ADR-0065 D1）。`celeris`（起動時の警告）と `task-api`（`GET /health` の `db.filesystem` /
//! `db.device`）の両方から使う、決定的な純関数だけを置く（I/O はしない。呼び出し側が
//! `/proc/self/mountinfo` を読んで渡す）。

use std::path::Path;

/// ADR-0013 D5 の前提（DB はローカルディスク）を破っている場合に警告するための、ネットワーク FS の一覧。
pub const NETWORK_FILESYSTEMS: &[&str] = &[
    "nfs", "nfs4", "cifs", "smb3", "9p", "afs", "ceph", "lustre", "gpfs", "beegfs", "glusterfs",
];

/// マウント点のファイルシステム種別とマウントソース（`/proc/self/mountinfo` の `-` の後の
/// 1・2 番目のフィールド）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    pub fstype: String,
    /// マウントソース。ループマウントなら `/dev/loop0` のような値になる。
    pub source: String,
}

/// `fstype` がネットワークファイルシステム（NFS・CIFS・FUSE 経由のリモートマウント等）かどうか。
pub fn is_network_filesystem(fstype: &str) -> bool {
    NETWORK_FILESYSTEMS.contains(&fstype) || fstype.starts_with("fuse.")
}

/// ADR-0065 D1: マウントソースが loop デバイス（`/dev/loop*`）かどうか。実体は raw イメージファイル
/// で、そのイメージ自体がネットワーク越し（NFS 上のファイル）の可能性を排除できない。
pub fn is_loop_device(source: &str) -> bool {
    source.starts_with("/dev/loop")
}

/// `/proc/self/mountinfo` の内容から、`target` を含む最長一致のマウント点の情報を返す。
/// `mountinfo` の各行は `... <mount_point> ... - <fstype> <source> <options>` の形
/// （`man 5 proc_pid_mountinfo`）。同じマウント点に複数行あれば後の行（実体）を優先する。
pub fn mount_info_for(mountinfo: &str, target: &Path) -> Option<MountInfo> {
    let mut best: Option<(usize, MountInfo)> = None;
    for line in mountinfo.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mount_point) = before.split_whitespace().nth(4) else {
            continue;
        };
        let mut fields = after.split_whitespace();
        let Some(fstype) = fields.next() else {
            continue;
        };
        let Some(source) = fields.next() else {
            continue;
        };
        if target.starts_with(mount_point)
            && best
                .as_ref()
                .is_none_or(|(len, _)| mount_point.len() >= *len)
        {
            best = Some((
                mount_point.len(),
                MountInfo {
                    fstype: fstype.to_string(),
                    source: source.to_string(),
                },
            ));
        }
    }
    best.map(|(_, info)| info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_info_for_picks_the_longest_matching_mount_point() {
        let mountinfo = "\
25 30 0:24 / / rw,relatime shared:1 - ext4 /dev/mapper/root rw
26 25 0:52 / /home rw,relatime shared:2 - nfs4 server:/home rw,vers=4.2
27 26 0:53 / /home/u/local rw,relatime shared:3 - ext4 /dev/sdb1 rw";
        assert_eq!(
            mount_info_for(mountinfo, Path::new("/var/lib/celeris")),
            Some(MountInfo {
                fstype: "ext4".into(),
                source: "/dev/mapper/root".into()
            })
        );
        assert_eq!(
            mount_info_for(mountinfo, Path::new("/home/u/workspace")),
            Some(MountInfo {
                fstype: "nfs4".into(),
                source: "server:/home".into()
            })
        );
        assert_eq!(
            mount_info_for(mountinfo, Path::new("/home/u/local/db")),
            Some(MountInfo {
                fstype: "ext4".into(),
                source: "/dev/sdb1".into()
            })
        );
        assert_eq!(mount_info_for("garbage", Path::new("/home")), None);
    }

    #[test]
    fn mount_info_for_prefers_the_later_line_at_the_same_mount_point() {
        // 同じマウント点に autofs と実体が並ぶ場合は後の行（実体）を採る。
        let autofs_first = "\
25 30 0:24 / / rw,relatime shared:1 - ext4 /dev/mapper/root rw
26 25 0:51 / /home rw,relatime shared:2 - autofs systemd-1 rw
27 25 0:52 / /home rw,relatime shared:3 - nfs4 server:/home rw,vers=4.2";
        assert_eq!(
            mount_info_for(autofs_first, Path::new("/home/u/x")).map(|i| i.fstype),
            Some("nfs4".to_string())
        );
    }

    #[test]
    fn network_filesystem_and_loop_device_classification() {
        assert!(is_network_filesystem("nfs4"));
        assert!(is_network_filesystem("cifs"));
        assert!(is_network_filesystem("fuse.sshfs"));
        assert!(!is_network_filesystem("ext4"));

        assert!(is_loop_device("/dev/loop0"));
        assert!(is_loop_device("/dev/loop12"));
        assert!(!is_loop_device("/dev/sdb1"));
        assert!(!is_loop_device("server:/home"));
    }
}
