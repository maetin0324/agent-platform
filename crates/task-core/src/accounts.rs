//! アカウントのレート制限の観測値（ADR-0024 D4, ADR-0025 D1/D3）。
//!
//! Claude Code / codex が stream-json の実測値をそのまま持つための型。推定はしない。
//! タスクの真実（DB・イベント）ではなく観測値で、replay の対象外。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// アカウントプールのアダプタの種類（ADR-0025 D1）。`(adapter, id)` でアカウントを識別する。
///
/// task-core に置くのは、task-dispatch / task-worker / task-api / celeris のいずれからも参照できる
/// 基底クレートだからで、依存を増やさない（ADR-0025 の指示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AccountAdapter {
    ClaudeCode,
    Codex,
}

impl AccountAdapter {
    pub const ALL: [AccountAdapter; 2] = [AccountAdapter::ClaudeCode, AccountAdapter::Codex];

    /// `"claude-code"` / `"codex"`（設定の `adapter` や API の `?adapter=` と同じ文字列）。
    pub fn as_str(self) -> &'static str {
        match self {
            AccountAdapter::ClaudeCode => "claude-code",
            AccountAdapter::Codex => "codex",
        }
    }

    /// 設定・クエリの文字列から解決する。既知でなければ `None`。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude-code" => Some(AccountAdapter::ClaudeCode),
            "codex" => Some(AccountAdapter::Codex),
            _ => None,
        }
    }

    /// ログイン済みかどうかを示すファイル名（ADR-0025 D1: 中身は読まない、存在だけを見る）。
    pub fn credentials_marker(self) -> &'static str {
        match self {
            AccountAdapter::ClaudeCode => ".credentials.json",
            AccountAdapter::Codex => "auth.json",
        }
    }

    /// プールで選んだアカウントの根ディレクトリを渡す環境変数名（ADR-0025 D2）。
    pub fn env_var(self) -> &'static str {
        match self {
            AccountAdapter::ClaudeCode => "CLAUDE_SECURESTORAGE_CONFIG_DIR",
            AccountAdapter::Codex => "CODEX_HOME",
        }
    }
}

impl std::fmt::Display for AccountAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// 観測した時刻（Unix 秒。celeris の壁時計）
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
            Some(RateWindow {
                utilization: w.get("utilization")?.as_f64()?,
                resets_at: w.get("resetsAt")?.as_i64()?,
            })
        };
        let obs = Self {
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
            status: info
                .get("status")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            resets_at: info.get("resetsAt").and_then(|v| v.as_i64()),
            observed_at,
        };
        if obs.five_hour.is_none() && obs.seven_day.is_none() && obs.status.is_none() {
            return None;
        }
        Some(obs)
    }

    /// Codex rollout の観測値。リセット時刻は絶対秒と相対秒を区別する。
    pub fn from_codex_token_count(line: &serde_json::Value, observed_at: i64) -> Option<Self> {
        if line.get("type").and_then(|v| v.as_str()) != Some("token_count") {
            return None;
        }
        Self::from_codex_limits(line.get("rate_limits")?, observed_at)
    }

    /// account/rateLimits/read の result。複数 bucket があれば Codex の枠だけを読む。
    pub fn from_codex_account_limits(result: &serde_json::Value, observed_at: i64) -> Option<Self> {
        let limits = result
            .get("rateLimitsByLimitId")
            .and_then(|buckets| buckets.get("codex"))
            .or_else(|| result.get("rateLimits"))?;
        if limits
            .get("limitId")
            .and_then(|v| v.as_str())
            .is_some_and(|id| id != "codex")
        {
            return None;
        }
        Self::from_codex_limits(limits, observed_at)
    }

    fn from_codex_limits(limits: &serde_json::Value, observed_at: i64) -> Option<Self> {
        let mut obs = Self {
            five_hour: None,
            seven_day: None,
            status: None,
            resets_at: None,
            observed_at,
        };
        for key in ["primary", "secondary"] {
            if let Some((window, long)) = limits.get(key).and_then(|w| codex_window(w, observed_at))
            {
                if long {
                    obs.seven_day = Some(window);
                } else {
                    obs.five_hour = Some(window);
                }
            }
        }
        (obs.five_hour.is_some() || obs.seven_day.is_some()).then_some(obs)
    }
}

fn codex_window(value: &serde_json::Value, observed_at: i64) -> Option<(RateWindow, bool)> {
    let used_percent = value
        .get("usedPercent")
        .or_else(|| value.get("used_percent"))?
        .as_f64()?;
    let minutes = value
        .get("windowDurationMins")
        .or_else(|| value.get("window_minutes"))?
        .as_i64()?;
    if !used_percent.is_finite() || !(0.0..=100.0).contains(&used_percent) || minutes <= 0 {
        return None;
    }
    let resets_at = if let Some(absolute) = value.get("resetsAt").or_else(|| value.get("resets_at"))
    {
        absolute.as_i64()?
    } else {
        let seconds = value
            .get("resets_in_seconds")
            .or_else(|| value.get("reset_after_seconds"))?
            .as_i64()?;
        if seconds < 0 {
            return None;
        }
        observed_at.checked_add(seconds)?
    };
    if resets_at < 0 {
        return None;
    }
    Some((
        RateWindow {
            utilization: used_percent / 100.0,
            resets_at,
        },
        minutes > 1440,
    ))
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
        assert_eq!(
            obs.five_hour,
            Some(RateWindow {
                utilization: 0.14,
                resets_at: 1789605600
            })
        );
        assert_eq!(
            obs.seven_day,
            Some(RateWindow {
                utilization: 0.24,
                resets_at: 1790031600
            })
        );
        assert_eq!(obs.status.as_deref(), Some("allowed"));
        assert_eq!(obs.resets_at, Some(1789605600));
        assert_eq!(obs.observed_at, 100);
    }

    #[test]
    fn missing_windows_and_other_types_are_tolerated() {
        let partial: serde_json::Value = serde_json::from_str(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":5}}"#,
        )
        .expect("json");
        let obs = RateLimitObservation::from_stream_json(&partial, 1).expect("status only");
        assert!(obs.five_hour.is_none() && obs.seven_day.is_none());
        assert_eq!(obs.status.as_deref(), Some("rejected"));

        let other: serde_json::Value =
            serde_json::from_str(r#"{"type":"assistant"}"#).expect("json");
        assert!(RateLimitObservation::from_stream_json(&other, 1).is_none());
        let empty: serde_json::Value =
            serde_json::from_str(r#"{"type":"rate_limit_event","rate_limit_info":{}}"#)
                .expect("json");
        assert!(RateLimitObservation::from_stream_json(&empty, 1).is_none());
    }

    // ---- AccountAdapter (ADR-0025 D1) ----

    #[test]
    fn account_adapter_string_round_trip_and_markers() {
        assert_eq!(AccountAdapter::ClaudeCode.as_str(), "claude-code");
        assert_eq!(AccountAdapter::Codex.as_str(), "codex");
        assert_eq!(
            AccountAdapter::parse("claude-code"),
            Some(AccountAdapter::ClaudeCode)
        );
        assert_eq!(AccountAdapter::parse("codex"), Some(AccountAdapter::Codex));
        assert_eq!(AccountAdapter::parse("fake"), None);
        assert_eq!(
            AccountAdapter::ClaudeCode.credentials_marker(),
            ".credentials.json"
        );
        assert_eq!(AccountAdapter::Codex.credentials_marker(), "auth.json");
        assert_eq!(
            AccountAdapter::ClaudeCode.env_var(),
            "CLAUDE_SECURESTORAGE_CONFIG_DIR"
        );
        assert_eq!(AccountAdapter::Codex.env_var(), "CODEX_HOME");
        assert_eq!(
            serde_json::to_string(&AccountAdapter::Codex).expect("json"),
            "\"codex\""
        );
        assert_eq!(
            serde_json::to_string(&AccountAdapter::ClaudeCode).expect("json"),
            "\"claude-code\""
        );
    }

    // ---- RateLimitObservation::from_codex_token_count (ADR-0025 D3) ----

    /// `resets_in_seconds` を使う形（実測でこの名前が使われている場合）。
    #[test]
    fn codex_token_count_with_resets_in_seconds_field() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"token_count","rate_limits":{"primary":{"used_percent":14.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":24.0,"window_minutes":10080,"resets_in_seconds":432000}}}"#,
        )
        .expect("json");
        let obs = RateLimitObservation::from_codex_token_count(&line, 1_000).expect("observation");
        assert_eq!(
            obs.five_hour,
            Some(RateWindow {
                utilization: 0.14,
                resets_at: 1_000 + 3_600
            })
        );
        assert_eq!(
            obs.seven_day,
            Some(RateWindow {
                utilization: 0.24,
                resets_at: 1_000 + 432_000
            })
        );
        assert_eq!(obs.observed_at, 1_000);
    }

    /// `reset_after_seconds` を使う形。
    #[test]
    fn codex_token_count_with_reset_after_seconds_field() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"token_count","rate_limits":{"primary":{"used_percent":50.0,"window_minutes":300,"reset_after_seconds":7200}}}"#,
        )
        .expect("json");
        let obs = RateLimitObservation::from_codex_token_count(&line, 500).expect("observation");
        assert_eq!(
            obs.five_hour,
            Some(RateWindow {
                utilization: 0.5,
                resets_at: 500 + 7_200
            })
        );
        assert!(obs.seven_day.is_none());
    }

    /// `resets_at` は Unix 絶対秒。
    #[test]
    fn codex_token_count_with_absolute_resets_at() {
        let line: serde_json::Value = serde_json::from_str(
            r#"{"type":"token_count","rate_limits":{"secondary":{"used_percent":10.0,"window_minutes":10080,"resets_at":86400}}}"#,
        )
        .expect("json");
        let obs = RateLimitObservation::from_codex_token_count(&line, 2_000).expect("observation");
        assert_eq!(
            obs.seven_day,
            Some(RateWindow {
                utilization: 0.1,
                resets_at: 86_400
            })
        );
        assert!(obs.five_hour.is_none());
    }

    #[test]
    fn codex_missing_or_invalid_windows_are_unknown() {
        for primary in [
            serde_json::json!({"usedPercent": 20, "windowDurationMins": 300}),
            serde_json::json!({"usedPercent": -1, "windowDurationMins": 300, "resetsAt": 1000}),
            serde_json::json!({"usedPercent": 101, "windowDurationMins": 300, "resetsAt": 1000}),
            serde_json::json!({"usedPercent": 20, "windowDurationMins": 0, "resetsAt": 1000}),
        ] {
            assert!(
                RateLimitObservation::from_codex_account_limits(
                    &serde_json::json!({"rateLimits": {"primary": primary}}),
                    100
                )
                .is_none()
            );
        }
    }

    #[test]
    fn codex_app_server_prefers_codex_bucket_and_absolute_time() {
        let result = serde_json::json!({
            "rateLimits": {"primary": {"usedPercent": 99, "windowDurationMins": 300, "resetsAt": 2000}},
            "rateLimitsByLimitId": {"codex": {
                "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 2000},
                "secondary": {"usedPercent": 40, "windowDurationMins": 10080, "resetsAt": 9000}
            }}
        });
        let obs = RateLimitObservation::from_codex_account_limits(&result, 1000).unwrap();
        assert_eq!(
            obs.five_hour,
            Some(RateWindow {
                utilization: 0.25,
                resets_at: 2000
            })
        );
        assert_eq!(
            obs.seven_day,
            Some(RateWindow {
                utilization: 0.4,
                resets_at: 9000
            })
        );
        assert!(RateLimitObservation::from_codex_account_limits(&serde_json::json!({"rateLimits": {"limitId": "other", "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 2000}}}), 1000).is_none());
    }

    /// `window_minutes` による枠の割り当て: 1440 以下は five_hour（短い枠）、それより長ければ seven_day（長い枠）。
    /// `primary`/`secondary` という名前ではなく窓の長さで判断する（境界値もテストする）。
    #[test]
    fn codex_token_count_window_classification_is_by_length_not_by_key_name() {
        let boundary: serde_json::Value = serde_json::from_str(
            r#"{"type":"token_count","rate_limits":{"primary":{"used_percent":1.0,"window_minutes":1440,"resets_in_seconds":1}}}"#,
        )
        .expect("json");
        let obs = RateLimitObservation::from_codex_token_count(&boundary, 0).expect("observation");
        assert!(
            obs.five_hour.is_some(),
            "1440 minutes is still the short window"
        );
        assert!(obs.seven_day.is_none());

        // 名前が "secondary" でも window_minutes が短ければ five_hour 枠に入る。
        let swapped: serde_json::Value = serde_json::from_str(
            r#"{"type":"token_count","rate_limits":{"secondary":{"used_percent":5.0,"window_minutes":60,"resets_in_seconds":10}}}"#,
        )
        .expect("json");
        let obs2 = RateLimitObservation::from_codex_token_count(&swapped, 0).expect("observation");
        assert!(obs2.five_hour.is_some());
        assert!(obs2.seven_day.is_none());

        let long: serde_json::Value = serde_json::from_str(
            r#"{"type":"token_count","rate_limits":{"primary":{"used_percent":1.0,"window_minutes":1441,"resets_in_seconds":1}}}"#,
        )
        .expect("json");
        let obs3 = RateLimitObservation::from_codex_token_count(&long, 0).expect("observation");
        assert!(obs3.five_hour.is_none());
        assert!(obs3.seven_day.is_some());
    }

    #[test]
    fn codex_token_count_wrong_type_or_missing_rate_limits_is_none() {
        let wrong_type: serde_json::Value =
            serde_json::from_str(r#"{"type":"item.started"}"#).expect("json");
        assert!(RateLimitObservation::from_codex_token_count(&wrong_type, 0).is_none());

        let no_limits: serde_json::Value =
            serde_json::from_str(r#"{"type":"token_count"}"#).expect("json");
        assert!(RateLimitObservation::from_codex_token_count(&no_limits, 0).is_none());

        let empty_limits: serde_json::Value =
            serde_json::from_str(r#"{"type":"token_count","rate_limits":{}}"#).expect("json");
        assert!(RateLimitObservation::from_codex_token_count(&empty_limits, 0).is_none());
    }
}
