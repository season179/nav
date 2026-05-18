mod collector;
mod parser;
mod request;
mod sse;
pub mod types;
mod websocket;

use crate::agent::ResponsesTransport;
use crate::{auth::AuthConfig, cli::Transport};
use anyhow::Result;
use futures_util::{Stream, stream};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(crate) use collector::ResponseCollector;
pub use parser::{ToolCall, into_raw_output, process_response};
pub(crate) use parser::{function_calls_from, turn_usage_from};
pub(crate) use request::response_body;

/// Real `Responses` transport backed by the existing WebSocket and SSE code.
///
/// The agent loop holds this as a `dyn ResponsesTransport` so a stub transport
/// can be swapped in for tests without touching the network.
pub struct OpenAiTransport {
    client: reqwest::Client,
    auth: Arc<AuthConfig>,
    transport: Transport,
}

impl OpenAiTransport {
    pub fn new(client: reqwest::Client, auth: AuthConfig, transport: Transport) -> Self {
        Self {
            client,
            auth: Arc::new(auth),
            transport,
        }
    }
}

impl ResponsesTransport for OpenAiTransport {
    fn create<'a>(
        &'a self,
        body: Value,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Pin<Box<dyn Stream<Item = Result<Value>> + Send>>>>
                + Send
                + 'a,
        >,
    > {
        let client = self.client.clone();
        let auth = self.auth.clone();
        let transport = self.transport;
        Box::pin(async move {
            let (tx, rx) = mpsc::unbounded_channel::<Result<Value>>();
            tokio::spawn(async move {
                match transport {
                    Transport::Websocket => {
                        websocket::stream_websocket(auth.as_ref(), body, tx).await;
                    }
                    Transport::Sse => {
                        sse::stream_sse(&client, auth.as_ref(), body, tx).await;
                    }
                }
            });
            let stream = stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            });
            let boxed: Pin<Box<dyn Stream<Item = Result<Value>> + Send>> = Box::pin(stream);
            Ok(boxed)
        })
    }
}

#[cfg(test)]
mod tests;
