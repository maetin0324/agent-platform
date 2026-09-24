//! Bearer 認証（`task-api::middleware` と同じ規律: SHA-256 の指紋を定数時間で比較する）。
//! `llm-proxy` は `task-api` に依存しない（依存が逆になるのを避ける。`GET /llm/sources` は
//! `task-api` 側からこのクレートを読みに来る）ので、ここに小さく持つ。

use axum::http::HeaderMap;
use sha2::{Digest, Sha256};

pub fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn check_bearer(headers: &HeaderMap, expected: &[u8; 32]) -> bool {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty());
    match presented {
        Some(token) => constant_time_eq(&token_digest(token), expected),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn accepts_the_right_bearer_token_only() {
        let expected = token_digest("s3cr3t");
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer s3cr3t"),
        );
        assert!(check_bearer(&headers, &expected));

        let mut wrong = HeaderMap::new();
        wrong.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer nope"),
        );
        assert!(!check_bearer(&wrong, &expected));

        assert!(!check_bearer(&HeaderMap::new(), &expected));
    }
}
