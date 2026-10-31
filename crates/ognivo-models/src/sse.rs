//! Server-sent events as llama-server writes them: `data:` lines, `:` keep-alive comments
//! (sent after 30 s of silence, e.g. during a long prefill), and a final `data: [DONE]`
//! that only `/v1/chat/completions` sends, and only when the stream ends normally.

use futures::stream::{BoxStream, StreamExt};
use serde_json::Value;

use crate::engine::EngineError;

#[derive(Debug, Clone, PartialEq)]
pub enum SseEvent {
    Data(Value),
    /// `data: [DONE]`: the stream ended normally.
    Done,
}

/// Turns a response body into SSE events. A body that ends inside a line, a transport
/// error or a payload that is not JSON ends the stream with an error. Nothing is read
/// after `[DONE]`.
pub(crate) fn events(resp: reqwest::Response) -> BoxStream<'static, Result<SseEvent, EngineError>> {
    let bytes = resp.bytes_stream().map(|r| r.map(|b| b.to_vec())).boxed();
    futures::stream::unfold(
        (bytes, Vec::new(), false),
        |(mut bytes, mut buf, finished)| async move {
            if finished {
                return None;
            }
            loop {
                match next_payload(&mut buf) {
                    Some(Payload::Done) => return Some((Ok(SseEvent::Done), (bytes, buf, true))),
                    Some(Payload::Data(data)) => {
                        return match serde_json::from_slice::<Value>(&data) {
                            Ok(v) => Some((Ok(SseEvent::Data(v)), (bytes, buf, false))),
                            Err(e) => {
                                let err = EngineError::Protocol(format!("bad SSE payload: {e}"));
                                Some((Err(err), (bytes, buf, true)))
                            }
                        };
                    }
                    None => match bytes.next().await {
                        Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                        Some(Err(e)) => {
                            let err = EngineError::Http(e.to_string());
                            return Some((Err(err), (bytes, buf, true)));
                        }
                        None if buf.trim_ascii().is_empty() => return None,
                        None => {
                            let err =
                                EngineError::Protocol("stream ended inside an SSE line".into());
                            return Some((Err(err), (bytes, buf, true)));
                        }
                    },
                }
            }
        },
    )
    .boxed()
}

#[derive(Debug, PartialEq, Eq)]
enum Payload {
    Data(Vec<u8>),
    Done,
}

/// Takes the next complete `data:` line out of `buf`, skipping other SSE lines.
fn next_payload(buf: &mut Vec<u8>) -> Option<Payload> {
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=pos).collect();
        let Some(data) = line.trim_ascii().strip_prefix(b"data:") else {
            continue;
        };
        let data = data.trim_ascii();
        if data == b"[DONE]" {
            return Some(Payload::Done);
        }
        if !data.is_empty() {
            return Some(Payload::Data(data.to_vec()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_lines_comments_and_done() {
        let mut buf = b": ping\n\nevent: x\ndata: {\"a\":1}\n\ndata: {\"b\"".to_vec();
        assert_eq!(
            next_payload(&mut buf),
            Some(Payload::Data(b"{\"a\":1}".to_vec()))
        );
        assert_eq!(next_payload(&mut buf), None); // incomplete line stays buffered
        buf.extend_from_slice(b":2}\r\ndata: [DONE]\n");
        assert_eq!(
            next_payload(&mut buf),
            Some(Payload::Data(b"{\"b\":2}".to_vec()))
        );
        assert_eq!(next_payload(&mut buf), Some(Payload::Done));
    }
}
