//! Chat Completions wire-format transport.
//!
//! Sibling to [`crate::model::responses`]. Both modules export a concrete
//! [`ResponsesTransport`] implementation so the agent loop can drive either
//! the OpenAI Responses API or any OpenAI-compatible Chat Completions
//! endpoint (Z.AI, Groq, Together, local Ollama, etc.) without changes to
//! `agent_loop/runner.rs`.
//!
//! Selection happens at construction time:
//!
//! - Codex/ChatGPT auth (the legacy [`crate::model::auth::AuthMode::Chatgpt`]
//!   flow) ⇒ [`crate::model::responses::OpenAiTransport`].
//! - A `ResolvedProvider` produced by
//!   [`crate::model::auth::resolve_provider`] from the providers catalog ⇒
//!   [`ChatCompletionsTransport`].
//!
//! Request body construction, SSE normalization, and response parsing all
//! normalize into the shared Responses-shaped event/envelope path so the
//! agent loop can stay backend-agnostic after transport selection.

mod collector;
mod delta;
mod history;
mod parser;
mod request;
mod sse;
mod types;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use futures_util::stream;
use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::agent_loop::AgentEvent;
use crate::model::auth::ResolvedProvider;
use crate::model::responses::RetryPolicy;
use crate::model::responses::{ResponsesError, retry};
use crate::model::{EventStream, ResponsesTransport, WireFormat};

pub(crate) use parser::{
    assistant_text, into_raw_output, process_response, sanitize_continuation_items, turn_usage_from,
};
pub(crate) use request::build_request_body_with_options;

/// Transport for OpenAI-compatible `POST {base_url}/chat/completions` SSE
/// endpoints, parameterized by a resolved provider/model entry from the
/// catalog.
///
/// The Chat Completions API does not expose a WebSocket variant of the
/// stream, so this transport is SSE-only. No `Transport` enum is taken at
/// construction — that flag is meaningful for [`crate::model::responses`]
/// only.
#[allow(dead_code)]
pub struct ChatCompletionsTransport {
    client: reqwest::Client,
    resolved: ResolvedProvider,
    idle_timeout: Duration,
    retry_policy: RetryPolicy,
}

impl ChatCompletionsTransport {
    /// Construct with the resolved provider/model entry, an idle timeout for
    /// the SSE stream, and a connect-time retry policy. The retry policy
    /// behaves like the Responses backend's: it covers only the connect, not
    /// mid-stream errors (retrying mid-stream would duplicate text already
    /// emitted to the user).
    pub fn new(
        client: reqwest::Client,
        resolved: ResolvedProvider,
        idle_timeout: Duration,
        retry_policy: RetryPolicy,
    ) -> Self {
        Self {
            client,
            resolved,
            idle_timeout,
            retry_policy,
        }
    }

    /// Construct using nav's normal streaming-client timeouts.
    pub fn with_default_client(
        resolved: ResolvedProvider,
        idle_timeout: Duration,
        retry_policy: RetryPolicy,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self::new(client, resolved, idle_timeout, retry_policy))
    }
}

impl ResponsesTransport for ChatCompletionsTransport {
    fn wire_format(&self) -> WireFormat {
        WireFormat::ChatCompletions
    }

    fn chat_completions_provider(&self) -> Option<ResolvedProvider> {
        Some(self.resolved.clone())
    }

    fn create<'a>(
        &'a self,
        body: Value,
        events: UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>> {
        let client = self.client.clone();
        let resolved = self.resolved.clone();
        let idle_timeout = self.idle_timeout;
        let policy = self.retry_policy;
        Box::pin(async move {
            let (tx, rx) = mpsc::unbounded_channel::<Result<Value, ResponsesError>>();
            let max_attempts = policy.max_attempts;
            let on_retry = |attempt: u32, delay: Duration, err: &retry::TransportError| {
                let _ = events.send(AgentEvent::ProviderRetry {
                    attempt,
                    max_attempts,
                    delay_ms: delay.as_millis() as u64,
                    reason: err.to_string(),
                });
            };

            let result = retry::retry(&policy, on_retry, || async {
                sse::connect_sse(&client, &resolved, body.clone()).await
            })
            .await;
            match result {
                Ok(response) => {
                    tokio::spawn(async move {
                        sse::drive_sse(response, idle_timeout, tx).await;
                    });
                }
                Err(retry::TransportError::ContextWindowExceeded { message }) => {
                    let _ = tx.send(Err(ResponsesError::ContextWindowExceeded { message }));
                }
                Err(err) => return Err(err.into()),
            }

            let stream = stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            });
            let boxed: EventStream = Box::pin(stream);
            Ok(boxed)
        })
    }
}

#[cfg(test)]
mod tests;
