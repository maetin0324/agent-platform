//! 全要求に掛ける共通の検査と応答ヘッダ（`docs/gui/api.md` §1.2〜§1.5）。
//!
//! 順序: Host 検査 → `OPTIONS` は 405 → Bearer 認証（`/health` を除く）→ `POST` の Origin / Content-Type / 本文サイズ。
//! 応答には `Cache-Control: no-store`、`X-Content-Type-Options: nosniff`、`X-Request-Id` を付け、ハンドラが返した
//! `ApiProblem` をここで `application/problem+json` に描画する（`instance` = `X-Request-Id`）。CORS ヘッダは出さない。

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::MAX_BODY_BYTES;
use crate::problem::{ApiProblem, PendingProblem};
use crate::state::ApiState;

const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");
const HEALTH_PATH: &str = "/api/v1/health";
const STREAM_PATH: &str = "/api/v1/stream";
/// これ以上かかった要求は `warn` で記録する（ADR-0015 D1）。SSE は対象外。
const SLOW_REQUEST: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) async fn guard(State(state): State<ApiState>, req: Request, next: Next) -> Response {
    let request_id = Ulid::new().to_string();
    // ADR-0015 D1: 所要時間を測る。パス以外（クエリ・本体）は記録しない。
    let started = std::time::Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let mut response = match check_request(&state, &req) {
        Ok(()) => next.run(req).await,
        Err(problem) => problem.into_response(),
    };
    if let Some(PendingProblem(problem)) = response.extensions_mut().remove::<PendingProblem>() {
        response = problem.render(&request_id);
    }
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        headers.insert(X_REQUEST_ID, value);
    }
    let elapsed = started.elapsed();
    let duration_ms = elapsed.as_millis() as u64;
    let status = response.status().as_u16();
    if elapsed >= SLOW_REQUEST && path != STREAM_PATH {
        tracing::warn!(%request_id, %method, %path, status, duration_ms, "slow api request");
    } else {
        tracing::debug!(%request_id, %method, %path, status, duration_ms, "api request");
    }
    response
}

/// `Host` ヘッダと（absolute-form の）URI の authority の両方を許可リストと照合する。どちらも無い、
/// `Host` が複数ある（RFC 9112 §3.2）、ASCII でない場合は許可しない。
fn host_allowed(state: &ApiState, req: &Request) -> bool {
    let mut host_headers = req.headers().get_all(header::HOST).iter();
    let header_host = host_headers.next();
    if host_headers.next().is_some() {
        return false;
    }
    let header_host = match header_host.map(|v| v.to_str()) {
        Some(Ok(value)) => Some(value.to_string()),
        Some(Err(_)) => return false,
        None => None,
    };
    let authority_host = req.uri().authority().map(|a| a.host().to_string());
    let candidates: Vec<String> = header_host.into_iter().chain(authority_host).collect();
    !candidates.is_empty()
        && candidates.iter().all(|candidate| {
            host_without_port(candidate).is_some_and(|h| state.inner.allowed_hosts.contains(&h))
        })
}

fn check_request(state: &ApiState, req: &Request) -> Result<(), ApiProblem> {
    if !host_allowed(state, req) {
        return Err(ApiProblem::host_not_allowed());
    }

    if req.method() == Method::OPTIONS {
        return Err(ApiProblem::method_not_allowed());
    }

    if let Some(expected) = &state.inner.token_digest
        && req.uri().path() != HEALTH_PATH
    {
        check_bearer(req.headers(), expected)?;
    }

    // ADR-0017 で PATCH/DELETE（プロバイダ管理）が加わるまでは変更系 = POST だけだった。ADR-0030 で
    // `PUT /secrets/{id}` が加わり PUT も同じ扱いにする。`Origin` の拒否は本文の有無に関わらず全ての変更系
    // メソッドに掛ける（監査で発見: PATCH/DELETE が POST 専用のこのチェックを素通りしていた）。
    let is_mutating = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    if is_mutating && req.headers().contains_key(header::ORIGIN) {
        return Err(ApiProblem::origin_forbidden());
    }
    // Content-Type / 本文サイズは本文を伴うメソッド（POST・PUT・PATCH）だけ検査する（DELETE は本文を取らない）。
    if matches!(*req.method(), Method::POST | Method::PUT | Method::PATCH) {
        if !is_json_content_type(req.headers()) {
            return Err(ApiProblem::unsupported_media_type());
        }
        let declared = req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if declared.is_some_and(|len| len > MAX_BODY_BYTES as u64) {
            return Err(ApiProblem::payload_too_large());
        }
    }
    Ok(())
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

pub(crate) fn check_bearer(headers: &HeaderMap, expected: &[u8; 32]) -> Result<(), ApiProblem> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty());
    match presented {
        Some(token) if constant_time_eq(&token_digest(token), expected) => Ok(()),
        _ => Err(ApiProblem::unauthorized()),
    }
}

/// ADR-0017 D1: 管理系エンドポイントは `token_file` 未設定（loopback 限定構成）でも認証をスキップしない
/// （通常のガードは `token_digest` が無ければ全て通す。管理系はここで別に検査する）。
pub(crate) fn require_admin(state: &ApiState, headers: &HeaderMap) -> Result<(), ApiProblem> {
    match &state.inner.token_digest {
        Some(expected) => check_bearer(headers, expected),
        None => Err(ApiProblem::unauthorized()),
    }
}

/// ADR-0040 D4（Phase 47）: ディスパッチャの状態を要する管理系（`reload` / `check` / クラスタ接続 /
/// アカウントの確認・削除・ログイン中継 / `notify/test`）は `active` のときだけ受ける。`standby` と
/// `draining` の間は 503 `standby` に `Retry-After: 2` を付けて返す（読み書きの通常のエンドポイントは
/// 同じ DB を見ているのでそのまま動く）。
pub(crate) fn require_active(state: &ApiState) -> Result<(), ApiProblem> {
    if state.inner.role.accepts_admin() {
        return Ok(());
    }
    Err(ApiProblem::standby())
}

/// トークンは SHA-256 の値で持ち、比較は長さに依らない定数時間で行う。
pub(crate) fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub(crate) fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `Host` ヘッダの値からポートを除き、小文字にする（IPv6 は `[::1]` の形）。不正な形は `None`。
pub(crate) fn host_without_port(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let host = if let Some(rest) = value.strip_prefix('[') {
        let end = rest.find(']')?;
        let after = &rest[end + 1..];
        let port_ok = after.is_empty()
            || after
                .strip_prefix(':')
                .is_some_and(|port| port.chars().all(|c| c.is_ascii_digit()));
        if !port_ok {
            return None;
        }
        format!("[{}]", &rest[..end])
    } else {
        match value.rsplit_once(':') {
            Some((host, port))
                if port.chars().all(|c| c.is_ascii_digit()) && !host.contains(':') =>
            {
                host.to_string()
            }
            Some(_) => return None,
            None => value.to_string(),
        }
    };
    if host.is_empty() || host == "[]" {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Host の許可リスト: `localhost`、`127.0.0.1`、`[::1]`、`listen` のホスト、`allowed_hosts`（ポートは無視）。
pub(crate) fn allowed_host_list(listen: SocketAddr, extra: &[String]) -> Vec<String> {
    let mut hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "[::1]".into()];
    hosts.push(match listen.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    });
    for entry in extra {
        let entry = entry.trim();
        let normalized = match entry.parse::<Ipv6Addr>() {
            Ok(ip) => Some(format!("[{ip}]")),
            Err(_) => host_without_port(entry),
        };
        if let Some(host) = normalized
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_without_port_strips_ports_and_normalizes_case() {
        assert_eq!(
            host_without_port("localhost:7710").as_deref(),
            Some("localhost")
        );
        assert_eq!(host_without_port("LOCALHOST").as_deref(), Some("localhost"));
        assert_eq!(host_without_port("127.0.0.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(host_without_port("[::1]:7710").as_deref(), Some("[::1]"));
        assert_eq!(host_without_port("[::1]").as_deref(), Some("[::1]"));
        assert_eq!(host_without_port("::1"), None);
        assert_eq!(host_without_port("evil.example:abc"), None);
        assert_eq!(host_without_port("[::1]x"), None);
        assert_eq!(host_without_port(""), None);
    }

    #[test]
    fn allowed_hosts_include_defaults_listen_host_and_extras() {
        let listen: SocketAddr = "10.0.0.5:7710".parse().unwrap_or_else(|e| panic!("{e}"));
        let hosts = allowed_host_list(
            listen,
            &[
                "Celeris.Lab.Example".to_string(),
                "::1".to_string(),
                "fe80::1".to_string(),
            ],
        );
        assert!(hosts.contains(&"localhost".to_string()));
        assert!(hosts.contains(&"127.0.0.1".to_string()));
        assert!(hosts.contains(&"[::1]".to_string()));
        assert!(hosts.contains(&"10.0.0.5".to_string()));
        assert!(hosts.contains(&"celeris.lab.example".to_string()));
        assert!(hosts.contains(&"[fe80::1]".to_string()));
        assert_eq!(hosts.iter().filter(|h| *h == "[::1]").count(), 1);
    }

    #[test]
    fn constant_time_eq_compares_digests() {
        assert!(constant_time_eq(
            &token_digest("secret"),
            &token_digest("secret")
        ));
        assert!(!constant_time_eq(
            &token_digest("secret"),
            &token_digest("secret2")
        ));
    }
}
