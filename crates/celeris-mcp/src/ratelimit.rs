//! ADR-0056 D4: クライアントごとの `tools/call` の流量制限（1 分あたり `rate_limit_per_min`）。
//!
//! メモリ上のスライディングウィンドウだけ（DB には残さない。再起動で消えてよい観測値）。

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct RateLimiter {
    hits: HashMap<String, VecDeque<Instant>>,
}

const WINDOW: Duration = Duration::from_secs(60);

impl RateLimiter {
    /// `client_id` が今この 1 回を使ってよいか。`limit_per_min` が 0 なら常に許す（制限なし）。
    /// 使ってよければ記録して `Ok(())`、超えていれば次に空くまでの秒数を `Err` で返す。
    pub fn check(&mut self, client_id: &str, limit_per_min: u32, now: Instant) -> Result<(), u64> {
        if limit_per_min == 0 {
            return Ok(());
        }
        let entry = self.hits.entry(client_id.to_string()).or_default();
        while let Some(&front) = entry.front() {
            if now.duration_since(front) >= WINDOW {
                entry.pop_front();
            } else {
                break;
            }
        }
        if entry.len() >= limit_per_min as usize {
            let oldest = *entry.front().expect("non-empty: len >= limit_per_min > 0");
            let retry_after = WINDOW.saturating_sub(now.duration_since(oldest));
            return Err(retry_after.as_secs().max(1));
        }
        entry.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_limit_then_blocks_with_retry_after() {
        let mut rl = RateLimiter::default();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(rl.check("a", 3, t0).is_ok());
        }
        let err = rl.check("a", 3, t0).unwrap_err();
        assert!(err >= 1);
    }

    #[test]
    fn different_clients_are_independent() {
        let mut rl = RateLimiter::default();
        let t0 = Instant::now();
        assert!(rl.check("a", 1, t0).is_ok());
        assert!(rl.check("b", 1, t0).is_ok());
        assert!(rl.check("a", 1, t0).is_err());
    }

    #[test]
    fn old_hits_expire_out_of_the_window() {
        let mut rl = RateLimiter::default();
        let t0 = Instant::now();
        assert!(rl.check("a", 1, t0).is_ok());
        assert!(rl.check("a", 1, t0 + Duration::from_secs(30)).is_err());
        assert!(rl.check("a", 1, t0 + Duration::from_secs(61)).is_ok());
    }

    #[test]
    fn zero_limit_means_unlimited() {
        let mut rl = RateLimiter::default();
        let t0 = Instant::now();
        for _ in 0..1000 {
            assert!(rl.check("a", 0, t0).is_ok());
        }
    }
}
