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
                        let event = parse(&data);
                        let failed = event.is_err();
                        return Some((event, (bytes, buf, failed)));
                    }
                    None => match bytes.next().await {
                        Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                        Some(Err(e)) => {
                            let err = EngineError::Http(e.to_string());
                            return Some((Err(err), (bytes, buf, true)));
                        }
                        None if buf.trim_ascii().is_empty() => return None,
                        None => return Some((Err(cut_line()), (bytes, buf, true))),
                    },
                }
            }
        },
    )
    .boxed()
}

/// A whole response body as SSE events, the way [`events`] reads a live one: for replaying
/// recorded streams. The events come in order, an error last where the live stream would
/// end with one, and nothing after `[DONE]`.
pub(crate) fn decode(body: &[u8]) -> Vec<Result<SseEvent, EngineError>> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
        let (line, tail) = rest.split_at(pos + 1);
        rest = tail;
        let event = match payload(line) {
            None => continue,
            Some(Payload::Done) => Ok(SseEvent::Done),
            Some(Payload::Data(data)) => parse(&data),
        };
        let last = !matches!(event, Ok(SseEvent::Data(_)));
        out.push(event);
        if last {
            return out;
        }
    }
    if !rest.trim_ascii().is_empty() {
        out.push(Err(cut_line()));
    }
    out
}

fn parse(data: &[u8]) -> Result<SseEvent, EngineError> {
    serde_json::from_slice::<Value>(data)
        .map(SseEvent::Data)
        .map_err(|e| EngineError::Protocol(format!("bad SSE payload: {e}")))
}

fn cut_line() -> EngineError {
    EngineError::Protocol("stream ended inside an SSE line".into())
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
        if let Some(p) = payload(&line) {
            return Some(p);
        }
    }
    None
}

/// What one SSE line carries: a `data:` payload, `[DONE]`, or nothing (a comment, another
/// field, empty data).
fn payload(line: &[u8]) -> Option<Payload> {
    let data = line.trim_ascii().strip_prefix(b"data:")?.trim_ascii();
    if data == b"[DONE]" {
        Some(Payload::Done)
    } else if data.is_empty() {
        None
    } else {
        Some(Payload::Data(data.to_vec()))
    }
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

    #[test]
    fn a_whole_body_decodes_like_a_stream() {
        let body = b"data: {\"a\":1}\n\n: ping\ndata: [DONE]\n\ndata: {\"late\":1}\n";
        let events = decode(body);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].as_ref().unwrap(),
            &SseEvent::Data(serde_json::json!({"a": 1}))
        );
        assert_eq!(events[1].as_ref().unwrap(), &SseEvent::Done);
        // the events before a cut line or a bad payload come first, as from a live stream
        let cut = decode(b"data: {\"a\":1}\ndata: {\"b\"");
        assert!(
            matches!(cut.as_slice(), [Ok(SseEvent::Data(_)), Err(_)]),
            "{cut:?}"
        );
        let bad = decode(b"data: nope\ndata: {\"a\":1}\n");
        assert!(matches!(bad.as_slice(), [Err(_)]), "{bad:?}");
        assert!(decode(b"").is_empty());
    }
}
