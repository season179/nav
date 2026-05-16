use crate::auth::AuthConfig;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::str;
use tokio::sync::mpsc::UnboundedSender;

pub(super) async fn stream_sse(
    client: &reqwest::Client,
    auth: &AuthConfig,
    mut body: Value,
    tx: UnboundedSender<Result<Value>>,
) {
    // SSE is the older path. It remains useful because it is plain HTTP
    // and shows why the WebSocket transport is a latency optimization.
    body["stream"] = json!(true);

    if let Err(err) = drive_sse(client, auth, body, &tx).await {
        let _ = tx.send(Err(err));
    }
}

async fn drive_sse(
    client: &reqwest::Client,
    auth: &AuthConfig,
    body: Value,
    tx: &UnboundedSender<Result<Value>>,
) -> Result<()> {
    let response = client
        .post(format!("{}/responses", auth.http_base_url))
        .json(&body)
        .send()
        .await
        .context("Responses API request failed")?;

    let status = response.status();
    if !status.is_success() {
        let error_body = response.text().await.unwrap_or_default();
        bail!("Responses API returned {status}: {error_body}");
    }

    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();

    // SSE arrives as arbitrary byte chunks, not neat JSON objects. We keep
    // a byte buffer so UTF-8 split across chunks is decoded only after a full frame arrives.
    while let Some(chunk) = stream.next().await {
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
    Ok(())
}

fn frame_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}
