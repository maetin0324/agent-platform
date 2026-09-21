//! `openai-compatible`（ADR-0053 D1-3）: 既存の OpenAI 互換エンドポイント（Qwen 等）へそのまま中継する。
//!
//! 供給元固有の写しは要らない。要求 JSON をそのまま転送し、応答（非 stream の JSON、または SSE の
//! bytes）もそのまま返す。`server` 側が上流の生の応答を素通しできるよう、ここは「送る」ことだけをする。

use crate::config::OpenAiCompatibleConfig;
use crate::neterr::safe_reqwest_error;

use super::SourceError;

/// `GET <base_url>/models` が 200 を返すかだけを見る（到達性 probe。ADR-0053 D1）。
pub async fn probe(client: &reqwest::Client, cfg: &OpenAiCompatibleConfig) -> bool {
    let url = format!("{}/models", cfg.base_url.trim_end_matches('/'));
    let mut builder = client.get(&url).timeout(std::time::Duration::from_secs(3));
    if let Some(key) = &cfg.api_key {
        builder = builder.bearer_auth(key);
    }
    matches!(builder.send().await, Ok(resp) if resp.status().is_success())
}

/// upstream の生の応答（ステータス・ヘッダのうち `content-type`/`retry-after`・本体）。
pub struct RawResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub retry_after: Option<u64>,
    pub body: reqwest::Response,
}

/// `POST <base_url>/chat/completions` へそのまま転送する（本文は呼び出し側が組んだ生の JSON）。
pub async fn send_raw(
    client: &reqwest::Client,
    cfg: &OpenAiCompatibleConfig,
    body: &serde_json::Value,
) -> Result<RawResponse, SourceError> {
    if !cfg.enabled {
        return Err(SourceError::Unavailable(cfg.id.clone()));
    }
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut builder = client.post(&url).json(body);
    if let Some(key) = &cfg.api_key {
        builder = builder.bearer_auth(key);
    }
    let resp = builder
        .send()
        .await
        .map_err(|e| SourceError::Network(safe_reqwest_error(&e)))?;
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    Ok(RawResponse {
        status,
        content_type,
        retry_after,
        body: resp,
    })
}
