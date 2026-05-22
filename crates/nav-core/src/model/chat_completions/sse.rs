//! SSE connect + drive for the Chat Completions API.
//!
//! Chat Completions does not expose a WebSocket equivalent of the Responses
//! API, so this backend is SSE-only — no websocket sibling. Filled in by F2
//! together with [`super::delta::normalize_event`].

use super::super::responses::{ResponsesError, RetryPolicy};
use crate::model::auth::ResolvedProvider;
use anyhow::Result;
use reqwest::Response;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

/// Issue the streaming `POST {base_url}/chat/completions` request and verify
/// a 2xx response. Counterpart to
/// [`crate::model::responses::sse::connect_sse`], but reads
/// `base_url`/`bearer`/`headers` from a [`ResolvedProvider`] instead of an
/// [`crate::model::auth::AuthConfig`].
///
/// Stub: filled in by F2.
#[allow(dead_code)]
pub(super) async fn connect_sse(
    _client: &reqwest::Client,
    _resolved: &ResolvedProvider,
    _body: Value,
    _retry_policy: RetryPolicy,
) -> Result<Response> {
    unimplemented!("Chat Completions SSE connect lands in F2")
}

/// Read the SSE stream, normalize each `chat.completion.chunk` frame through
/// [`super::delta::normalize_event`], and forward the result onto `tx` so the
/// shared collector can consume both backends.
///
/// Stub: filled in by F2.
#[allow(dead_code)]
pub(super) async fn drive_sse(
    _response: Response,
    _idle_timeout: Duration,
    _tx: UnboundedSender<Result<Value, ResponsesError>>,
) {
    unimplemented!("Chat Completions SSE drive lands in F2")
}
