//! ADR-0040 D4（Phase 47）: celeris の「インスタンスの役割」。`daemon_instances` の 1 行 = 1 プロセス。
//!
//! ここにあるのは**型と行の変換だけ**で、役割を決める規則（誰が active になるか、いつ drain するか）は
//! celeris 側（`celeris::instance`）にある。判断は決定的で、LLM もワーカーも関与しない（DESIGN 原則 1〜4）。

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::store::{StoreError, parse_rfc3339};

/// インスタンスの役割（ADR-0040 D4）。`daemon_instances.role` の綴りと 1 対 1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstanceRole {
    /// dispatch する。tick の裏方（通知・報告・途中目標レビュー・クラスタ・アカウント）を動かす。常に 1 つだけ。
    Active,
    /// API は受けるが dispatch も裏方もしない。ディスパッチャの状態を要する管理 API は 503。
    Standby,
    /// listener を閉じ、手元の run とレビューだけ面倒を見る。0 になったら exit 0。
    Draining,
    /// `--mode verify`。dispatch も裏方も無し。`daemon_instances` に行を書かない。
    Verify,
}

impl InstanceRole {
    pub const ALL: [InstanceRole; 4] = [Self::Active, Self::Standby, Self::Draining, Self::Verify];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Standby => "standby",
            Self::Draining => "draining",
            Self::Verify => "verify",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "standby" => Some(Self::Standby),
            "draining" => Some(Self::Draining),
            "verify" => Some(Self::Verify),
            _ => None,
        }
    }

    /// `SharedRole` が使う数値表現（`AtomicU8`）。
    fn as_u8(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Standby => 1,
            Self::Draining => 2,
            Self::Verify => 3,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Standby,
            2 => Self::Draining,
            3 => Self::Verify,
            _ => Self::Active,
        }
    }
}

impl std::fmt::Display for InstanceRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `--mode`（ADR-0040 D3）。`verify` は本番のデータのコピーに対する検証専用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DaemonMode {
    #[default]
    Normal,
    Verify,
}

impl DaemonMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Verify => "verify",
        }
    }
}

impl std::fmt::Display for DaemonMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// tick ループと API が共有する「いまの役割」（ADR-0040 D4: standby の間は管理 API が 503）。
/// tick ループだけが書き、API は読むだけ。
#[derive(Debug, Clone)]
pub struct SharedRole(Arc<AtomicU8>);

impl SharedRole {
    pub fn new(role: InstanceRole) -> Self {
        Self(Arc::new(AtomicU8::new(role.as_u8())))
    }

    pub fn get(&self) -> InstanceRole {
        InstanceRole::from_u8(self.0.load(Ordering::SeqCst))
    }

    pub fn set(&self, role: InstanceRole) {
        self.0.store(role.as_u8(), Ordering::SeqCst);
    }

    /// ADR-0040 D4: ディスパッチャの状態を要する管理 API を受けられるのは `active` だけ
    /// （`standby` と `draining` は 503 `standby`）。
    pub fn accepts_admin(&self) -> bool {
        matches!(self.get(), InstanceRole::Active | InstanceRole::Verify)
    }
}

impl Default for SharedRole {
    fn default() -> Self {
        Self::new(InstanceRole::Active)
    }
}

/// `daemon_instances` の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonInstance {
    pub instance_id: String,
    /// `--release <sha12>` / `CELERIS_RELEASE` / `"dev"`。
    pub release: String,
    pub pid: u32,
    pub role: InstanceRole,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub heartbeat_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schemars(with = "Option<String>")]
    pub handoff_requested_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schemars(with = "Option<String>")]
    pub drained_at: Option<OffsetDateTime>,
}

impl DaemonInstance {
    /// `now - heartbeat_at < freshness` なら「生きている」とみなす（ADR-0040 D4）。
    pub fn is_fresh(&self, now: OffsetDateTime, freshness: std::time::Duration) -> bool {
        let age = now - self.heartbeat_at;
        // 未来の heartbeat（時計のずれ）も「新しい」として扱う。
        age < time::Duration::try_from(freshness).unwrap_or(time::Duration::MAX)
    }
}

pub(crate) const SELECT_INSTANCE: &str = "SELECT instance_id, \"release\", pid, role, started_at, heartbeat_at, \
     handoff_requested_at, drained_at FROM daemon_instances";

pub(crate) fn row_to_instance(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<DaemonInstance, StoreError>> {
    let instance_id: String = row.get(0)?;
    let release: String = row.get(1)?;
    let pid: i64 = row.get(2)?;
    let role_col: String = row.get(3)?;
    let started_at: String = row.get(4)?;
    let heartbeat_at: String = row.get(5)?;
    let handoff_requested_at: Option<String> = row.get(6)?;
    let drained_at: Option<String> = row.get(7)?;
    let Some(role) = InstanceRole::parse(&role_col) else {
        return Ok(Err(StoreError::Invalid(format!(
            "invalid daemon instance role: {role_col}"
        ))));
    };
    Ok((|| {
        Ok(DaemonInstance {
            instance_id,
            release,
            pid: u32::try_from(pid).unwrap_or(0),
            role,
            started_at: parse_rfc3339(&started_at)?,
            heartbeat_at: parse_rfc3339(&heartbeat_at)?,
            handoff_requested_at: handoff_requested_at.as_deref().map(parse_rfc3339).transpose()?,
            drained_at: drained_at.as_deref().map(parse_rfc3339).transpose()?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_spellings_round_trip() {
        for role in InstanceRole::ALL {
            assert_eq!(InstanceRole::parse(role.as_str()), Some(role));
            assert_eq!(role.to_string(), role.as_str());
        }
        assert_eq!(InstanceRole::parse("nonsense"), None);
        assert_eq!(DaemonMode::default().as_str(), "normal");
        assert_eq!(DaemonMode::Verify.to_string(), "verify");
    }

    #[test]
    fn shared_role_is_readable_from_another_holder() {
        let role = SharedRole::new(InstanceRole::Standby);
        let copy = role.clone();
        assert_eq!(copy.get(), InstanceRole::Standby);
        assert!(!copy.accepts_admin());
        role.set(InstanceRole::Active);
        assert_eq!(copy.get(), InstanceRole::Active);
        assert!(copy.accepts_admin());
        role.set(InstanceRole::Draining);
        assert!(!copy.accepts_admin());
        // verify は 1 プロセスしかいないので管理 API を断らない（dispatch はしない）。
        role.set(InstanceRole::Verify);
        assert!(copy.accepts_admin());
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::store::{SqliteStore, TaskStore};

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000 + secs).expect("ts")
    }

    fn row(id: &str, release: &str, role: InstanceRole, heartbeat: i64) -> DaemonInstance {
        DaemonInstance {
            instance_id: id.into(),
            release: release.into(),
            pid: 4242,
            role,
            started_at: at(0),
            heartbeat_at: at(heartbeat),
            handoff_requested_at: None,
            drained_at: None,
        }
    }

    /// ADR-0040 D4: 登録 → heartbeat → 引き継ぎ要求 → 役割の変更 → drained → 削除の一連が往復する。
    #[test]
    fn a_row_round_trips_through_the_whole_handoff() {
        let store = SqliteStore::open_in_memory().expect("open");
        assert!(store.instance_list().expect("list").is_empty());

        store.instance_register(&row("old", "aaaaaaaaaaaa", InstanceRole::Active, 0)).expect("register");
        let listed = store.instance_list().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].release, "aaaaaaaaaaaa");
        assert_eq!(listed[0].pid, 4242);
        assert_eq!(listed[0].role, InstanceRole::Active);
        assert_eq!(listed[0].handoff_requested_at, None);

        assert!(store.instance_heartbeat("old", at(5)).expect("heartbeat"));
        assert!(!store.instance_heartbeat("nobody", at(5)).expect("heartbeat"));
        assert_eq!(store.instance_list().expect("list")[0].heartbeat_at, at(5));

        // 引き継ぎの要求は 1 回だけ（2 回目は上書きしない）。
        assert!(store.instance_request_handoff("old", at(6)).expect("handoff"));
        assert!(!store.instance_request_handoff("old", at(9)).expect("handoff"));
        assert_eq!(store.instance_list().expect("list")[0].handoff_requested_at, Some(at(6)));

        assert!(store.instance_set_role("old", InstanceRole::Draining, at(7)).expect("role"));
        let after = &store.instance_list().expect("list")[0];
        assert_eq!((after.role, after.heartbeat_at), (InstanceRole::Draining, at(7)));

        assert!(store.instance_mark_drained("old", at(8)).expect("drained"));
        assert_eq!(store.instance_list().expect("list")[0].drained_at, Some(at(8)));
        assert!(store.instance_delete("old").expect("delete"));
        assert!(!store.instance_delete("old").expect("delete"));
        assert!(store.instance_list().expect("list").is_empty());
    }

    /// ADR-0040 D4: 同じ `instance_id` で登録し直すと `handoff_requested_at` / `drained_at` は消える。
    /// 一覧は `started_at` 昇順。
    #[test]
    fn register_replaces_the_row_and_the_list_is_ordered_by_started_at() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut first = row("a", "r1", InstanceRole::Active, 0);
        first.handoff_requested_at = Some(at(1));
        first.drained_at = Some(at(2));
        store.instance_register(&first).expect("register");
        assert_eq!(store.instance_list().expect("list")[0].drained_at, Some(at(2)));

        let mut again = row("a", "r2", InstanceRole::Standby, 3);
        again.started_at = at(10);
        store.instance_register(&again).expect("register");
        let listed = store.instance_list().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].release.as_str(), listed[0].role), ("r2", InstanceRole::Standby));
        assert_eq!((listed[0].handoff_requested_at, listed[0].drained_at), (None, None));

        store.instance_register(&row("b", "r3", InstanceRole::Active, 0)).expect("register");
        let ids: Vec<String> = store.instance_list().expect("list").into_iter().map(|i| i.instance_id).collect();
        assert_eq!(ids, ["b", "a"], "started_at が古い方が先");
    }

    /// ADR-0040 D4: 終わった（`drained_at`）・死んだ（heartbeat が古い）他の行だけを消す。自分は消さない。
    #[test]
    fn delete_stale_removes_drained_and_dead_rows_but_never_keep() {
        let store = SqliteStore::open_in_memory().expect("open");
        store.instance_register(&row("me", "new", InstanceRole::Active, 100)).expect("register");
        store.instance_register(&row("dead", "old", InstanceRole::Active, 0)).expect("register");
        store.instance_register(&row("live", "other", InstanceRole::Standby, 100)).expect("register");
        store.instance_register(&row("done", "old", InstanceRole::Draining, 100)).expect("register");
        store.instance_mark_drained("done", at(100)).expect("drained");
        // 自分の行が古くても消えない。
        store.instance_heartbeat("me", at(0)).expect("heartbeat");

        let removed = store.instance_delete_stale("me", at(50)).expect("stale");
        assert_eq!(removed, ["dead", "done"]);
        let ids: Vec<String> = store.instance_list().expect("list").into_iter().map(|i| i.instance_id).collect();
        assert_eq!(ids, ["live", "me"], "started_at が同じなら instance_id 昇順");
    }

    /// `is_fresh` は `now - heartbeat_at < freshness`（未来の heartbeat も「新しい」）。
    #[test]
    fn freshness_is_measured_against_the_heartbeat() {
        let inst = row("a", "r", InstanceRole::Active, 0);
        let window = std::time::Duration::from_secs(10);
        assert!(inst.is_fresh(at(9), window));
        assert!(!inst.is_fresh(at(10), window));
        assert!(!inst.is_fresh(at(60), window));
        assert!(inst.is_fresh(at(-60), window));
    }

    /// `role` は CHECK 制約で 4 つの綴りしか入らない。
    #[test]
    fn the_role_column_rejects_unknown_spellings() {
        let store = SqliteStore::open_in_memory().expect("open");
        store.instance_register(&row("a", "r", InstanceRole::Verify, 0)).expect("register");
        assert_eq!(store.instance_list().expect("list")[0].role, InstanceRole::Verify);
        let conn = store.lock().expect("lock");
        let err = conn.execute("UPDATE daemon_instances SET role = 'nonsense' WHERE instance_id = 'a'", []);
        assert!(err.is_err(), "CHECK(role IN (...)) が効いている");
    }
}
