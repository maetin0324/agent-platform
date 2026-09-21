//! `reqwest::Error` の `Display` は URL を含みうるので、そのまま文字列にしない
//! （`celeris::notify::safe_error` と同じ規律。ADR-0053: 値も URL もログ・応答に出さない）。

pub fn safe_reqwest_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else if e.is_body() || e.is_decode() {
        "bad response body".to_string()
    } else if e.is_request() {
        "invalid request".to_string()
    } else {
        "request failed".to_string()
    }
}
