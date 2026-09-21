//! LLM source（`claude-oauth` / `codex-oauth` / `openai-compatible`）の実装（ADR-0053 D1）。
//!
//! どの source も同じ形の結果を返す: 非 stream は 1 つの応答、stream は `ChatCompletionChunk` の列。
//! エラーは分類済み（`SourceError`）で、`server`/`selection` が cooldown・やり直しを決める
//! （source 自身は再試行の方針を持たない。DESIGN 原則の「判断は 1 か所」をこのクレート内でも守る）。

pub mod claude;
pub mod codex;
pub mod relay;

use futures_util::stream::BoxStream;

use crate::openai::{ChatCompletionChunk, ChatCompletionResponse};

/// upstream が返した／通信に失敗した結果の分類。**メッセージに秘密の値を含めない**。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// 401（または OAuth の再認証が必要）。呼び出し側が cooldown（`AuthFailed`）にする。
    #[error("upstream rejected the credentials (401)")]
    Unauthorized,
    /// 429。`retry_after` があれば秒数。
    #[error("upstream rate-limited the request (429)")]
    RateLimited { retry_after: Option<u64> },
    /// その他の HTTP エラー応答（本文は要約のみ。値は含めない）。
    #[error("upstream returned HTTP {status}: {summary}")]
    Upstream { status: u16, summary: String },
    /// 接続できない・タイムアウト等（bytes はまだ送っていない）。
    #[error("network error: {0}")]
    Network(String),
    /// 資格情報ファイルが読めない・壊れている。
    #[error("credentials error: {0}")]
    Credentials(String),
    /// この source は無効（設定に無い／`enabled = false`）。
    #[error("source unavailable: {0}")]
    Unavailable(String),
}

impl SourceError {
    /// stream 開始前に起きたエラーは、他の候補へやり直してよい（ADR-0053 D1）。
    /// （このクレートの `send`/`send_stream` は、実際に bytes を送り始める前にしか `Err` を返さない
    /// 契約なので、常に true。呼び出し側の理解を明文化するためのメソッド。）
    pub fn retryable_before_any_bytes(&self) -> bool {
        true
    }
}

pub enum SendOutcome {
    NonStream(ChatCompletionResponse),
    Stream(BoxStream<'static, Result<ChatCompletionChunk, SourceError>>),
}
