//! OpenAI 互換エンドポイントの**到達性の検査**（ADR-0052 D1。Phase 64）。
//!
//! 知識整理 run（`knowledge` ハーネス = `langmem` アダプタ）の LLM は pegasus のトンネル越しの Qwen で、
//! トンネルが落ちている間は run が必ず落ちる。dispatch の**直前**に `GET <base_url>/models` を当てて、
//! 届かなければ tier `cheap` の汎用ハーネスへ倒す（ADR-0052 D2）。
//!
//! **LLM は呼ばない**（CLAUDE.md「ディスパッチャやストアに LLM 呼び出しを入れない」）。ここがやるのは
//! ADR-0043 D3 のコンテナ runtime の probe と同じ種類の、決定的な 1 回の HTTP GET だけ。
//!
//! 依存を増やさないため、`std::net::TcpStream` に最小限の HTTP/1.1 を自分で書く
//! （リクエストは 1 行 + ヘッダ、応答はステータス行だけ読む）。`http://` だけを見る:
//! `https://` や書き方の壊れた `base_url` は [`Reachability::Unknown`] にして、**従来どおり**
//! `langmem` で走らせる（検査できないことを「落ちている」と決めつけない）。
//!
//! **Phase 65b 追記**: `[llm_proxy]`（ADR-0053）を `[knowledge.langmem].base_url` に向けたとき、
//! `GET /v1/models` は Bearer トークンが無いと 401 を返す（`/healthz` を除く全エンドポイントが
//! 認証を要求する。`docs/llm-source.md` §6）。401/403 は「LLM が落ちている」ことを意味しない
//! （トークンが未設定・不一致というだけ）ので、[`Reachability::Unreachable`] にせず
//! [`Reachability::Unknown`]（= 従来どおり `langmem` で走らせる）にする。呼び出し側が
//! `[knowledge.langmem].api_key_secret` から解決した平文のトークンを渡せば、`Authorization: Bearer`
//! ヘッダを付けて検査する。**トークンの値はどのログにも出さない**。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// ADR-0052 D1: 検査の制限時間（3 秒）。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// ADR-0052 D1: 検査の結果をキャッシュする時間（60 秒。tick ごとに叩かない）。
pub const PROBE_CACHE_TTL: Duration = Duration::from_secs(60);

/// ステータス行を読むときの上限（これを超えたら壊れた応答とみなす）。
const MAX_STATUS_LINE: usize = 512;

/// [`probe_models`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    /// `GET <base_url>/models` が 2xx を返した。
    Ok,
    /// 接続できない・時間切れ・2xx 以外。`reason` は人が読む 1 行（進行イベントに出す）。
    Unreachable { reason: String },
    /// 検査できない（`base_url` が無い・`https://`・書き方が壊れている）。**従来どおり**扱う。
    Unknown { reason: String },
}

impl Reachability {
    /// フォールバックすべきか（`Unreachable` のときだけ）。
    pub fn should_fall_back(&self) -> Option<&str> {
        match self {
            Reachability::Unreachable { reason } => Some(reason),
            Reachability::Ok | Reachability::Unknown { .. } => None,
        }
    }
}

/// `base_url` を `(host, port, path)` に分解する（`http://host[:port][/path]`）。
fn split_http_url(base_url: &str) -> Result<(String, u16, String), String> {
    let trimmed = base_url.trim();
    let rest = match trimmed.strip_prefix("http://") {
        Some(rest) => rest,
        None if trimmed.starts_with("https://") => {
            return Err("https は検査しない".to_string());
        }
        None => return Err("http:// で始まっていない".to_string()),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/').to_string()),
        None => (rest, String::new()),
    };
    if authority.is_empty() {
        return Err("ホストが空".to_string());
    }
    // IPv6 のリテラル（`[::1]:8000`）も読めるようにする。
    let (host, port) = if let Some(end) = authority.strip_prefix('[').and_then(|a| a.find(']')) {
        let host = &authority[1..=end];
        match authority[end + 2..].strip_prefix(':') {
            Some(p) => (host, p),
            None => (host, ""),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, p),
            None => (authority, ""),
        }
    };
    let port: u16 = if port.is_empty() {
        80
    } else {
        port.parse().map_err(|_| format!("ポートが数でない: {port}"))?
    };
    if host.is_empty() {
        return Err("ホストが空".to_string());
    }
    Ok((host.to_string(), port, path))
}

/// ADR-0052 D1（Phase 65b で `bearer_token` を追加）: `GET <base_url>/models` を `timeout` で
/// 1 回だけ当てる。`bearer_token` があれば `Authorization: Bearer <token>` を付ける
/// （celeris の `llm-proxy` のように、`/healthz` 以外の全エンドポイントが認証を要求する上流のため）。
///
/// ネットワーク I/O はここだけ。返るのは決定的な 3 値（[`Reachability`]）で、判断は呼び出し側
/// （`task_dispatch::Dispatcher`）がする。**`bearer_token` の値はログに出さない。**
pub fn probe_models(base_url: &str, timeout: Duration, bearer_token: Option<&str>) -> Reachability {
    let (host, port, path) = match split_http_url(base_url) {
        Ok(parts) => parts,
        Err(reason) => return Reachability::Unknown { reason },
    };
    let target = format!("{path}/models");
    let deadline = Instant::now() + timeout;

    let remaining = |deadline: Instant| deadline.saturating_duration_since(Instant::now());
    let addrs = match (host.as_str(), port).to_socket_addrs() {
        Ok(addrs) => addrs.collect::<Vec<_>>(),
        Err(e) => {
            return Reachability::Unreachable {
                reason: format!("名前を引けない: {e}"),
            };
        }
    };
    let Some(addr) = addrs.into_iter().next() else {
        return Reachability::Unreachable {
            reason: "名前に対応する住所が無い".to_string(),
        };
    };

    let left = remaining(deadline);
    if left.is_zero() {
        return Reachability::Unreachable {
            reason: "時間切れ（接続する前）".to_string(),
        };
    }
    let mut stream = match TcpStream::connect_timeout(&addr, left) {
        Ok(s) => s,
        Err(e) => {
            return Reachability::Unreachable {
                reason: format!("接続できない: {e}"),
            };
        }
    };
    let left = remaining(deadline);
    if left.is_zero() {
        return Reachability::Unreachable {
            reason: "時間切れ（接続の直後）".to_string(),
        };
    }
    if stream.set_read_timeout(Some(left)).is_err() || stream.set_write_timeout(Some(left)).is_err()
    {
        return Reachability::Unreachable {
            reason: "ソケットの時間切れを設定できない".to_string(),
        };
    }
    let request = match bearer_token {
        Some(token) => format!(
            "GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: */*\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        ),
        None => format!("GET {target} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: */*\r\nConnection: close\r\n\r\n"),
    };
    if let Err(e) = stream.write_all(request.as_bytes()) {
        return Reachability::Unreachable {
            reason: format!("要求を送れない: {e}"),
        };
    }
    let _ = stream.flush();

    // ステータス行（`HTTP/1.1 200 OK`）だけ読む。本文は読まない。
    let mut line = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        if remaining(deadline).is_zero() {
            return Reachability::Unreachable {
                reason: "応答が時間内に来ない".to_string(),
            };
        }
        match stream.read(&mut byte) {
            Ok(0) => {
                return Reachability::Unreachable {
                    reason: "応答が無いまま閉じられた".to_string(),
                };
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                if byte[0] != b'\r' {
                    line.push(byte[0]);
                }
                if line.len() > MAX_STATUS_LINE {
                    return Reachability::Unreachable {
                        reason: "応答のステータス行が長すぎる".to_string(),
                    };
                }
            }
            Err(e) => {
                return Reachability::Unreachable {
                    reason: format!("応答を読めない: {e}"),
                };
            }
        }
    }
    let status_line = String::from_utf8_lossy(&line).trim().to_string();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(code) if (200..300).contains(&code) => Reachability::Ok,
        // Phase 65b: 401/403 は「到達性が無い」ではなく「認証が合っていない」。LLM が落ちている
        // わけではないので、フォールバックさせない（従来どおり langmem で走らせる）。
        Some(code @ (401 | 403)) => Reachability::Unknown {
            reason: format!("HTTP {code}（認証エラーは到達性の欠落として扱わない）"),
        },
        Some(code) => Reachability::Unreachable {
            reason: format!("HTTP {code}"),
        },
        None => Reachability::Unreachable {
            reason: format!("応答が HTTP ではない: {status_line}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// 127.0.0.1 の空きポートに束ねる（**外部ネットワークには出ない**）。
    fn listener() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").expect("bind")
    }

    #[test]
    fn a_200_from_a_local_fake_server_is_reachable() {
        let server = listener();
        let addr = server.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel::<String>();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"data\": []}\r\n\r\n",
            );
        });
        let base = format!("http://127.0.0.1:{}/v1", addr.port());
        assert_eq!(probe_models(&base, PROBE_TIMEOUT, None), Reachability::Ok);
        let request = rx.recv().expect("request");
        assert!(request.starts_with("GET /v1/models HTTP/1.1"), "{request}");
        handle.join().expect("join");
    }

    #[test]
    fn a_non_2xx_answer_is_unreachable() {
        let server = listener();
        let addr = server.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        });
        let base = format!("http://127.0.0.1:{}/v1", addr.port());
        let outcome = probe_models(&base, PROBE_TIMEOUT, None);
        assert_eq!(outcome.should_fall_back(), Some("HTTP 502"), "{outcome:?}");
        handle.join().expect("join");
    }

    /// ADR-0052 D1: 接続不可（トンネルが落ちている＝誰も listen していないポート）。
    #[test]
    fn a_refused_connection_is_unreachable() {
        let server = listener();
        let port = server.local_addr().expect("addr").port();
        drop(server); // ここで誰も listen していないポートになる。
        let base = format!("http://127.0.0.1:{port}/v1");
        let outcome = probe_models(&base, PROBE_TIMEOUT, None);
        let reason = outcome.should_fall_back().expect("unreachable");
        assert!(reason.contains("接続できない"), "{reason}");
    }

    /// ADR-0052 D1: タイムアウト（受けるだけで何も返さないサーバ）。
    #[test]
    fn a_server_that_never_answers_times_out() {
        let server = listener();
        let addr = server.local_addr().expect("addr");
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let (stream, _) = server.accept().expect("accept");
            // 何も書かずに、検査が終わるまで開けたままにする。
            let _ = done_rx.recv();
            drop(stream);
        });
        let base = format!("http://127.0.0.1:{}/v1", addr.port());
        let started = Instant::now();
        let outcome = probe_models(&base, Duration::from_millis(300), None);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "制限時間で打ち切る: {:?}",
            started.elapsed()
        );
        let reason = outcome.should_fall_back().expect("unreachable");
        assert!(reason.contains("応答"), "{reason}");
        let _ = done_tx.send(());
        handle.join().expect("join");
    }

    /// 検査できない書き方は `Unknown`（= 従来どおり `langmem` で走らせる）。
    #[test]
    fn unprobeable_base_urls_are_unknown() {
        for base in [
            "https://api.example.com/v1",
            "ftp://nope",
            "",
            "http://",
            "http://host:notaport/v1",
        ] {
            assert!(
                matches!(probe_models(base, PROBE_TIMEOUT, None), Reachability::Unknown { .. }),
                "{base}"
            );
        }
    }

    /// Phase 65b: `bearer_token` を渡すと `Authorization: Bearer <token>` が送られ、それが無いと
    /// 401 を返す上流（celeris の `llm-proxy` の `/v1/models` と同じ挙動）でも到達できる。
    #[test]
    fn a_bearer_token_is_sent_as_an_authorization_header() {
        let server = listener();
        let addr = server.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel::<String>();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = tx.send(req.clone());
            if req.contains("Authorization: Bearer secret-proxy-token") {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            } else {
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let base = format!("http://127.0.0.1:{}/v1", addr.port());
        assert_eq!(probe_models(&base, PROBE_TIMEOUT, Some("secret-proxy-token")), Reachability::Ok);
        let request = rx.recv().expect("request");
        assert!(request.contains("Authorization: Bearer secret-proxy-token"), "{request}");
        handle.join().expect("join");
    }

    #[test]
    fn without_a_bearer_token_the_same_upstream_answers_401() {
        let server = listener();
        let addr = server.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(!req.contains("Authorization"), "{req}");
            let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        });
        let base = format!("http://127.0.0.1:{}/v1", addr.port());
        let outcome = probe_models(&base, PROBE_TIMEOUT, None);
        assert!(matches!(outcome, Reachability::Unknown { .. }), "{outcome:?}");
        handle.join().expect("join");
    }

    /// Phase 65b: 401/403 は「到達性が無い」ではない（＝ 認証エラーで `Unreachable` にしない。
    /// フォールバックさせず、従来どおり `langmem` で走らせる）。
    #[test]
    fn a_401_or_403_from_the_probe_is_unknown_not_unreachable() {
        for status in ["401 Unauthorized", "403 Forbidden"] {
            let server = listener();
            let addr = server.local_addr().expect("addr");
            let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
            let handle = std::thread::spawn(move || {
                let (mut stream, _) = server.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            });
            let base = format!("http://127.0.0.1:{}/v1", addr.port());
            let outcome = probe_models(&base, PROBE_TIMEOUT, None);
            assert!(matches!(outcome, Reachability::Unknown { .. }), "{status}: {outcome:?}");
            assert!(outcome.should_fall_back().is_none(), "{status}: フォールバックしない");
            handle.join().expect("join");
        }
    }

    #[test]
    fn urls_split_into_host_port_and_path() {
        assert_eq!(
            split_http_url("http://127.0.0.1:18000/v1").expect("split"),
            ("127.0.0.1".to_string(), 18000, "/v1".to_string())
        );
        assert_eq!(
            split_http_url("http://bnode150/v1/").expect("split"),
            ("bnode150".to_string(), 80, "/v1".to_string())
        );
        assert_eq!(
            split_http_url("http://[::1]:8000").expect("split"),
            ("::1".to_string(), 8000, String::new())
        );
    }
}
