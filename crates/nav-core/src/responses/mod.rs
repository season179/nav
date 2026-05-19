mod collector;
mod parser;
mod request;
mod retry;
mod sse;
pub mod types;
mod websocket;

use crate::agent::{AgentEvent, ResponsesTransport};
use crate::{auth::AuthConfig, cli::Transport};
use anyhow::{Result, anyhow};
use futures_util::{Stream, stream};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};

pub(crate) use collector::ResponseCollector;
pub use parser::{ToolCall, into_raw_output, process_response};
pub(crate) use parser::{assistant_text, function_calls_from, turn_usage_from};
pub(crate) use request::response_body;
pub use retry::RetryPolicy;

/// Errors yielded by a `ResponsesTransport` stream.
///
/// `ContextWindowExceeded` is broken out from generic transport failures so the
/// agent loop can recover (drop the oldest tool pair and retry) instead of
/// aborting the turn. Anything else becomes `Other` and surfaces as an
/// `AgentEvent::Error`.
#[derive(Debug)]
pub enum ResponsesError {
    ContextWindowExceeded { message: String },
    Other(anyhow::Error),
}

impl std::fmt::Display for ResponsesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResponsesError::ContextWindowExceeded { message } => {
                write!(f, "context window exceeded: {message}")
            }
            ResponsesError::Other(err) => write!(f, "{err:#}"),
        }
    }
}

impl From<anyhow::Error> for ResponsesError {
    fn from(err: anyhow::Error) -> Self {
        ResponsesError::Other(err)
    }
}

impl From<ResponsesError> for anyhow::Error {
    fn from(err: ResponsesError) -> Self {
        match err {
            ResponsesError::ContextWindowExceeded { message } => {
                anyhow!("context window exceeded: {message}")
            }
            ResponsesError::Other(inner) => inner,
        }
    }
}

/// Returns the error message if the event represents a context-window overflow.
///
/// Both shapes seen on the Responses API:
/// - `{"type": "error", "code": "context_length_exceeded", "message": "..."}`
/// - `{"type": "response.failed", "response": {"error": {"code": "...", "message": "..."}}}`
pub(crate) fn detect_context_overflow(event: &Value) -> Option<String> {
    let event_type = event.get("type").and_then(Value::as_str)?;
    let (code, message) = match event_type {
        "error" => {
            let code = event.get("code").and_then(Value::as_str);
            let message = event.get("message").and_then(Value::as_str);
            (code, message)
        }
        "response.failed" => {
            let err = event.get("response").and_then(|r| r.get("error"))?;
            let code = err.get("code").and_then(Value::as_str);
            let message = err.get("message").and_then(Value::as_str);
            (code, message)
        }
        _ => return None,
    };
    if is_overflow_code(code) {
        Some(message.unwrap_or_default().to_string())
    } else {
        None
    }
}

/// Returns the error message if an HTTP error response body indicates a
/// context-window overflow. Used by the SSE and WebSocket connect paths so a
/// 400 / handshake rejection routes through the same recovery as stream-time
/// overflows. Expected body shape:
/// `{"error": {"code": "context_length_exceeded", "message": "..."}}`.
pub(crate) fn detect_http_overflow(body: &str) -> Option<String> {
    let json: Value = serde_json::from_str(body).ok()?;
    let err = json.get("error")?;
    let code = err.get("code").and_then(Value::as_str);
    if is_overflow_code(code) {
        Some(
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    } else {
        None
    }
}

fn is_overflow_code(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("context_length_exceeded") | Some("context_window_exceeded")
    )
}

/// Real `Responses` transport backed by the existing WebSocket and SSE code.
///
/// The agent loop holds this as a `dyn ResponsesTransport` so a stub transport
/// can be swapped in for tests without touching the network.
pub struct OpenAiTransport {
    client: reqwest::Client,
    auth: Arc<AuthConfig>,
    transport: Transport,
    idle_timeout: Duration,
    retry_policy: RetryPolicy,
}

impl OpenAiTransport {
    /// Construct with sensible defaults: 60s SSE/WS idle timeout and a 3-attempt
    /// exponential-backoff retry policy.
    pub fn new(client: reqwest::Client, auth: AuthConfig, transport: Transport) -> Self {
        Self::with_config(
            client,
            auth,
            transport,
            Duration::from_secs(60),
            RetryPolicy::default(),
        )
    }

    pub fn with_config(
        client: reqwest::Client,
        auth: AuthConfig,
        transport: Transport,
        idle_timeout: Duration,
        retry_policy: RetryPolicy,
    ) -> Self {
        Self {
            client,
            auth: Arc::new(auth),
            transport,
            idle_timeout,
            retry_policy,
        }
    }
}

impl ResponsesTransport for OpenAiTransport {
    fn create<'a>(
        &'a self,
        body: Value,
        events: UnboundedSender<AgentEvent>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<
                        Pin<Box<dyn Stream<Item = Result<Value, ResponsesError>> + Send>>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        let client = self.client.clone();
        let auth = self.auth.clone();
        let transport = self.transport;
        let idle_timeout = self.idle_timeout;
        let policy = self.retry_policy;
        Box::pin(async move {
            let (tx, rx) = mpsc::unbounded_channel::<Result<Value, ResponsesError>>();

            // Retry covers *only* connect: once events start flowing, retrying
            // would duplicate text already emitted to the user / session log.
            // The body is cloned per attempt because connect_* consumes it.
            let max_attempts = policy.max_attempts;
            let on_retry =
                |attempt: u32, delay: std::time::Duration, err: &retry::TransportError| {
                    let _ = events.send(AgentEvent::ProviderRetry {
                        attempt,
                        max_attempts,
                        delay_ms: delay.as_millis() as u64,
                        reason: err.to_string(),
                    });
                };

            match transport {
                Transport::Websocket => {
                    let result = retry::retry(&policy, on_retry, || async {
                        websocket::connect_ws(auth.as_ref(), body.clone()).await
                    })
                    .await;
                    match result {
                        Ok(socket) => {
                            tokio::spawn(async move {
                                websocket::drive_ws(socket, idle_timeout, tx).await;
                            });
                        }
                        Err(retry::TransportError::ContextWindowExceeded { message }) => {
                            // Surface as a stream-level error so run_agent's
                            // one-shot recovery handles connect-time and
                            // stream-time overflows the same way.
                            let _ = tx.send(Err(ResponsesError::ContextWindowExceeded { message }));
                        }
                        Err(err) => return Err(err.into()),
                    }
                }
                Transport::Sse => {
                    let result = retry::retry(&policy, on_retry, || async {
                        sse::connect_sse(&client, auth.as_ref(), body.clone()).await
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
                }
            }

            let stream = stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            });
            let boxed: Pin<Box<dyn Stream<Item = Result<Value, ResponsesError>> + Send>> =
                Box::pin(stream);
            Ok(boxed)
        })
    }
}

#[cfg(test)]
mod tests;
