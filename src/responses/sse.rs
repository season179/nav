use super::{ResponseCollector, types::ResponseEnvelope};
use crate::auth::AuthConfig;
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::str;

pub(super) async fn create_response_sse(
    client: &reqwest::Client,
    auth: &AuthConfig,
    mut body: Value,
) -> Result<ResponseEnvelope> {
    // SSE is the older path. It remains useful because it is plain HTTP
    // and shows why the WebSocket transport is a latency optimization.
    body["stream"] = json!(true);

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

    decode_sse_response(response).await
}

async fn decode_sse_response(response: reqwest::Response) -> Result<ResponseEnvelope> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut collector = ResponseCollector::default();

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
                if collector.push_event(&event, "Responses API stream")? {
                    return collector.finish("Responses API stream");
                }
            }
        }
    }

    collector.finish("Responses API stream")
}

fn frame_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}
