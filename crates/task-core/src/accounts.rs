//! Claude アカウントのレート制限の観測値（ADR-0024 D4）。
//!
//! Claude Code が stream-json の `rate_limit_event` で出す実測値をそのまま持つための型。推定はしない。
//! タスクの真実（DB・イベント）ではなく観測値で、replay の対象外。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// 1 つの枠（5 時間 / 7 日）の観測値。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RateWindow {
    /// 0.0〜1.0（`unifiedWindows.<w>.utilization`）
    pub utilization: f64,
    /// 枠がリセットされる時刻（Unix 秒、`resetsAt`）
    pub resets_at: i64,
}

/// `rate_limit_event` 1 行分の観測値。枠は欠けることがある。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RateLimitObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<RateWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<RateWindow>,
    /// `rate_limit_info.status`（`allowed` / `allowed_warning` / `rejected` 等。未知の値もそのまま）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// `rate_limit_info.resetsAt`（`status` が指す枠のリセット時刻、Unix 秒）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// 観測した時刻（Unix 秒。taskd の壁時計）
    pub observed_at: i64,
}

impl RateLimitObservation {
    /// stream-json の 1 行（`{"type":"rate_limit_event","rate_limit_info":{...}}`）を解析する。
    /// `rate_limit_event` でない、または枠も status も無ければ `None`。
    pub fn from_stream_json(line: &serde_json::Value, observed_at: i64) -> Option<Self> {
        if line.get("type").and_then(|v| v.as_str()) != Some("rate_limit_event") {
            return None;
        }
        let info = line.get("rate_limit_info")?;
        let window = |name: &str| -> Option<RateWindow> {
            let w = info.get("unifiedWindows")?.get(name)?;
            Some(RateWindow { utilization: w.get("utilization")?.as_f64()?, resets_at: w.get("resetsAt")?.as_i64()? })
        };
        let obs = Self {
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
            status: info.get("status").and_then(|v| v.as_str()).map(str::to_owned),
            resets_at: info.get("resetsAt").and_then(|v| v.as_i64()),
            observed_at,
        };
        if obs.five_hour.is_none() && obs.seven_day.is_none() && obs.status.is_none() {
            return None;
        }
        Some(obs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_line_observed_on_claude_2_1_273() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1789605600,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"out_of_credits","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.14,"resetsAt":1789605600},"seven_day":{"utilization":0.24,"resetsAt":1790031600}}},"uuid":"u","session_id":"s"}"#,
        )
        .expect("json");
        let obs = RateLimitObservation::from_stream_json(&line, 100).expect("observation");
        assert_eq!(obs.five_hour, Some(RateWindow { utilization: 0.14, resets_at: 1789605600 }));
        assert_eq!(obs.seven_day, Some(RateWindow { utilization: 0.24, resets_at: 1790031600 }));
        assert_eq!(obs.status.as_deref(), Some("allowed"));
        assert_eq!(obs.resets_at, Some(1789605600));
        assert_eq!(obs.observed_at, 100);
    }

    #[test]
    fn missing_windows_and_other_types_are_tolerated() {
        let partial: serde_json::Value =
            serde_json::from_str(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":5}}"#)
                .expect("json");
        let obs = RateLimitObservation::from_stream_json(&partial, 1).expect("status only");
        assert!(obs.five_hour.is_none() && obs.seven_day.is_none());
        assert_eq!(obs.status.as_deref(), Some("rejected"));

        let other: serde_json::Value = serde_json::from_str(r#"{"type":"assistant"}"#).expect("json");
        assert!(RateLimitObservation::from_stream_json(&other, 1).is_none());
        let empty: serde_json::Value =
            serde_json::from_str(r#"{"type":"rate_limit_event","rate_limit_info":{}}"#).expect("json");
        assert!(RateLimitObservation::from_stream_json(&empty, 1).is_none());
    }
}
