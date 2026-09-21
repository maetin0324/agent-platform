//! SSE の最小デコーダ（上流の Anthropic / Codex Responses の `event:`/`data:` 行を読む側）。
//! 完全なイベント（`\n\n` で終わる）だけを取り出すので、複数バイト文字が chunk 境界で割れても壊れない
//! （完全な境界が来るまでバイト列のまま溜める）。

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 受け取った bytes を溜め、完成したイベントを 0 件以上返す。
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = find_double_newline(&self.buf) {
            let (raw, rest_start) = split_event(&self.buf, pos);
            let rest = self.buf[rest_start..].to_vec();
            if let Some(event) = parse_event(&raw) {
                out.push(event);
            }
            self.buf = rest;
        }
        out
    }
}

/// `\n\n` または `\r\n\r\n` の位置（区切りの先頭）を返す。
fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn split_event(buf: &[u8], pos: usize) -> (Vec<u8>, usize) {
    (buf[..pos].to_vec(), pos + 2)
}

fn parse_event(raw: &[u8]) -> Option<SseEvent> {
    let text = String::from_utf8_lossy(raw);
    let mut event = None;
    let mut data_lines = Vec::new();
    for line in text.split(['\n']) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(v) = line.strip_prefix("event:") {
            event = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("data:") {
            data_lines.push(v.trim_start().to_string());
        }
    }
    if data_lines.is_empty() && event.is_none() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
    })
}

/// 出て行く側（OpenAI 互換）の SSE 1 行を作る。
pub fn encode_data(json: &str) -> String {
    format!("data: {json}\n\n")
}

pub const DONE: &str = "data: [DONE]\n\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_events_split_across_pushes() {
        let mut dec = SseDecoder::new();
        let mut out = dec.push(b"event: message_start\ndata: {\"a\":1");
        assert!(out.is_empty());
        out = dec.push(b"}\n\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event.as_deref(), Some("message_start"));
        assert_eq!(out[0].data, "{\"a\":1}");
    }

    #[test]
    fn decodes_multiple_events_in_one_push() {
        let mut dec = SseDecoder::new();
        let out = dec.push(b"event: a\ndata: 1\n\nevent: b\ndata: 2\n\n");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].event.as_deref(), Some("a"));
        assert_eq!(out[1].event.as_deref(), Some("b"));
    }

    #[test]
    fn multiline_data_is_joined_with_newlines() {
        let mut dec = SseDecoder::new();
        let out = dec.push(b"data: line1\ndata: line2\n\n");
        assert_eq!(out[0].data, "line1\nline2");
    }
}
