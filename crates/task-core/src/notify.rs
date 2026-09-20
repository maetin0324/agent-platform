//! 通知の台帳（ADR-0037。migration 0008 / 0009）。「人の判断が要る」出来事を **1 回だけ** 知らせるための、
//! `(kind, key)` の重複排除だけを持つ小さな表。
//!
//! ここにあるのは型と SQL だけで、**何を知らせるかの判定は celeris（`celeris::notify`）にあり、
//! 送信（HTTP）もそこにある**。task-core はネットワークに出ない（ADR-0001 D2 / DESIGN 原則 1）。
//!
//! `store.rs` は `TaskStore` の supertrait として `NotificationStore` を要求するだけ
//! （`report.rs` / `approval.rs` と同じ形）。
//!
//! `project_id`（migration 0009。ADR-0037 D6 / GUI 依頼 G13i-P1）: GUI が `milestone_ready` /
//! `secretary_reply` から案件へリンクを張れるように、判定（`celeris::notify::scan`）が候補を作った
//! 時点で分かっている案件 id をそのまま台帳に書く。応答時に途中目標から逆引きしない
//! （安い方: 書き込み時に 1 回決めるだけで済む）。

use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::org::ProjectId;
use crate::store::{SqliteStore, StoreError, format_rfc3339, parse_rfc3339};

/// 送信を諦めるまでの試行回数（ADR-0037 D1「最大 3 回、以後は諦めて failed を記録」）。
pub const MAX_NOTIFY_ATTEMPTS: u32 = 3;

/// `[notify] discord_webhook_secret` の既定値（ADR-0037 D2）。
pub const DEFAULT_WEBHOOK_SECRET_ID: &str = "discord-webhook";

/// 1 件の通知の識別子（ULID）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct NotificationId(#[schemars(with = "String")] pub Ulid);

impl NotificationId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for NotificationId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for NotificationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for NotificationId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 知らせる理由（ADR-0037 D1 の 5 種）。**どれも「人の判断が要る」ときだけ**。
/// `result` / `progress` は入れない（SPEC §3.5 の数時間単位の流れは GUI の報告の仕事）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// 途中目標に属する仕事がすべて終端になった（達成の判定と次の Go を人に求める）。`key` = 途中目標 id。
    MilestoneReady,
    /// 未決の認可の要求。`key` = 認可 id。
    ApprovalPending,
    /// タスクが `blocked`（人への質問）。`key` = タスク id。
    QuestionBlocked,
    /// 秘書レベル（level 0）の悪い知らせ。`key` = 報告 id。
    BadNews,
    /// `proposed` の案件に秘書の返事が付いた（人の返事待ち）。`key` = 案件 id。
    SecretaryReply,
}

impl NotificationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::MilestoneReady => "milestone_ready",
            NotificationKind::ApprovalPending => "approval_pending",
            NotificationKind::QuestionBlocked => "question_blocked",
            NotificationKind::BadNews => "bad_news",
            NotificationKind::SecretaryReply => "secretary_reply",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "milestone_ready" => Some(NotificationKind::MilestoneReady),
            "approval_pending" => Some(NotificationKind::ApprovalPending),
            "question_blocked" => Some(NotificationKind::QuestionBlocked),
            "bad_news" => Some(NotificationKind::BadNews),
            "secretary_reply" => Some(NotificationKind::SecretaryReply),
            _ => None,
        }
    }

    /// 判定の順（GUI と再送の順を決定的にするため）。
    pub const ALL: [NotificationKind; 5] = [
        NotificationKind::MilestoneReady,
        NotificationKind::ApprovalPending,
        NotificationKind::QuestionBlocked,
        NotificationKind::BadNews,
        NotificationKind::SecretaryReply,
    ];
}

impl std::fmt::Display for NotificationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `notifications` の 1 行。**webhook の URL は入らない**（秘密なので。ADR-0037 D3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Notification {
    pub id: NotificationId,
    pub kind: NotificationKind,
    /// 重複排除の鍵（途中目標 id / 認可 id / タスク id / 報告 id / 案件 id）。
    pub key: String,
    /// 送る文面（決定的な定型文）。
    #[serde(default)]
    pub body: String,
    /// GUI がリンクを作るための案件 id（ADR-0037 D6）。`milestone_ready` はその途中目標の案件、
    /// `secretary_reply` はその案件自身、他の種は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    #[schemars(with = "Option<String>")]
    pub sent_at: Option<OffsetDateTime>,
    /// POST を試した回数。
    pub attempts: u32,
    /// `None` = まだ決着していない（次の tick で再送）、`Some(true)` = 送れた、`Some(false)` = 諦めた。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// 最後の失敗の理由（URL・ホスト名は入れない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Notification {
    /// まだ送れても諦めてもいない（次の tick で送る対象）。
    pub fn is_pending(&self) -> bool {
        self.ok.is_none()
    }

    /// これ以上試さない（`attempts` が上限に達した）。
    pub fn exhausted(&self) -> bool {
        self.attempts >= MAX_NOTIFY_ATTEMPTS
    }
}

/// ADR-0037 D1: `notifications` 表の読み書き。`TaskStore` の supertrait で、実装は `SqliteStore` のみ。
pub trait NotificationStore: Send + Sync {
    /// `(kind, key)` がまだ無ければ pending の行を 1 件作り、その行を返す。既にあれば `None`
    /// （**判定は tick ごとに何度走ってもよい**: 2 回目以降は何も起こらない）。
    fn notification_upsert_pending(
        &self,
        kind: NotificationKind,
        key: &str,
        body: &str,
        project_id: Option<ProjectId>,
        at: OffsetDateTime,
    ) -> Result<Option<Notification>, StoreError>;

    /// 送信の結果を書く。
    ///
    /// - `ok = Some(true)`: 送れた（`sent_at = at`、`ok = 1`、`attempts += 1`）。
    /// - `ok = None`: 失敗したがまだ諦めない（`attempts += 1`、`error` を記録、`ok` は NULL のまま）。
    /// - `ok = Some(false)`: 諦めた（`ok = 0`、`error` を記録、`attempts` は増やさない）。
    ///
    /// 「何回で諦めるか」は呼び出し側（celeris）の方針で、ここは言われたとおりに書くだけ。
    /// 無い id は `Ok(false)`。
    fn notification_mark(
        &self,
        id: NotificationId,
        ok: Option<bool>,
        error: Option<&str>,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError>;

    /// 新しい順（`created_at` 降順、同値は id 降順）に最大 `limit` 件（GUI の「直近の送信」）。
    fn notification_recent(&self, limit: usize) -> Result<Vec<Notification>, StoreError>;

    /// まだ決着していない行（`ok IS NULL`）を古い順に返す（次に送る対象）。
    fn notification_pending(&self) -> Result<Vec<Notification>, StoreError>;
}

const SELECT_NOTIFICATION: &str = "SELECT id, kind, key, body, created_at, sent_at, attempts, ok, error, project_id FROM notifications";

fn row_to_notification(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<Notification, StoreError>> {
    let id: String = row.get(0)?;
    let kind_col: String = row.get(1)?;
    let key: String = row.get(2)?;
    let body: String = row.get(3)?;
    let created_at: String = row.get(4)?;
    let sent_at: Option<String> = row.get(5)?;
    let attempts: i64 = row.get(6)?;
    let ok: Option<i64> = row.get(7)?;
    let error: Option<String> = row.get(8)?;
    let project_id_col: Option<String> = row.get(9)?;
    let Ok(id) = id.parse::<NotificationId>() else {
        return Ok(Err(StoreError::Invalid(format!(
            "invalid notification id: {id}"
        ))));
    };
    let Some(kind) = NotificationKind::parse(&kind_col) else {
        return Ok(Err(StoreError::Invalid(format!(
            "invalid notification kind: {kind_col}"
        ))));
    };
    let project_id = match project_id_col {
        Some(raw) => match raw.parse::<ProjectId>() {
            Ok(id) => Some(id),
            Err(_) => {
                return Ok(Err(StoreError::Invalid(format!(
                    "invalid notification project_id: {raw}"
                ))));
            }
        },
        None => None,
    };
    Ok((|| {
        Ok(Notification {
            id,
            kind,
            key,
            body,
            project_id,
            created_at: parse_rfc3339(&created_at)?,
            sent_at: match sent_at {
                Some(raw) => Some(parse_rfc3339(&raw)?),
                None => None,
            },
            attempts: u32::try_from(attempts).unwrap_or(u32::MAX),
            ok: ok.map(|v| v != 0),
            error,
        })
    })())
}

impl NotificationStore for SqliteStore {
    fn notification_upsert_pending(
        &self,
        kind: NotificationKind,
        key: &str,
        body: &str,
        project_id: Option<ProjectId>,
        at: OffsetDateTime,
    ) -> Result<Option<Notification>, StoreError> {
        let conn = self.lock()?;
        let id = NotificationId::new();
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO notifications (id, kind, key, body, created_at, attempts, project_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            params![id.to_string(), kind.as_str(), key, body, format_rfc3339(at)?, project_id.map(|p| p.to_string())],
        )?;
        if inserted == 0 {
            return Ok(None);
        }
        let row = conn
            .query_row(
                &format!("{SELECT_NOTIFICATION} WHERE id = ?1"),
                params![id.to_string()],
                row_to_notification,
            )
            .optional()?;
        row.transpose()
    }

    fn notification_mark(
        &self,
        id: NotificationId,
        ok: Option<bool>,
        error: Option<&str>,
        at: OffsetDateTime,
    ) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let changed = match ok {
            Some(true) => conn.execute(
                "UPDATE notifications SET attempts = attempts + 1, sent_at = ?2, ok = 1, error = NULL \
                 WHERE id = ?1",
                params![id.to_string(), format_rfc3339(at)?],
            )?,
            None => conn.execute(
                "UPDATE notifications SET attempts = attempts + 1, error = ?2 WHERE id = ?1",
                params![id.to_string(), error],
            )?,
            Some(false) => conn.execute(
                "UPDATE notifications SET ok = 0, error = ?2 WHERE id = ?1",
                params![id.to_string(), error],
            )?,
        };
        Ok(changed > 0)
    }

    fn notification_recent(&self, limit: usize) -> Result<Vec<Notification>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "{SELECT_NOTIFICATION} ORDER BY created_at DESC, id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
            row_to_notification,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    fn notification_pending(&self) -> Result<Vec<Notification>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "{SELECT_NOTIFICATION} WHERE ok IS NULL ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], row_to_notification)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteStore {
        SqliteStore::open(std::path::Path::new(":memory:")).expect("open")
    }

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000 + secs).expect("ts")
    }

    #[test]
    fn kind_spellings_round_trip() {
        for kind in NotificationKind::ALL {
            assert_eq!(NotificationKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(NotificationKind::parse("result"), None);
    }

    #[test]
    fn upsert_is_idempotent_per_kind_and_key() {
        let store = store();
        let first = store
            .notification_upsert_pending(NotificationKind::BadNews, "k1", "body", None, at(0))
            .expect("upsert");
        let first = first.expect("a new row");
        assert_eq!(first.kind, NotificationKind::BadNews);
        assert_eq!(first.key, "k1");
        assert_eq!(first.body, "body");
        assert_eq!(first.attempts, 0);
        assert!(first.is_pending());

        // 同じ (kind, key) は 2 回目以降は作らない。
        let second = store
            .notification_upsert_pending(NotificationKind::BadNews, "k1", "body", None, at(1))
            .expect("upsert");
        assert!(second.is_none());
        assert_eq!(store.notification_pending().expect("pending").len(), 1);

        // kind が違えば別物。
        let other = store
            .notification_upsert_pending(
                NotificationKind::QuestionBlocked,
                "k1",
                "body",
                None,
                at(2),
            )
            .expect("upsert");
        assert!(other.is_some());
        assert_eq!(store.notification_pending().expect("pending").len(), 2);
    }

    #[test]
    fn mark_records_success_failure_and_giving_up() {
        let store = store();
        let row = store
            .notification_upsert_pending(NotificationKind::ApprovalPending, "a1", "b", None, at(0))
            .expect("upsert")
            .expect("row");

        // 失敗（まだ諦めない）: attempts が増え、pending のまま。
        assert!(
            store
                .notification_mark(row.id, None, Some("timeout"), at(1))
                .expect("mark")
        );
        let pending = store.notification_pending().expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].attempts, 1);
        assert_eq!(pending[0].error.as_deref(), Some("timeout"));
        assert!(pending[0].ok.is_none());

        // 諦め: ok = Some(false) で pending から外れる。
        assert!(
            store
                .notification_mark(row.id, Some(false), Some("gave up"), at(2))
                .expect("mark")
        );
        assert!(store.notification_pending().expect("pending").is_empty());
        let recent = store.notification_recent(10).expect("recent");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].ok, Some(false));
        assert_eq!(recent[0].attempts, 1);
        assert!(recent[0].sent_at.is_none());

        // 成功: sent_at が入り ok = true、error は消える。
        let sent = store
            .notification_upsert_pending(NotificationKind::MilestoneReady, "m1", "b", None, at(3))
            .expect("upsert")
            .expect("row");
        assert!(
            store
                .notification_mark(sent.id, Some(true), None, at(4))
                .expect("mark")
        );
        let recent = store.notification_recent(10).expect("recent");
        let found = recent.iter().find(|n| n.id == sent.id).expect("row");
        assert_eq!(found.ok, Some(true));
        assert_eq!(found.sent_at, Some(at(4)));
        assert_eq!(found.attempts, 1);
        assert!(found.error.is_none());

        // 無い id は false。
        assert!(
            !store
                .notification_mark(NotificationId::new(), Some(true), None, at(5))
                .expect("mark")
        );
    }

    #[test]
    fn recent_is_newest_first_and_pending_is_oldest_first() {
        let store = store();
        for (i, key) in ["k1", "k2", "k3"].iter().enumerate() {
            store
                .notification_upsert_pending(
                    NotificationKind::BadNews,
                    key,
                    "b",
                    None,
                    at(i as i64),
                )
                .expect("upsert");
        }
        let recent = store.notification_recent(2).expect("recent");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].key, "k3");
        assert_eq!(recent[1].key, "k2");

        let pending = store.notification_pending().expect("pending");
        assert_eq!(
            pending.iter().map(|n| n.key.as_str()).collect::<Vec<_>>(),
            ["k1", "k2", "k3"]
        );
    }

    #[test]
    fn exhausted_reports_the_attempt_limit() {
        let store = store();
        let row = store
            .notification_upsert_pending(NotificationKind::SecretaryReply, "p1", "b", None, at(0))
            .expect("upsert")
            .expect("row");
        for i in 0..MAX_NOTIFY_ATTEMPTS {
            store
                .notification_mark(row.id, None, Some("no"), at(i64::from(i) + 1))
                .expect("mark");
        }
        let pending = store.notification_pending().expect("pending");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].exhausted());
    }
}
