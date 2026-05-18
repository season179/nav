use super::retry::{TransportError, parse_retry_after_seconds};
use super::{ResponsesError, detect_context_overflow, detect_http_overflow};
use crate::auth::AuthConfig;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::str;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::timeout;

/// Issue the streaming `POST /responses` request and verify a 2xx response.
///
/// Returns a [`TransportError`] so the retry wrapper can decide whether to
/// re-try (429 / 5xx / network / timeout) or surface the failure.
pub(super) async fn connect_sse(
    client: &reqwest::Client,
    auth: &AuthConfig,
    body: Value,
) -> Result<reqwest::Response, TransportError> {
    let mut body = body;
    body["stream"] = json!(true);

    let response = client
        .post(format!("{}/responses", auth.http_base_url))
        .json(&body)
        .send()
        .await?;

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

/// Read the SSE stream and forward decoded events onto `tx`. Aborts with
/// `ResponsesError::ContextWindowExceeded` when the server signals
/// `context_length_exceeded`; any other failure surfaces as `Other`.
///
/// `idle_timeout` bounds the wait between byte chunks — a stuck stream is
/// caught instead of hanging the agent indefinitely.
pub(super) async fn drive_sse(
    response: reqwest::Response,
    idle_timeout: Duration,
    tx: UnboundedSender<Result<Value, ResponsesError>>,
) {
    if let Err(err) = drive_sse_inner(response, idle_timeout, &tx).await {
        let _ = tx.send(Err(ResponsesError::Other(err)));
    }
}

async fn drive_sse_inner(
    response: reqwest::Response,
    idle_timeout: Duration,
    tx: &UnboundedSender<Result<Value, ResponsesError>>,
) -> Result<()> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();

    loop {
        let next = match timeout(idle_timeout, stream.next()).await {
            Ok(item) => item,
            Err(_) => {
                bail!(
                    "idle timeout: no SSE event for {}s",
                    idle_timeout.as_secs()
                );
            }
        };
        let Some(chunk) = next else {
            return Ok(());
        };
        let chunk = chunk.context("failed to read Responses API stream")?;
        buffer.extend_from_slice(&chunk);

        while let Some(index) = frame_boundary(&buffer) {
            let frame = buffer.drain(..index).collect::<Vec<_>>();
            buffer.drain(..2);
            let frame = str::from_utf8(&frame).context("failed to decode SSE frame as UTF-8")?;

            for line in frame.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }

                let event: Value =
                    serde_json::from_str(data).context("failed to decode SSE event")?;

                if let Some(message) = detect_context_overflow(&event) {
                    let _ = tx.send(Err(ResponsesError::ContextWindowExceeded { message }));
                    return Ok(());
                }

                let is_terminal = matches!(
                    event.get("type").and_then(Value::as_str),
                    Some("response.completed") | Some("error")
                );
                if tx.send(Ok(event)).is_err() {
                    return Ok(());
                }
                if is_terminal {
                    return Ok(());
                }
            }
        }
    }
}

fn frame_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}
