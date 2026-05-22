//! Model: provider auth, request submission, streaming transport, response
//! collection/parsing, usage extraction, and model-name handling.

use anyhow::Result;
use futures_util::Stream;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent_loop::AgentEvent;

pub use auth::{AuthConfig, ResolvedProvider, load_auth, resolve_provider};
pub use chat_completions::ChatCompletionsTransport;
pub use names::did_you_mean;
pub use responses::types::ResponseEnvelope;
pub use responses::{
    OpenAiTransport, ResponsesError, RetryPolicy, ToolCall, into_raw_output, process_response,
};

/// Wire shape a model backend expects for sampling requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    /// OpenAI Responses API: `instructions`, `input`, flat tool definitions.
    Responses,
    /// OpenAI-compatible Chat Completions: `messages`, nested tool functions.
    ChatCompletions,
}

impl WireFormat {
    pub fn label(self) -> &'static str {
        match self {
            WireFormat::Responses => "Responses",
            WireFormat::ChatCompletions => "Chat Completions",
        }
    }
}

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
///
/// Two concrete implementors ship today: [`OpenAiTransport`] for the OpenAI
/// Responses API (Codex/ChatGPT auth) and [`ChatCompletionsTransport`] for
/// OpenAI-compatible `/chat/completions` endpoints resolved through the
/// providers catalog ([`resolve_provider`]). The trait name predates the
/// second backend — keep it; renaming is a separate cleanup. Selection
/// happens at construction time in `nav-cli`/`nav-tui`: the agent loop only
/// sees a `&dyn ResponsesTransport`.
pub trait ResponsesTransport: Send + Sync {
    /// Wire format this transport expects.
    fn wire_format(&self) -> WireFormat {
        WireFormat::Responses
    }

    /// Resolved provider metadata for Chat Completions transports.
    ///
    /// Responses transports return `None`; the agent loop uses this only when
    /// it has to build a Chat Completions request body.
    fn chat_completions_provider(&self) -> Option<ResolvedProvider> {
        None
    }

    fn create<'a>(
        &'a self,
        body: Value,
        events: UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>>;
}

/// Shared model transport handle used by frontends that can swap backends
/// while the process stays alive.
#[derive(Clone)]
pub struct ModelTransportHandle {
    current: Arc<Mutex<Arc<dyn ResponsesTransport>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSwapOutcome {
    pub from: WireFormat,
    pub to: WireFormat,
}

impl ModelTransportHandle {
    pub fn new<T>(transport: T) -> Self
    where
        T: ResponsesTransport + 'static,
    {
        Self::from_arc(Arc::new(transport))
    }

    pub fn from_arc(transport: Arc<dyn ResponsesTransport>) -> Self {
        Self {
            current: Arc::new(Mutex::new(transport)),
        }
    }

    pub fn wire_format(&self) -> WireFormat {
        self.current_transport().wire_format()
    }

    pub fn swap_to<T>(&self, transport: T) -> Result<ModelSwapOutcome>
    where
        T: ResponsesTransport + 'static,
    {
        self.swap_arc(Arc::new(transport))
    }

    pub fn swap_arc(&self, next: Arc<dyn ResponsesTransport>) -> Result<ModelSwapOutcome> {
        let to = next.wire_format();
        let mut guard = self.current.lock().expect("model transport mutex poisoned");
        let from = guard.wire_format();
        if from == WireFormat::ChatCompletions && to == WireFormat::Responses {
            anyhow::bail!(
                "cannot switch from Chat Completions back to Codex/Responses in this session; \
                 reverse history conversion is not supported yet"
            );
        }
        *guard = next;
        Ok(ModelSwapOutcome { from, to })
    }

    fn current_transport(&self) -> Arc<dyn ResponsesTransport> {
        let guard = self.current.lock().expect("model transport mutex poisoned");
        Arc::clone(&*guard)
    }
}

impl ResponsesTransport for ModelTransportHandle {
    fn wire_format(&self) -> WireFormat {
        self.current_transport().wire_format()
    }

    fn chat_completions_provider(&self) -> Option<ResolvedProvider> {
        self.current_transport().chat_completions_provider()
    }

    fn create<'a>(
        &'a self,
        body: Value,
        events: UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>> {
        let transport = self.current_transport();
        Box::pin(async move { transport.create(body, events).await })
    }
}

pub mod auth;

pub mod chat_completions;

pub mod names;

pub mod resolve_value;

pub mod responses;
