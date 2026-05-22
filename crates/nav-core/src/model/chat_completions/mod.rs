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
//! Today only the Codex path is reachable from `nav-cli`; G9 will rewrite
//! `load_auth`'s api-key branch to consult the catalog and instantiate this
//! transport. F1 only puts the module in place — request body construction
//! (C1), response parsing (C2), and SSE event normalization (F2) follow.

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
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent_loop::AgentEvent;
use crate::model::auth::ResolvedProvider;
use crate::model::responses::RetryPolicy;
use crate::model::{EventStream, ResponsesTransport};

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
}

impl ResponsesTransport for ChatCompletionsTransport {
    fn create<'a>(
        &'a self,
        _body: Value,
        _events: UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>> {
        // The body is built by `request::build_request_body` (C1), driven by
        // `sse::connect_sse` + `sse::drive_sse` (F2), normalized through
        // `delta::normalize_event` (F2), and folded into a `ResponseEnvelope`
        // by `collector::ChatCompletionsCollector` (C2). Until those land
        // this transport is unreachable from production code — selection in
        // `nav-cli` still goes through `OpenAiTransport` only.
        Box::pin(async {
            unimplemented!(
                "ChatCompletionsTransport::create — request/parser/sse/collector are filled in by C1/C2/F2"
            )
        })
    }
}

#[cfg(test)]
mod tests;
