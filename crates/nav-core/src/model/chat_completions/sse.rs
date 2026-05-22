//! SSE connect + drive for the Chat Completions API.
//!
//! Chat Completions does not expose a WebSocket equivalent of the Responses
//! API, so this backend is SSE-only — no websocket sibling. The driver mirrors
//! [`crate::model::responses::sse`] frame-by-frame and then routes each
//! parsed `chat.completion.chunk` through
//! [`super::delta::ChatCompletionsAccumulator`] so the rest of the agent loop
//! sees the same Responses-shape event stream both backends share.

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use reqwest::Response;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{Value, json};
use std::str;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::timeout;

use super::delta::ChatCompletionsAccumulator;
use crate::model::auth::ResolvedProvider;
use crate::model::responses::ResponsesError;
use crate::model::responses::detect_http_overflow;
use crate::model::responses::retry::{TransportError, parse_retry_after_seconds};

/// Issue the streaming `POST {base_url}/chat/completions` request and verify
/// a 2xx response. Counterpart to
/// [`crate::model::responses::sse::connect_sse`], but reads
/// `base_url`/`bearer`/`headers` from a [`ResolvedProvider`] instead of an
/// [`crate::model::auth::AuthConfig`].
///
/// `stream: true` is asserted on the request body here so a caller that
/// forgets to set it still gets an SSE response back. Auth attaches per
/// request because each provider entry has its own bearer / header set —
/// the underlying `reqwest::Client` is shared across providers.
#[allow(dead_code)]
pub(super) async fn connect_sse(
    client: &reqwest::Client,
    resolved: &ResolvedProvider,
    body: Value,
) -> Result<Response, TransportError> {
    let mut body = body;
    body["stream"] = json!(true);

    let url = format!(
        "{}/chat/completions",
        resolved.base_url.trim_end_matches('/')
    );

    let mut request = client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "text/event-stream");

    if let Some(bearer) = resolved.bearer.as_deref()
        && !bearer.is_empty()
    {
        request = request.header(AUTHORIZATION, format!("Bearer {bearer}"));
    }

    for (name, value) in &resolved.headers {
        request = request.header(name, value);
    }

    let response = request.json(&body).send().await?;

    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after_seconds);
    let body_text = response.text().await.unwrap_or_default();
    if let Some(message) = detect_http_overflow(&body_text) {
        return Err(TransportError::ContextWindowExceeded { message });
    }
    Err(TransportError::Http {
        status,
        retry_after,
        body: body_text,
    })
}

/// Read the SSE stream, fold each `chat.completion.chunk` through the
/// [`ChatCompletionsAccumulator`], and forward the normalized
/// Responses-shape events onto `tx` so the shared collector can consume
/// either backend.
///
/// `idle_timeout` bounds the wait between byte chunks; a stuck stream is
/// caught instead of hanging the agent indefinitely. `[DONE]` terminates the
/// stream cleanly; the accumulator's `finalize` then emits the closing
/// `response.completed` event.
#[allow(dead_code)]
pub(super) async fn drive_sse(
    response: Response,
    idle_timeout: Duration,
    tx: UnboundedSender<Result<Value, ResponsesError>>,
) {
    if let Err(err) = drive_sse_inner(response, idle_timeout, &tx).await {
        let _ = tx.send(Err(ResponsesError::Other(err)));
    }
}

async fn drive_sse_inner(
    response: Response,
    idle_timeout: Duration,
    tx: &UnboundedSender<Result<Value, ResponsesError>>,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut accumulator = ChatCompletionsAccumulator::new();

    'outer: loop {
        let next = match timeout(idle_timeout, stream.next()).await {
            Ok(item) => item,
            Err(_) => bail!("idle timeout: no SSE event for {}s", idle_timeout.as_secs()),
        };
        let Some(chunk) = next else {
            break;
        };
        let bytes = chunk.context("failed to read Chat Completions API stream")?;
        buffer.extend_from_slice(&bytes);

        while let Some(boundary) = frame_boundary(&buffer) {
            let frame = buffer.drain(..boundary.frame_end).collect::<Vec<_>>();
            buffer.drain(..boundary.separator_len);
            let frame = str::from_utf8(&frame).context("failed to decode SSE frame as UTF-8")?;

            for line in frame.split(['\n', '\r']) {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                if handle_data_payload(data, &mut accumulator, tx)? == StreamDecision::Done {
                    break 'outer;
                }
            }
        }
    }

    for event in accumulator.finalize() {
        if tx.send(Ok(event)).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamDecision {
    Continue,
    Done,
}

fn handle_data_payload(
    data: &str,
    accumulator: &mut ChatCompletionsAccumulator,
    tx: &UnboundedSender<Result<Value, ResponsesError>>,
) -> Result<StreamDecision> {
    let data = data.trim();
    if data.is_empty() {
        return Ok(StreamDecision::Continue);
    }
    if data == "[DONE]" {
        return Ok(StreamDecision::Done);
    }
    if !looks_like_json_payload(data) {
        return Ok(StreamDecision::Continue);
    }

    let parsed: Value = match serde_json::from_str(data) {
        Ok(parsed) => parsed,
        Err(err) => bail!("failed to decode SSE event: {err}"),
    };
    match accumulator.push_chunk(&parsed) {
        Ok(events) => {
            for event in events {
                let is_terminal = is_terminal_event(&event);
                if tx.send(Ok(event)).is_err() || is_terminal {
                    return Ok(StreamDecision::Done);
                }
            }
        }
        Err(err) => {
            let _ = tx.send(Err(err));
            return Ok(StreamDecision::Done);
        }
    }
    Ok(StreamDecision::Continue)
}

fn looks_like_json_payload(data: &str) -> bool {
    matches!(
        data.trim_start().as_bytes().first(),
        Some(b'{') | Some(b'[')
    )
}

fn is_terminal_event(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed") | Some("error")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameBoundary {
    frame_end: usize,
    separator_len: usize,
}

fn frame_boundary(buffer: &[u8]) -> Option<FrameBoundary> {
    for index in 0..buffer.len() {
        match buffer[index] {
            b'\n' if buffer.get(index + 1) == Some(&b'\n') => {
                return Some(FrameBoundary {
                    frame_end: index,
                    separator_len: 2,
                });
            }
            b'\r' if buffer.get(index + 1) == Some(&b'\r') => {
                return Some(FrameBoundary {
                    frame_end: index,
                    separator_len: 2,
                });
            }
            b'\r'
                if buffer.get(index + 1) == Some(&b'\n')
                    && buffer.get(index + 2) == Some(&b'\r')
                    && buffer.get(index + 3) == Some(&b'\n') =>
            {
                return Some(FrameBoundary {
                    frame_end: index,
                    separator_len: 4,
                });
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn event_type(event: &Value) -> &str {
        event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    #[test]
    fn frame_boundary_accepts_lf_crlf_and_cr_frames() {
        assert_eq!(
            frame_boundary(b"data: {}\n\nnext"),
            Some(FrameBoundary {
                frame_end: 8,
                separator_len: 2,
            })
        );
        assert_eq!(
            frame_boundary(b"data: {}\r\n\r\nnext"),
            Some(FrameBoundary {
                frame_end: 8,
                separator_len: 4,
            })
        );
        assert_eq!(
            frame_boundary(b"data: {}\r\rnext"),
            Some(FrameBoundary {
                frame_end: 8,
                separator_len: 2,
            })
        );
    }

    #[test]
    fn non_json_data_payloads_are_ignored_without_discarding_state() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut accumulator = ChatCompletionsAccumulator::new();

        assert_eq!(
            handle_data_payload(
                r#" {"choices":[{"index":0,"delta":{"content":"Hel"}}]} "#,
                &mut accumulator,
                &tx
            )
            .unwrap(),
            StreamDecision::Continue
        );
        assert_eq!(
            handle_data_payload("heartbeat", &mut accumulator, &tx).unwrap(),
            StreamDecision::Continue
        );
        assert_eq!(
            handle_data_payload(
                r#"{"choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
                &mut accumulator,
                &tx
            )
            .unwrap(),
            StreamDecision::Done
        );

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event.expect("stream should not error"));
        }
        let types: Vec<&str> = events.iter().map(event_type).collect();
        assert_eq!(
            types,
            vec![
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_item.done",
                "response.completed",
            ]
        );
    }
}
