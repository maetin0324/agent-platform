//! `[mcp]`（ADR-0056 D1。Phase 78）: celeris の `Config` に埋め込まれる設定の型。
//!
//! ここは値の入れ物と決定的な検証だけで、判断（認証・ディスパッチ）は持たない（DESIGN 原則 1。
//! `llm_proxy::config` と同じ規律）。

use std::net::SocketAddr;

use serde::Deserialize;

/// `[mcp]`。単一口の糖衣（`listen` / `auth` / `client`）と複数口
/// （`[[mcp.listeners]]`）の両方を受ける。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// 糖衣: これだけ書けば 1 口（既定 `auth = "token"`）になる。
    #[serde(default)]
    pub listen: Option<SocketAddr>,
    /// 糖衣の口の認証（`listen` を書いたときだけ効く）。
    #[serde(default)]
    pub auth: Option<String>,
    /// 糖衣の口が `auth = "none"` のときに固定するクライアント id。
    #[serde(default)]
    pub client: Option<String>,
    /// 複数口（ADR-0056 D1「口は複数持てる」）。
    #[serde(default)]
    pub listeners: Vec<McpListenerConfig>,
    /// クライアントごとの `tools/call` の上限（1 分あたり）。
    #[serde(default = "default_rate_limit_per_min")]
    pub rate_limit_per_min: u32,
}

/// `[[mcp.listeners]]` の 1 口。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpListenerConfig {
    pub listen: SocketAddr,
    #[serde(default = "default_auth")]
    pub auth: String,
    #[serde(default)]
    pub client: Option<String>,
}

fn default_auth() -> String {
    "token".to_string()
}

fn default_rate_limit_per_min() -> u32 {
    60
}

/// 検証を通った 1 口（`celeris` がこれを見て bind する。`celeris-mcp` のルータもこれで挙動を決める）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedListener {
    pub listen: SocketAddr,
    pub auth: ListenerAuth,
}

/// その口の認証の仕方（ADR-0056 D1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerAuth {
    /// `Authorization: Bearer <token>` を検査する（既定）。
    Token,
    /// 認証をしない。**その口に来た要求はすべてこのクライアントとして扱う**
    /// （ChatGPT の Secure MCP tunnel 用。loopback のみ）。
    Fixed(String),
}

/// `[mcp]` の設定エラー（celeris の `Config::validate` が返す）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpConfigError {
    #[error("[mcp]/[[mcp.listeners]]: auth must be \"token\" or \"none\" (found {0:?})")]
    UnknownAuth(String),
    #[error(
        "[mcp]/[[mcp.listeners]] {listen}: auth = \"none\" is only allowed on a loopback address"
    )]
    NoneNotLoopback { listen: SocketAddr },
    #[error("[mcp]/[[mcp.listeners]] {listen}: auth = \"none\" requires `client = \"<id>\"`")]
    NoneWithoutClient { listen: SocketAddr },
    #[error("[mcp]/[[mcp.listeners]] {listen}: `client` is only valid with auth = \"none\"")]
    ClientWithoutNone { listen: SocketAddr },
}

impl McpConfig {
    /// このプロセスで MCP サーバーを起こすか（1 口も無ければ `false`）。
    pub fn effective_enabled(&self) -> bool {
        self.listen.is_some() || !self.listeners.is_empty()
    }

    /// 糖衣とリストをまとめ、決定的に検証した口の一覧を返す。
    pub fn resolve_listeners(&self) -> Result<Vec<ResolvedListener>, McpConfigError> {
        let mut raw: Vec<(SocketAddr, String, Option<String>)> = Vec::new();
        if let Some(listen) = self.listen {
            raw.push((
                listen,
                self.auth.clone().unwrap_or_else(default_auth),
                self.client.clone(),
            ));
        }
        for l in &self.listeners {
            raw.push((l.listen, l.auth.clone(), l.client.clone()));
        }
        let mut out = Vec::with_capacity(raw.len());
        for (listen, auth, client) in raw {
            let auth = match auth.as_str() {
                "token" => {
                    if client.is_some() {
                        return Err(McpConfigError::ClientWithoutNone { listen });
                    }
                    ListenerAuth::Token
                }
                "none" => {
                    if !listen.ip().is_loopback() {
                        return Err(McpConfigError::NoneNotLoopback { listen });
                    }
                    let Some(client) = client.filter(|c| !c.trim().is_empty()) else {
                        return Err(McpConfigError::NoneWithoutClient { listen });
                    };
                    ListenerAuth::Fixed(client)
                }
                other => return Err(McpConfigError::UnknownAuth(other.to_string())),
            };
            out.push(ResolvedListener { listen, auth });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn default_config_has_no_listeners() {
        let cfg = McpConfig::default();
        assert!(!cfg.effective_enabled());
        assert_eq!(cfg.resolve_listeners().unwrap(), vec![]);
    }

    #[test]
    fn sugar_listen_defaults_to_token_auth() {
        let cfg = McpConfig {
            listen: Some(addr("127.0.0.1:18200")),
            ..Default::default()
        };
        assert!(cfg.effective_enabled());
        let listeners = cfg.resolve_listeners().unwrap();
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].auth, ListenerAuth::Token);
    }

    #[test]
    fn multiple_listeners_mix_token_and_fixed_none() {
        let cfg = McpConfig {
            listeners: vec![
                McpListenerConfig {
                    listen: addr("127.0.0.1:18200"),
                    auth: "token".to_string(),
                    client: None,
                },
                McpListenerConfig {
                    listen: addr("127.0.0.1:18201"),
                    auth: "none".to_string(),
                    client: Some("chatgpt".to_string()),
                },
            ],
            ..Default::default()
        };
        let listeners = cfg.resolve_listeners().unwrap();
        assert_eq!(listeners.len(), 2);
        assert_eq!(listeners[0].auth, ListenerAuth::Token);
        assert_eq!(
            listeners[1].auth,
            ListenerAuth::Fixed("chatgpt".to_string())
        );
    }

    #[test]
    fn none_on_non_loopback_is_a_config_error() {
        let cfg = McpConfig {
            listeners: vec![McpListenerConfig {
                listen: addr("0.0.0.0:18201"),
                auth: "none".to_string(),
                client: Some("chatgpt".to_string()),
            }],
            ..Default::default()
        };
        assert_eq!(
            cfg.resolve_listeners(),
            Err(McpConfigError::NoneNotLoopback {
                listen: addr("0.0.0.0:18201")
            })
        );
    }

    #[test]
    fn none_without_client_is_a_config_error() {
        let cfg = McpConfig {
            listeners: vec![McpListenerConfig {
                listen: addr("127.0.0.1:18201"),
                auth: "none".to_string(),
                client: None,
            }],
            ..Default::default()
        };
        assert_eq!(
            cfg.resolve_listeners(),
            Err(McpConfigError::NoneWithoutClient {
                listen: addr("127.0.0.1:18201")
            })
        );
    }

    #[test]
    fn token_with_client_is_a_config_error() {
        let cfg = McpConfig {
            listeners: vec![McpListenerConfig {
                listen: addr("127.0.0.1:18200"),
                auth: "token".to_string(),
                client: Some("chatgpt".to_string()),
            }],
            ..Default::default()
        };
        assert_eq!(
            cfg.resolve_listeners(),
            Err(McpConfigError::ClientWithoutNone {
                listen: addr("127.0.0.1:18200")
            })
        );
    }

    #[test]
    fn unknown_auth_is_a_config_error() {
        let cfg = McpConfig {
            listeners: vec![McpListenerConfig {
                listen: addr("127.0.0.1:18200"),
                auth: "bogus".to_string(),
                client: None,
            }],
            ..Default::default()
        };
        assert_eq!(
            cfg.resolve_listeners(),
            Err(McpConfigError::UnknownAuth("bogus".to_string()))
        );
    }
}
