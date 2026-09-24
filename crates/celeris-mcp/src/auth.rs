//! ADR-0056 D1 / D4（Phase 78）: 口ごとの認証。決定的（LLM も I/O 以外の判断も無い。DESIGN 原則 1）。

use sha2::{Digest, Sha256};
use task_core::{McpScope, TaskStore};
use time::OffsetDateTime;

use crate::config::ListenerAuth;

/// トークンの値から DB に残す形（SHA-256 の 16 進）を作る。**値そのものはここにしか通らない**
/// （呼び出し側はこの結果だけを保存・比較する）。
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// `celerisctl mcp client add` が発行するトークン（新しいクレートを足さないため、乱数は
/// `ulid`（OS の乱数源を使う thread-local RNG）に頼る。3 本つなげて十分な当てずっぽう耐性を持たせる）。
pub fn generate_token() -> String {
    format!(
        "{}{}{}",
        ulid::Ulid::new(),
        ulid::Ulid::new(),
        ulid::Ulid::new()
    )
    .to_lowercase()
}

/// 認証を通ったクライアント（スコープ込み。トークンの値は持たない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthedClient {
    pub id: String,
    pub name: String,
    pub scopes: Vec<McpScope>,
}

impl AuthedClient {
    pub fn has_scope(&self, scope: McpScope) -> bool {
        self.scopes.contains(&scope)
    }
}

/// 認証の失敗（HTTP 401 に写す。理由は応答に出さない — トークンの手掛かりを与えないため）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// `auth = "token"` の口で `Authorization: Bearer <token>` が無い・形が違う。
    MissingToken,
    /// トークンがどのクライアントとも一致しない。
    InvalidToken,
    /// クライアントが失効している。
    Revoked,
    /// `auth = "none"` の口が指す `client` が存在しない（設定ミス）。
    UnknownFixedClient,
}

/// 口の認証方式にしたがってクライアントを決める。成功したら `last_used_at` を更新する。
pub fn authenticate(
    store: &dyn TaskStore,
    listener_auth: &ListenerAuth,
    bearer: Option<&str>,
    now: OffsetDateTime,
) -> Result<AuthedClient, AuthError> {
    let client = match listener_auth {
        ListenerAuth::Token => {
            let token = bearer
                .filter(|t| !t.is_empty())
                .ok_or(AuthError::MissingToken)?;
            let hash = hash_token(token);
            store
                .mcp_client_by_token_hash(&hash)
                .ok()
                .flatten()
                .ok_or(AuthError::InvalidToken)?
        }
        ListenerAuth::Fixed(client_id) => store
            .mcp_client_get(client_id)
            .ok()
            .flatten()
            .ok_or(AuthError::UnknownFixedClient)?,
    };
    if client.is_revoked() {
        return Err(AuthError::Revoked);
    }
    let _ = store.mcp_client_touch_last_used(&client.id, now);
    Ok(AuthedClient {
        id: client.id,
        name: client.name,
        scopes: client.scopes,
    })
}

/// `Authorization: Bearer <token>` からトークンの値だけを取り出す。
pub fn bearer_token(value: Option<&str>) -> Option<&str> {
    value?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_is_long_and_not_repeated() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
        assert!(a.len() >= 60, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }
    use task_core::{McpClient, McpClientStore, SqliteStore};

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().expect("open")
    }

    fn client(id: &str, token: Option<&str>, scopes: Vec<McpScope>, revoked: bool) -> McpClient {
        let now = OffsetDateTime::now_utc();
        McpClient {
            id: id.to_string(),
            name: id.to_string(),
            token_hash: token.map(hash_token),
            scopes,
            created_at: now,
            last_used_at: None,
            revoked_at: if revoked { Some(now) } else { None },
        }
    }

    #[test]
    fn token_listener_accepts_a_matching_bearer_token() {
        let store = store();
        store
            .mcp_client_create(&client(
                "c1",
                Some("secret"),
                vec![McpScope::KnowledgeRead],
                false,
            ))
            .unwrap();
        let authed = authenticate(
            &store,
            &ListenerAuth::Token,
            Some("secret"),
            OffsetDateTime::now_utc(),
        )
        .expect("authed");
        assert_eq!(authed.id, "c1");
        assert!(authed.has_scope(McpScope::KnowledgeRead));
    }

    #[test]
    fn token_listener_rejects_missing_wrong_or_revoked_token() {
        let store = store();
        store
            .mcp_client_create(&client("c1", Some("secret"), vec![], false))
            .unwrap();
        store
            .mcp_client_create(&client("c2", Some("secret2"), vec![], false))
            .unwrap();
        store
            .mcp_client_revoke("c2", OffsetDateTime::now_utc())
            .unwrap();
        assert_eq!(
            authenticate(
                &store,
                &ListenerAuth::Token,
                None,
                OffsetDateTime::now_utc()
            ),
            Err(AuthError::MissingToken)
        );
        assert_eq!(
            authenticate(
                &store,
                &ListenerAuth::Token,
                Some("wrong"),
                OffsetDateTime::now_utc()
            ),
            Err(AuthError::InvalidToken)
        );
        assert_eq!(
            authenticate(
                &store,
                &ListenerAuth::Token,
                Some("secret2"),
                OffsetDateTime::now_utc()
            ),
            Err(AuthError::Revoked)
        );
    }

    #[test]
    fn a_no_token_client_can_never_authenticate_on_a_token_listener() {
        let store = store();
        store
            .mcp_client_create(&client("c1", None, vec![], false))
            .unwrap();
        // トークンが無いので、どんな値を当てても一致しない（ハッシュ比較が `NULL` に当たらない）。
        assert_eq!(
            authenticate(
                &store,
                &ListenerAuth::Token,
                Some(""),
                OffsetDateTime::now_utc()
            ),
            Err(AuthError::MissingToken)
        );
        assert!(matches!(
            authenticate(
                &store,
                &ListenerAuth::Token,
                Some("anything"),
                OffsetDateTime::now_utc()
            ),
            Err(AuthError::InvalidToken)
        ));
    }

    #[test]
    fn fixed_listener_binds_to_the_named_client_ignoring_bearer() {
        let store = store();
        store
            .mcp_client_create(&client(
                "chatgpt",
                None,
                vec![McpScope::KnowledgePropose],
                false,
            ))
            .unwrap();
        let authed = authenticate(
            &store,
            &ListenerAuth::Fixed("chatgpt".to_string()),
            None,
            OffsetDateTime::now_utc(),
        )
        .expect("authed");
        assert_eq!(authed.id, "chatgpt");
        assert!(authed.has_scope(McpScope::KnowledgePropose));
    }

    #[test]
    fn fixed_listener_with_unknown_or_revoked_client_fails() {
        let store = store();
        assert_eq!(
            authenticate(
                &store,
                &ListenerAuth::Fixed("nope".to_string()),
                None,
                OffsetDateTime::now_utc()
            ),
            Err(AuthError::UnknownFixedClient)
        );
        store
            .mcp_client_create(&client("chatgpt", None, vec![], false))
            .unwrap();
        store
            .mcp_client_revoke("chatgpt", OffsetDateTime::now_utc())
            .unwrap();
        assert_eq!(
            authenticate(
                &store,
                &ListenerAuth::Fixed("chatgpt".to_string()),
                None,
                OffsetDateTime::now_utc()
            ),
            Err(AuthError::Revoked)
        );
    }

    #[test]
    fn bearer_token_parses_the_header_value() {
        assert_eq!(bearer_token(Some("Bearer abc")), Some("abc"));
        assert_eq!(bearer_token(Some("Bearer  abc")), Some("abc"));
        assert_eq!(bearer_token(Some("abc")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None);
        assert_eq!(bearer_token(None), None);
    }
}
