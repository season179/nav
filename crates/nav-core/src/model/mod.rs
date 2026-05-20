//! Model: provider auth, request submission, streaming transport, response
//! collection/parsing, usage extraction, and model-name handling.

use anyhow::Result;
use futures_util::Stream;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::AgentEvent;

pub use auth::{AuthConfig, load_auth};
pub use names::did_you_mean;
pub use responses::types::ResponseEnvelope;
pub use responses::{
    OpenAiTransport, ResponsesError, RetryPolicy, ToolCall, into_raw_output, process_response,
};

/// Stream of raw provider events yielded by a model transport.
///
/// `ResponsesError::ContextWindowExceeded` is the only error variant the agent
/// loop recovers from; everything else is wrapped in `Other` and surfaces as
/// an `AgentEvent::Error`.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Value, ResponsesError>> + Send>>;

/// Abstraction over model request submission so the agent loop can be driven
/// by either the real provider client or a test stub.
///
/// `events` lets the transport surface durable provider events onto the same
/// channel the rest of the agent loop uses, without forcing the transport to
/// know about session persistence.
pub trait ResponsesTransport: Send + Sync {
    fn create<'a>(
        &'a self,
        body: Value,
        events: UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>>;
}

pub mod auth;

pub mod names;

pub mod responses;
