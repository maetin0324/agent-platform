//! CLI（Claude Code / Codex）が書いた OAuth 認証情報ファイルをそのまま読み、更新も同じファイルに書き戻す
//! （ADR-0053 D1: DB や別の場所に写さない）。**値はどの `Debug`/ログにも出さない**。
//!
//! ファイルの形は固定のフィクスチャで検証する（このセッションでは本物の資格情報ファイルを読まない）:
//! - Claude: `{"claudeAiOauth": {"accessToken", "refreshToken", "expiresAt", "scopes", "subscriptionType"}}`
//! - Codex: `{"OPENAI_API_KEY", "tokens": {"id_token", "access_token", "refresh_token", "account_id"}, "last_refresh"}`

use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("cannot read credentials file")]
    Read(#[source] std::io::Error),
    #[error("credentials file is not valid JSON")]
    Parse(#[source] serde_json::Error),
    #[error("credentials file is missing an expected field: {0}")]
    Shape(&'static str),
    #[error("cannot write credentials file")]
    Write(#[source] std::io::Error),
}

/// `path` の JSON を読む。値そのものはこの関数の戻り値以外どこにも出さない。
pub fn read_json(path: &Path) -> Result<serde_json::Value, CredentialError> {
    let text = fs::read_to_string(path).map_err(CredentialError::Read)?;
    serde_json::from_str(&text).map_err(CredentialError::Parse)
}

/// `<path>.tmp` に書いてから rename し、mode を 0600 にする（ADR-0053 D1: 更新は同じファイルへ）。
pub fn write_json_atomic(path: &Path, value: &serde_json::Value) -> Result<(), CredentialError> {
    let json = serde_json::to_vec_pretty(value).map_err(CredentialError::Parse)?;
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp_name);
    fs::write(&tmp_path, &json).map_err(CredentialError::Write)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))
            .map_err(CredentialError::Write)?;
    }
    fs::rename(&tmp_path, path).map_err(CredentialError::Write)?;
    Ok(())
}

/// Claude Code の `.credentials.json` から取り出した値。`Debug` はトークンを出さない。
#[derive(Clone)]
pub struct ClaudeTokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix ミリ秒（`expiresAt`）。
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for ClaudeTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

pub const CLAUDE_CREDENTIALS_FILE: &str = ".credentials.json";

pub fn parse_claude_tokens(value: &serde_json::Value) -> Result<ClaudeTokens, CredentialError> {
    let oauth = value
        .get("claudeAiOauth")
        .ok_or(CredentialError::Shape("claudeAiOauth"))?;
    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or(CredentialError::Shape("claudeAiOauth.accessToken"))?
        .to_string();
    let refresh_token = oauth
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .ok_or(CredentialError::Shape("claudeAiOauth.refreshToken"))?
        .to_string();
    let expires_at_ms = oauth
        .get("expiresAt")
        .and_then(|v| v.as_i64())
        .ok_or(CredentialError::Shape("claudeAiOauth.expiresAt"))?;
    Ok(ClaudeTokens {
        access_token,
        refresh_token,
        expires_at_ms,
    })
}

/// `value` の `claudeAiOauth` の 3 フィールドだけを書き換える（`scopes`/`subscriptionType` 等はそのまま残す）。
pub fn apply_claude_tokens(value: &mut serde_json::Value, tokens: &ClaudeTokens) {
    if let Some(oauth) = value
        .get_mut("claudeAiOauth")
        .and_then(|v| v.as_object_mut())
    {
        oauth.insert(
            "accessToken".to_string(),
            tokens.access_token.clone().into(),
        );
        oauth.insert(
            "refreshToken".to_string(),
            tokens.refresh_token.clone().into(),
        );
        oauth.insert("expiresAt".to_string(), tokens.expires_at_ms.into());
    }
}

/// Codex の `auth.json` から取り出した値。`expires_at` は無い（`last_refresh` は RFC3339 の時刻のみで、
/// 期限は分からない。ADR-0053 のフィクスチャに expires 相当が無いため、更新は 401 を受けての事後更新に限る。
/// `docs/llm-source.md` に明記）。
#[derive(Clone)]
pub struct CodexTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
    #[allow(dead_code)]
    pub id_token: String,
}

impl std::fmt::Debug for CodexTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("id_token", &"<redacted>")
            .field("account_id", &self.account_id)
            .finish()
    }
}

pub const CODEX_CREDENTIALS_FILE: &str = "auth.json";

pub fn parse_codex_tokens(value: &serde_json::Value) -> Result<CodexTokens, CredentialError> {
    let tokens = value
        .get("tokens")
        .ok_or(CredentialError::Shape("tokens"))?;
    let access_token = tokens
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or(CredentialError::Shape("tokens.access_token"))?
        .to_string();
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .ok_or(CredentialError::Shape("tokens.refresh_token"))?
        .to_string();
    let account_id = tokens
        .get("account_id")
        .and_then(|v| v.as_str())
        .ok_or(CredentialError::Shape("tokens.account_id"))?
        .to_string();
    let id_token = tokens
        .get("id_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(CodexTokens {
        access_token,
        refresh_token,
        account_id,
        id_token,
    })
}

/// `value` の `tokens.*` を書き換え、`last_refresh` を今の RFC3339 にする。
pub fn apply_codex_tokens(value: &mut serde_json::Value, tokens: &CodexTokens, now_rfc3339: &str) {
    if let Some(t) = value.get_mut("tokens").and_then(|v| v.as_object_mut()) {
        t.insert(
            "access_token".to_string(),
            tokens.access_token.clone().into(),
        );
        t.insert(
            "refresh_token".to_string(),
            tokens.refresh_token.clone().into(),
        );
        t.insert("id_token".to_string(), tokens.id_token.clone().into());
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("last_refresh".to_string(), now_rfc3339.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn claude_fixture() -> serde_json::Value {
        serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "fake-access-abc",
                "refreshToken": "fake-refresh-abc",
                "expiresAt": 1_800_000_000_000i64,
                "scopes": ["user:inference"],
                "subscriptionType": "max"
            }
        })
    }

    fn codex_fixture() -> serde_json::Value {
        serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "fake-id",
                "access_token": "fake-access",
                "refresh_token": "fake-refresh",
                "account_id": "acct-123"
            },
            "last_refresh": "2026-09-21T00:00:00Z"
        })
    }

    #[test]
    fn parses_claude_fixture() {
        let tokens = parse_claude_tokens(&claude_fixture()).unwrap();
        assert_eq!(tokens.access_token, "fake-access-abc");
        assert_eq!(tokens.expires_at_ms, 1_800_000_000_000);
        // Debug は値を出さない。
        assert!(!format!("{tokens:?}").contains("fake-access-abc"));
    }

    #[test]
    fn parses_codex_fixture() {
        let tokens = parse_codex_tokens(&codex_fixture()).unwrap();
        assert_eq!(tokens.account_id, "acct-123");
        assert_eq!(tokens.access_token, "fake-access");
        assert!(!format!("{tokens:?}").contains("fake-access"));
    }

    #[test]
    fn missing_field_is_a_shape_error() {
        let bad = serde_json::json!({"claudeAiOauth": {"accessToken": "a"}});
        assert!(matches!(
            parse_claude_tokens(&bad),
            Err(CredentialError::Shape("claudeAiOauth.refreshToken"))
        ));
    }

    #[test]
    fn write_back_preserves_unknown_fields_and_updates_only_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CLAUDE_CREDENTIALS_FILE);
        write_json_atomic(&path, &claude_fixture()).unwrap();
        assert!(
            !dir.path()
                .join(format!("{CLAUDE_CREDENTIALS_FILE}.tmp"))
                .exists()
        );

        let mut value = read_json(&path).unwrap();
        let new_tokens = ClaudeTokens {
            access_token: "new-access".into(),
            refresh_token: "new-refresh".into(),
            expires_at_ms: 1_900_000_000_000,
        };
        apply_claude_tokens(&mut value, &new_tokens);
        write_json_atomic(&path, &value).unwrap();

        let reloaded = read_json(&path).unwrap();
        assert_eq!(reloaded["claudeAiOauth"]["accessToken"], "new-access");
        assert_eq!(reloaded["claudeAiOauth"]["subscriptionType"], "max");
        assert_eq!(
            reloaded["claudeAiOauth"]["scopes"],
            serde_json::json!(["user:inference"])
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn codex_write_back_updates_last_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CODEX_CREDENTIALS_FILE);
        write_json_atomic(&path, &codex_fixture()).unwrap();
        let mut value = read_json(&path).unwrap();
        let tokens = CodexTokens {
            access_token: "new-access".into(),
            refresh_token: "new-refresh".into(),
            account_id: "acct-123".into(),
            id_token: "new-id".into(),
        };
        apply_codex_tokens(&mut value, &tokens, "2026-09-21T01:00:00Z");
        write_json_atomic(&path, &value).unwrap();
        let reloaded = read_json(&path).unwrap();
        assert_eq!(reloaded["tokens"]["access_token"], "new-access");
        assert_eq!(reloaded["last_refresh"], "2026-09-21T01:00:00Z");
        assert_eq!(reloaded["OPENAI_API_KEY"], serde_json::Value::Null);
    }
}
